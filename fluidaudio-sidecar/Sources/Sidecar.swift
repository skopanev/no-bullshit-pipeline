import Foundation
import AVFoundation
import CoreML
import FluidAudio

struct SpeakerSegment: Codable {
    let speakerId: String
    let startTime: Double
    let endTime: Double
    let text: String
}

/// Raw per-segment diarization embedding (256-d WeSpeaker), emitted for offline
/// clustering experiments (spectral + eigenvalue-gap) via --emit-embeddings.
struct DiarEmbedding: Codable {
    let startTime: Double
    let endTime: Double
    let speakerId: String
    let embedding: [Float]
}

struct FluidAudioOutputJSON: Codable {
    let text: String
    let speakerCount: Int
    let model: String
    let segments: [SpeakerSegment]
    let diarEmbeddings: [DiarEmbedding]?
    /// v2 (senko-port) diarization computed in the same run — lets the app
    /// store diarization.json from a single ASR pass.
    let diarV2: DiarV2Output?
}

func writeError(_ message: String) -> Never {
    FileHandle.standardError.write(Data("Error: \(message)\n".utf8))
    exit(1)
}

/// Write progress update to stderr (parsed by Rust side)
func writeProgress(_ stage: String, _ percent: Int) {
    FileHandle.standardError.write(Data("PROGRESS:\(stage):\(percent)\n".utf8))
}

/// Check if FluidAudio models are already cached. With `requireDiarizer=false`
/// only the ASR model needs to be present (Quick Dictate path).
///
/// We verify each specific file because FluidAudio's `downloadRepo` is
/// all-or-nothing — one missing file triggers a full repo re-download
/// (~2.8 GB for parakeet-coreml).
func modelsAreCached(requireDiarizer: Bool = true) -> Bool {
    let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
    guard let modelsDir = appSupport?.appendingPathComponent("FluidAudio/Models") else { return false }

    // FluidAudio 0.14.5 caches into `parakeet-tdt-0.6b-v3/`; older releases
    // used `parakeet-tdt-0.6b-v3-coreml/`. Accept either.
    let asrRequired = [
        "Preprocessor.mlmodelc",
        "Encoder.mlmodelc",          // .int8 default → ModelNames.ASR.encoderFile
        "Decoder.mlmodelc",
        "JointDecisionv3.mlmodelc",  // v3 jointV3File — added in FluidAudio 0.14.5
    ]
    let asrOk = ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3-coreml"].contains { repo in
        let dir = modelsDir.appendingPathComponent(repo)
        return asrRequired.allSatisfy { name in
            FileManager.default.fileExists(atPath: dir.appendingPathComponent(name).path)
        }
    }
    if !asrOk { return false }
    if !requireDiarizer { return true }

    // OfflineDiarizerManager files (batch mode) — distinct from the online
    // DiarizerManager which uses pyannote_segmentation/wespeaker_v2.
    let diarRequired = [
        "Segmentation.mlmodelc",
        "Embedding.mlmodelc",
        "FBank.mlmodelc",
        "PldaRho.mlmodelc",
    ]
    return ["speaker-diarization", "speaker-diarization-coreml"].contains { repo in
        let dir = modelsDir.appendingPathComponent(repo)
        return diarRequired.allSatisfy { name in
            FileManager.default.fileExists(atPath: dir.appendingPathComponent(name).path)
        }
    }
}

/// Join sub-word tokens into text, respecting word boundaries.
func joinTokens(_ tokens: [String]) -> String {
    tokens.joined()
        .replacingOccurrences(of: "\u{2581}", with: " ")
        .trimmingCharacters(in: .whitespaces)
}

/// Split audio into windows no longer than ~targetSec, snapping each cut to the
/// quietest 20ms point within a +/- slack window so we don't slice through a
/// word (a mid-word seam worsens code-switch). Qwen3's KV cache caps the total
/// sequence at maxCacheSeqLen (512); audio tokenizes at ~13.5 tok/s, so long
/// audio MUST be chunked or the decoder overflows ("prompt length exceeds cache").
func chunkBySilence(_ samples: [Float], sampleRate: Int, targetSec: Double, slackSec: Double) -> [[Float]] {
    let target = Int(targetSec * Double(sampleRate))
    let slack = Int(slackSec * Double(sampleRate))
    guard samples.count > target + slack else { return [samples] }
    var chunks: [[Float]] = []
    var start = 0
    let win = max(1, sampleRate / 50) // 20ms
    while start < samples.count {
        let nominalEnd = start + target
        if nominalEnd >= samples.count {
            chunks.append(Array(samples[start...]))
            break
        }
        // Search [nominalEnd - slack, nominalEnd + slack] for the quietest window.
        let lo = max(start + win, nominalEnd - slack)
        let hi = min(samples.count - win, nominalEnd + slack)
        var bestCut = nominalEnd
        var bestEnergy = Float.greatestFiniteMagnitude
        var p = lo
        while p < hi {
            var e: Float = 0
            for i in p..<(p + win) { e += samples[i] * samples[i] }
            if e < bestEnergy { bestEnergy = e; bestCut = p + win / 2 }
            p += win
        }
        chunks.append(Array(samples[start..<bestCut]))
        start = bestCut
    }
    return chunks
}

/// Pragmatic Cyrillic->Latin phonetic transliteration (for fuzzy matching only,
/// not display). Makes "папилайн"->"papilayn" so a char-level distance to the
/// English canonical "pipeline" becomes meaningful (≈0.6) instead of ~0.
func translitRuToLat(_ s: String) -> String {
    let map: [Character: String] = [
        "а": "a", "б": "b", "в": "v", "г": "g", "д": "d", "е": "e", "ё": "yo",
        "ж": "zh", "з": "z", "и": "i", "й": "y", "к": "k", "л": "l", "м": "m",
        "н": "n", "о": "o", "п": "p", "р": "r", "с": "s", "т": "t", "у": "u",
        "ф": "f", "х": "kh", "ц": "ts", "ч": "ch", "ш": "sh", "щ": "sch",
        "ъ": "", "ы": "y", "ь": "", "э": "e", "ю": "yu", "я": "ya",
    ]
    var out = ""
    for ch in s.lowercased() {
        if let r = map[ch] { out += r } else { out.append(ch) }
    }
    return out
}

/// Levenshtein similarity in [0,1]: 1 - dist/maxLen.
func levSim(_ a: String, _ b: String) -> Float {
    let s = Array(a), t = Array(b)
    if s.isEmpty || t.isEmpty { return (s.isEmpty && t.isEmpty) ? 1 : 0 }
    var prev = Array(0...t.count)
    var cur = [Int](repeating: 0, count: t.count + 1)
    for i in 1...s.count {
        cur[0] = i
        for j in 1...t.count {
            let cost = s[i - 1] == t[j - 1] ? 0 : 1
            cur[j] = Swift.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost)
        }
        swap(&prev, &cur)
    }
    return 1 - Float(prev[t.count]) / Float(max(s.count, t.count))
}

/// Load canonical terms (the part before ':' on each line) from a vocab file.
/// Aliases are intentionally ignored — the whole point of translit matching is
/// to NOT need hand-written manglings.
func loadCanonicalTerms(_ path: String) -> [String] {
    guard let contents = try? String(contentsOfFile: path, encoding: .utf8) else { return [] }
    var terms: [String] = []
    for line in contents.split(whereSeparator: { $0.isNewline }) {
        let t = line.trimmingCharacters(in: .whitespaces)
        if t.isEmpty || t.hasPrefix("#") { continue }
        let canon = t.split(separator: ":").first.map {
            String($0).trimmingCharacters(in: .whitespaces)
        } ?? t
        if !canon.isEmpty { terms.append(canon) }
    }
    return terms
}

/// Phonetic key: transliterate, fold c/q/k->k and z/s->s, drop vowels, collapse
/// runs. "папилайн"->"pln", "pipeline"->"pln"; "клод"->"kld", "claude"->"kld".
/// Recovers heavily-mangled cross-script matches that raw edit distance misses.
func phoneticKey(_ s: String) -> String {
    let lat = translitRuToLat(s).lowercased()
    var mapped = ""
    for ch in lat {
        switch ch {
        case "c", "q", "k": mapped.append("k")
        case "z", "s": mapped.append("s")
        case "a", "e", "i", "o", "u", "y": break  // drop vowels
        case "a"..."z": mapped.append(ch)
        default: break  // drop punctuation/digits
        }
    }
    var key = ""
    var last: Character? = nil
    for ch in mapped where ch != last { key.append(ch); last = ch }
    return key
}

/// Replace Cyrillic words whose transliteration is close to an English canonical.
/// Word-level, length>=4, Cyrillic-only candidates; preserves surrounding
/// punctuation and the canonical's original casing. Returns (text, replacements).
func translitRescore(text: String, canonicals: [String], threshold: Float, minLen: Int = 4, matchAll: Bool = false) -> (String, [(String, String, Float)]) {
    var repls: [(String, String, Float)] = []
    let words = text.split(separator: " ", omittingEmptySubsequences: false).map(String.init)
    var out: [String] = []
    for w in words {
        let core = w.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        let hasCyr = core.unicodeScalars.contains { $0.value >= 0x0400 && $0.value <= 0x04FF }
        if core.count >= minLen, hasCyr || matchAll {
            let coreKey = phoneticKey(core)
            var best: (String, Float)? = nil
            if !coreKey.isEmpty {
                for c in canonicals {
                    let sim = levSim(coreKey, phoneticKey(c))
                    if sim >= threshold, best == nil || sim > best!.1 { best = (c, sim) }
                }
            }
            if let b = best {
                out.append(w.replacingOccurrences(of: core, with: b.0))
                repls.append((core, b.0, b.1))
                continue
            }
        }
        out.append(w)
    }
    return (out.joined(separator: " "), repls)
}

/// Split ASR token timings into segments at pauses — a gap between consecutive
/// tokens larger than `gapThreshold` seconds starts a new segment. No
/// diarization: every segment is "Speaker 1". This drives the lifelog timecode
/// view — a fresh timestamp appears wherever speech resumes after silence, and
/// pure-silence stretches produce no segment at all (the timestamp marks the
/// moment text actually starts).
func segmentByPause(
    tokenTimings: [TokenTiming],
    gapThreshold: Double,
    fallbackText: String
) -> [SpeakerSegment] {
    guard !tokenTimings.isEmpty else {
        return fallbackText.isEmpty
            ? []
            : [SpeakerSegment(speakerId: "Speaker 1", startTime: 0, endTime: 0, text: fallbackText)]
    }

    var segments: [SpeakerSegment] = []
    var curTokens: [String] = []
    var segStart = tokenTimings[0].startTime
    var segEnd = tokenTimings[0].endTime
    var prevEnd = tokenTimings[0].startTime

    func flush() {
        guard !curTokens.isEmpty else { return }
        let text = joinTokens(curTokens)
        if !text.isEmpty {
            segments.append(SpeakerSegment(
                speakerId: "Speaker 1",
                startTime: segStart,
                endTime: segEnd,
                text: text
            ))
        }
        curTokens = []
    }

    for t in tokenTimings {
        if t.startTime - prevEnd > gapThreshold, !curTokens.isEmpty {
            flush()
            segStart = t.startTime
        }
        curTokens.append(t.token)
        segEnd = t.endTime
        prevEnd = t.endTime
    }
    flush()
    return segments
}

/// Merge ASR word timings with diarization segments.
func mergeAsrWithDiarization(
    tokenTimings: [TokenTiming],
    fullText: String,
    diarizationSegments: [TimedSpeakerSegment]
) -> ([SpeakerSegment], Int) {
    guard !diarizationSegments.isEmpty else {
        let endTime = tokenTimings.last?.endTime ?? 0
        return ([SpeakerSegment(
            speakerId: "Speaker 1",
            startTime: 0,
            endTime: endTime,
            text: fullText
        )], 1)
    }

    guard !tokenTimings.isEmpty else {
        return ([SpeakerSegment(
            speakerId: "Speaker 1",
            startTime: Double(diarizationSegments.first?.startTimeSeconds ?? 0),
            endTime: Double(diarizationSegments.last?.endTimeSeconds ?? 0),
            text: fullText
        )], Set(diarizationSegments.map { $0.speakerId }).count)
    }

    struct WordWithSpeaker {
        let token: String
        let startTime: Double
        let endTime: Double
        let speakerId: String
    }

    var assignedWords: [WordWithSpeaker] = []
    let defaultSpeaker = diarizationSegments.first?.speakerId ?? "Speaker 1"

    for timing in tokenTimings {
        let midpoint = (timing.startTime + timing.endTime) / 2.0

        var matchedSpeaker = defaultSpeaker
        for seg in diarizationSegments {
            let start = Double(seg.startTimeSeconds)
            let end = Double(seg.endTimeSeconds)
            if midpoint >= start && midpoint <= end {
                matchedSpeaker = seg.speakerId
                break
            }
        }

        assignedWords.append(WordWithSpeaker(
            token: timing.token,
            startTime: timing.startTime,
            endTime: timing.endTime,
            speakerId: matchedSpeaker
        ))
    }

    var segments: [SpeakerSegment] = []
    var currentSpeaker = ""
    var currentWords: [String] = []
    var segmentStart: Double = 0
    var segmentEnd: Double = 0

    for word in assignedWords {
        if word.speakerId != currentSpeaker {
            if !currentWords.isEmpty {
                segments.append(SpeakerSegment(
                    speakerId: currentSpeaker,
                    startTime: segmentStart,
                    endTime: segmentEnd,
                    text: joinTokens(currentWords)
                ))
            }
            currentSpeaker = word.speakerId
            currentWords = [word.token]
            segmentStart = word.startTime
            segmentEnd = word.endTime
        } else {
            currentWords.append(word.token)
            segmentEnd = word.endTime
        }
    }

    if !currentWords.isEmpty {
        segments.append(SpeakerSegment(
            speakerId: currentSpeaker,
            startTime: segmentStart,
            endTime: segmentEnd,
            text: joinTokens(currentWords)
        ))
    }

    let uniqueSpeakers = Array(Set(segments.map { $0.speakerId })).sorted()
    var speakerMap: [String: String] = [:]
    for (i, id) in uniqueSpeakers.enumerated() {
        speakerMap[id] = "Speaker \(i + 1)"
    }

    let normalizedSegments = segments.map { seg in
        SpeakerSegment(
            speakerId: speakerMap[seg.speakerId] ?? seg.speakerId,
            startTime: seg.startTime,
            endTime: seg.endTime,
            text: seg.text
        )
    }

    let outputSpeakerCount = Set(normalizedSegments.map { $0.speakerId }).count

    return (normalizedSegments, outputSpeakerCount)
}

// MARK: - Streaming (NDJSON over stdin/stdout)

/// Emit a JSON line to stdout (followed by '\n'). Used for streaming events.
func emitJSON<T: Encodable>(_ value: T) {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(value) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

struct StreamReady: Encodable { let type = "ready" }
struct StreamPartial: Encodable { let type = "partial"; let text: String }
struct StreamFinal: Encodable { let type = "final"; let text: String }
struct StreamError: Encodable { let type = "error"; let message: String }

/// Convert raw interleaved f32 mono PCM bytes (little-endian) into an
/// AVAudioPCMBuffer at the given sample rate.
func pcmBufferFromF32Bytes(_ data: Data, sampleRate: Double) -> AVAudioPCMBuffer? {
    let frameCount = data.count / MemoryLayout<Float>.size
    guard frameCount > 0 else { return nil }
    guard let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: sampleRate,
        channels: 1,
        interleaved: false
    ) else { return nil }
    guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frameCount)) else {
        return nil
    }
    buffer.frameLength = AVAudioFrameCount(frameCount)
    data.withUnsafeBytes { rawPtr in
        guard let src = rawPtr.bindMemory(to: Float.self).baseAddress else { return }
        guard let dst = buffer.floatChannelData?[0] else { return }
        dst.update(from: src, count: frameCount)
    }
    return buffer
}

/// FluidAudio's standard cache root — `~/Library/Application Support/FluidAudio`.
/// `DownloadUtils.downloadRepo` appends `repo.folderName` (e.g.
/// `parakeet-eou-streaming/160ms`) to this on download, so loadModels needs
/// the same composed path.
func fluidAudioCacheRoot() -> URL {
    let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    return appSupport.appendingPathComponent("FluidAudio", isDirectory: true)
}

func repoForChunkSize(_ chunkSize: StreamingChunkSize) -> Repo {
    switch chunkSize {
    case .ms160: return .parakeetEou160
    case .ms320: return .parakeetEou320
    case .ms1280: return .parakeetEou1280
    }
}

func streamingModelDir(chunkSize: StreamingChunkSize) -> URL {
    fluidAudioCacheRoot().appendingPathComponent(repoForChunkSize(chunkSize).folderName, isDirectory: true)
}

/// Download Parakeet EOU CoreML models if not already cached.
func ensureStreamingModelsCached(chunkSize: StreamingChunkSize) async throws {
    let destination = streamingModelDir(chunkSize: chunkSize)
    let encoderPath = destination.appendingPathComponent("streaming_encoder.mlmodelc")
    let decoderPath = destination.appendingPathComponent("decoder.mlmodelc")
    if FileManager.default.fileExists(atPath: encoderPath.path)
        && FileManager.default.fileExists(atPath: decoderPath.path)
    {
        return
    }
    writeProgress("Downloading streaming model", 5)
    try await DownloadUtils.downloadRepo(repoForChunkSize(chunkSize), to: fluidAudioCacheRoot())
}

/// Some HuggingFace repos ship only the `.mlpackage` source (e.g. the EOU
/// 320ms variant) rather than a prebuilt `.mlmodelc`. CoreML can't load
/// `.mlpackage` directly — has to compile it first. This is the same dance
/// FluidAudio's own `Qwen3AsrModels.loadModel` does internally for `.mlpackage`
/// inputs. Compilation result is cached next to the source so we only pay
/// the cost on the first run after download.
func ensureCompiledStreamingModels(in dir: URL) async throws {
    let modelNames = [
        "streaming_encoder",
        "decoder",
        "joint_decision",
        "parakeet_eou_preprocessor",
    ]
    for name in modelNames {
        let compiledPath = dir.appendingPathComponent("\(name).mlmodelc")
        let packagePath = dir.appendingPathComponent("\(name).mlpackage")
        if FileManager.default.fileExists(atPath: compiledPath.path) {
            continue
        }
        guard FileManager.default.fileExists(atPath: packagePath.path) else {
            continue
        }
        writeProgress("Compiling \(name)", 30)
        let tempCompiledURL = try await MLModel.compileModel(at: packagePath)
        try? FileManager.default.removeItem(at: compiledPath)
        try FileManager.default.copyItem(at: tempCompiledURL, to: compiledPath)
        try? FileManager.default.removeItem(at: tempCompiledURL)
    }
}

func runStreamMode() async {
    do {
        // .ms320 — WER ~5% vs ~8% on .ms160. Extra 160ms of in-stream
        // latency but still feels live. The 320ms HF repo ships .mlpackage
        // only; we compile it to .mlmodelc on first launch (cached).
        let chunkSize: StreamingChunkSize = .ms320
        let manager = StreamingEouAsrManager(chunkSize: chunkSize, eouDebounceMs: 1280)

        writeProgress("Loading models", 0)
        try await ensureStreamingModelsCached(chunkSize: chunkSize)
        try await ensureCompiledStreamingModels(in: streamingModelDir(chunkSize: chunkSize))
        try await manager.loadModels(from: streamingModelDir(chunkSize: chunkSize))
        writeProgress("Loading models", 50)

        // EOU callback → partial event. The transcript here is the segment
        // bounded by the detected end-of-utterance — partials accumulate over
        // multiple sentences.
        await manager.setEouCallback { transcript in
            emitJSON(StreamPartial(text: transcript))
        }

        emitJSON(StreamReady())
        writeProgress("Ready", 100)

        // Stream loop: read f32 mono 16 kHz PCM from stdin in fixed chunks.
        // 4096 bytes = 1024 f32 samples = 64 ms @ 16 kHz — comfortably small
        // for the 160 ms chunkSize the manager wants. The manager buffers
        // internally so we can push any size.
        let stdin = FileHandle.standardInput
        let readSize = 4096
        var accumulated = ""
        while true {
            let data = try stdin.read(upToCount: readSize) ?? Data()
            if data.isEmpty {
                break
            }
            guard let buffer = pcmBufferFromF32Bytes(data, sampleRate: 16_000) else {
                continue
            }
            let incremental = try await manager.process(audioBuffer: buffer)
            if !incremental.isEmpty {
                accumulated += incremental
                emitJSON(StreamPartial(text: accumulated))
            }
        }

        // EOF: flush whatever's left and emit the consolidated transcript.
        let tail = try await manager.finish()
        let finalText = accumulated + tail
        emitJSON(StreamFinal(text: finalText))
        exit(0)
    } catch {
        emitJSON(StreamError(message: error.localizedDescription))
        exit(1)
    }
}

/// Report whether the active engine/variant is already downloaded, plus its HF
/// repo id (for NBP's Rust side to check the remote version). Uses FluidAudio's
/// public `modelsExist` so cache-layout knowledge stays inside the library.
struct ModelStatusJSON: Encodable {
    let engine: String
    let variant: String
    let repo: String
    let cached: Bool
}

/// Emit the HF repo id for every managed on-device engine, sourced from
/// FluidAudio's own `Repo` enum. Lets NBP label the model picker with real
/// model names (single source — can't drift from what FluidAudio downloads).
func runListModels() {
    let models: [String: String] = [
        "parakeet-v3": Repo.parakeetV3.remotePath,
        "qwen3": Repo.qwen3Asr.remotePath,
    ]
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    if let data = try? encoder.encode(models) {
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
    exit(0)
}

func runStatus(argv: [String]) async {
    var engine = "parakeet-v3"
    var variant = "f32"
    var i = 1
    while i < argv.count {
        switch argv[i] {
        case "--engine": if i + 1 < argv.count { engine = argv[i + 1].lowercased(); i += 1 }
        case "--variant": if i + 1 < argv.count { variant = argv[i + 1].lowercased(); i += 1 }
        default: break
        }
        i += 1
    }

    var cached = false
    let repo: String
    if engine == "qwen3" {
        let v: Qwen3AsrVariant = (variant == "int8") ? .int8 : .f32
        // Repo id from FluidAudio's own enum — single source of truth, can't
        // drift from what FluidAudio actually downloads.
        repo = (v == .int8 ? Repo.qwen3AsrInt8 : Repo.qwen3Asr).remotePath
        if #available(macOS 15, iOS 18, *) {
            cached = Qwen3AsrModels.modelsExist(at: Qwen3AsrModels.defaultCacheDirectory(variant: v))
        }
    } else {
        repo = Repo.parakeetV3.remotePath
        cached = AsrModels.modelsExist(at: AsrModels.defaultCacheDirectory(for: .v3))
    }

    let out = ModelStatusJSON(engine: engine, variant: variant, repo: repo, cached: cached)
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    if let data = try? encoder.encode(out) {
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
    exit(0)
}

/// Download (or force-redownload) the active engine/variant via FluidAudio.
/// `--force` removes the cached model dir first, so an update re-pulls main.
/// Throttles progress emission to whole-percent changes — avoids flooding
/// stderr / the IPC pipe with thousands of byte-level ticks on a multi-GB pull.
final class ProgressThrottle: @unchecked Sendable {
    private var last = ""
    private let lock = NSLock()
    /// Emit on any change of stage OR whole percent — so a phase flip at the
    /// same percent (e.g. download→compile at 50%) still surfaces in the UI.
    func shouldEmit(_ stage: String, _ pct: Int) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let key = "\(stage):\(pct)"
        if key != last {
            last = key
            return true
        }
        return false
    }
}

func runDownload(argv: [String]) async {
    var engine = "parakeet-v3"
    var variant = "f32"
    var force = false
    var i = 1
    while i < argv.count {
        switch argv[i] {
        case "--engine": if i + 1 < argv.count { engine = argv[i + 1].lowercased(); i += 1 }
        case "--variant": if i + 1 < argv.count { variant = argv[i + 1].lowercased(); i += 1 }
        case "--force": force = true
        default: break
        }
        i += 1
    }

    do {
        // Real progress from FluidAudio: fractionCompleted spans download +
        // compile (0→1). Throttled to whole percents.
        let throttle = ProgressThrottle()
        let onProgress: DownloadUtils.ProgressHandler = { p in
            let pct = Int(p.fractionCompleted * 100)
            // Forward FluidAudio's real phase (label kept colon-free for the
            // `PROGRESS:stage:pct` line the Rust side parses).
            let stage: String
            switch p.phase {
            case .listing: stage = "Preparing"
            case .downloading: stage = "Downloading"
            case .compiling: stage = "Compiling"
            }
            if throttle.shouldEmit(stage, pct) { writeProgress(stage, pct) }
        }
        writeProgress("Preparing", 0)
        if engine == "qwen3" {
            guard #available(macOS 15, iOS 18, *) else {
                writeError("Qwen3-ASR requires macOS 15 or later")
            }
            let v: Qwen3AsrVariant = (variant == "int8") ? .int8 : .f32
            _ = try await Qwen3AsrModels.download(variant: v, force: force, progressHandler: onProgress)
        } else {
            if force {
                try? FileManager.default.removeItem(at: AsrModels.defaultCacheDirectory(for: .v3))
            }
            _ = try await AsrModels.downloadAndLoad(version: .v3, progressHandler: onProgress)
        }
        writeProgress("Complete", 100)
        FileHandle.standardOutput.write(Data("{\"ok\":true}\n".utf8))
        exit(0)
    } catch {
        writeError(error.localizedDescription)
    }
}

/// Isolated validation of the ported spectral clusterer. Reads
/// {"embeddings":[[Float]...],"starts":[Double...]} and prints speaker count.
func runClusterTest(path: String) {
    guard !path.isEmpty, let data = FileManager.default.contents(atPath: path),
        let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
        let embAny = obj["embeddings"] as? [[Any]]
    else {
        writeError("cluster-test: need JSON {embeddings:[[..]], starts:[..]}")
    }
    let embeddings: [[Float]] = embAny.map { row in row.compactMap { ($0 as? NSNumber)?.floatValue } }
    let starts: [Double] =
        (obj["starts"] as? [Any])?.compactMap { ($0 as? NSNumber)?.doubleValue }
        ?? (0..<embeddings.count).map { Double($0) }
    let labels = SpeakerClustering.cluster(embeddings: embeddings, starts: starts)
    let uniq = Set(labels).sorted()
    var counts: [Int: Int] = [:]
    for l in labels { counts[l, default: 0] += 1 }
    FileHandle.standardOutput.write(Data("SPEAKERS: \(uniq.count)\n".utf8))
    for s in uniq {
        FileHandle.standardOutput.write(Data("  speaker \(s): \(counts[s]!) segs\n".utf8))
    }
}

// MARK: - Main

@main
struct FluidAudioSidecar {
    static func main() async {
        guard CommandLine.arguments.count >= 2 else {
            writeError("Usage: fluidaudio-sidecar <path-to-wav> [--engine parakeet-v3|qwen3] [--variant f32|int8] [--vocab <file>] [--lang <code>] [--no-diarize] [--segment-pause <sec>]  |  fluidaudio-sidecar --stream")
        }

        if CommandLine.arguments[1] == "--resident-parakeet" {
            await runResidentParakeet()
            return
        }

        if CommandLine.arguments[1] == "--stream" {
            await runStreamMode()
            return
        }

        // Isolated test of the ported spectral clusterer: reads a JSON
        // {"embeddings":[[...]],"starts":[...]} and prints auto-detected speaker
        // count. Validates the clustering core independent of the embedder.
        if CommandLine.arguments[1] == "--cluster-test" {
            runClusterTest(path: CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "")
            return
        }

        // Self-contained native diarization (dense embeddings + spectral).
        if CommandLine.arguments[1] == "--diarize-v2" {
            await runDiarizeV2(wavPath: CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "")
            return
        }

        // Source-split diarization: <system.wav> <mic.wav> (clean stems).
        if CommandLine.arguments[1] == "--diarize-v2-split" {
            await runDiarizeV2Split(
                systemWav: CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "",
                micWav: CommandLine.arguments.count > 3 ? CommandLine.arguments[3] : "")
            return
        }

        // Report cache state for the active engine/variant (used by NBP's model
        // version checker). Uses FluidAudio's own modelsExist — we don't
        // duplicate its cache layout.
        if CommandLine.arguments.contains("--list-models") {
            runListModels()
            return
        }

        if CommandLine.arguments.contains("--status") {
            await runStatus(argv: CommandLine.arguments)
            return
        }

        // Explicitly (re)download the active model. `--force` clears the cache
        // first (for updates). FluidAudio owns the actual download.
        if CommandLine.arguments.contains("--download") {
            await runDownload(argv: CommandLine.arguments)
            return
        }

        // Parse args: first non-flag token is the WAV path; flags select the
        // engine and per-engine options. Defaults preserve the legacy behavior
        // (`<wav> [--no-diarize]` → Parakeet v3 batch + diarization).
        let argv = CommandLine.arguments
        var wavPath = ""
        var engine = "parakeet-v3"
        var variant = "f32"
        var vocabPath: String? = nil
        var vocabCbw: Float? = nil
        var vocabMinSim: Float? = nil
        var vocabMinScore: Float? = nil
        var useTranslit = false
        var useTranslitAll = false
        var translitThreshold: Float = 0.68
        var translitMinLen = 4
        var langCode: String? = nil
        var noDiarize = false
        // When set (seconds), and diarization is off, split the transcript into
        // timecoded segments at token gaps larger than this. Opt-in: the app's
        // plain `--no-diarize` path keeps the single-block behaviour.
        var segmentPause: Double? = nil
        var emitEmbeddings = false
        var ai = 1
        while ai < argv.count {
            switch argv[ai] {
            case "--no-diarize": noDiarize = true
            case "--engine": if ai + 1 < argv.count { engine = argv[ai + 1].lowercased(); ai += 1 }
            case "--variant": if ai + 1 < argv.count { variant = argv[ai + 1].lowercased(); ai += 1 }
            case "--vocab": if ai + 1 < argv.count { vocabPath = argv[ai + 1]; ai += 1 }
            case "--vocab-cbw": if ai + 1 < argv.count { vocabCbw = Float(argv[ai + 1]); ai += 1 }
            case "--vocab-min-sim": if ai + 1 < argv.count { vocabMinSim = Float(argv[ai + 1]); ai += 1 }
            case "--vocab-min-score": if ai + 1 < argv.count { vocabMinScore = Float(argv[ai + 1]); ai += 1 }
            case "--translit": useTranslit = true
            case "--translit-all": useTranslit = true; useTranslitAll = true
            case "--translit-threshold": if ai + 1 < argv.count { translitThreshold = Float(argv[ai + 1]) ?? translitThreshold; ai += 1 }
            case "--translit-min-len": if ai + 1 < argv.count { translitMinLen = Int(argv[ai + 1]) ?? translitMinLen; ai += 1 }
            case "--lang", "--language", "-l": if ai + 1 < argv.count { langCode = argv[ai + 1]; ai += 1 }
            case "--segment-pause": if ai + 1 < argv.count { segmentPause = Double(argv[ai + 1]); ai += 1 }
            case "--emit-embeddings": emitEmbeddings = true
            default:
                if !argv[ai].hasPrefix("--") && wavPath.isEmpty { wavPath = argv[ai] }
            }
            ai += 1
        }
        if wavPath.isEmpty {
            writeError("Missing WAV path")
        }

        // Qwen3 is a separate generative engine — dispatch and return early.
        if engine == "qwen3" {
            await runQwen3(wavPath: wavPath, variant: variant, langCode: langCode)
            return
        }
        if engine != "parakeet-v3" && engine != "parakeet" {
            writeError("Unknown --engine '\(engine)'. Use parakeet-v3 or qwen3.")
        }

        // Optional script-aware language hint for Parakeet v3 (nil = none).
        // NOTE: forcing a language SUPPRESSES the other script — leave nil for
        // RU+EN code-switch; only set via --lang for deliberate experiments.
        let language: Language? = langCode.flatMap { Language(rawValue: $0) }

        let fileURL = URL(fileURLWithPath: wavPath)

        guard FileManager.default.fileExists(atPath: wavPath) else {
            writeError("File not found: \(wavPath)")
        }

        func elapsed(_ since: CFAbsoluteTime) -> String {
            String(format: "%.3fs", CFAbsoluteTimeGetCurrent() - since)
        }
        func tick(_ label: String, _ since: CFAbsoluteTime) {
            FileHandle.standardError.write(Data("TIMING:\(label):\(elapsed(since))\n".utf8))
        }

        do {
            // v2 diarization uses its own bundled models (CAM++/pyannote), so
            // FluidAudio's offline-diarizer repo is never needed here.
            let cached = modelsAreCached(requireDiarizer: false)

            writeProgress("Preparing models", 0)

            if !cached { writeProgress("Downloading ASR model", 5) }
            let tDownload = CFAbsoluteTimeGetCurrent()
            // When all required v3 files are present, skip FluidAudio's network probe.
            // `downloadAndLoad` is all-or-nothing — one missing file re-downloads the
            // whole 2.8 GB repo even if the rest is cached.
            let asrModels = cached
                ? try await AsrModels.loadFromCache(version: .v3)
                : try await AsrModels.downloadAndLoad(version: .v3)
            tick("asrModels.downloadAndLoad", tDownload)

            let tInit = CFAbsoluteTimeGetCurrent()
            let asrManager = AsrManager(config: .default)
            // FluidAudio 0.14.5: loadModels replaces the old initialize(models:).
            try await asrManager.loadModels(asrModels)
            tick("asrManager.loadModels", tInit)
            writeProgress("Preparing models", 15)

            // Quick Dictate passes --no-diarize: single-speaker, only the
            // transcript matters. Diarization (when on) runs the senko-port v2
            // pipeline AFTER ASR — one Parakeet pass produces both the
            // transcript and the speaker map (no double ASR).
            writeProgress("Preparing models", 25)

            writeProgress("Transcribing", 25)

            let progressTask = Task {
                let stream = await asrManager.transcriptionProgressStream
                for try await progress in stream {
                    let percent = 25 + Int(progress * 35.0)
                    writeProgress("Transcribing", percent)
                }
            }

            // FluidAudio 0.14.5: transcribe takes an inout decoder state.
            let tTranscribe = CFAbsoluteTimeGetCurrent()
            var decoderState = TdtDecoderState.make(decoderLayers: await asrManager.decoderLayerCount)
            let asrResult = try await asrManager.transcribe(fileURL, decoderState: &decoderState, language: language)
            tick("asrManager.transcribe", tTranscribe)
            progressTask.cancel()
            writeProgress("Transcribing", 60)

            // Senko-port v2 diarization on the same wav (VAD → fbank → CAM++ →
            // spectral). Failure degrades to a single-speaker transcript, same
            // tolerance as the old FluidAudio diarizer path.
            var diarV2Result: DiarPipelineResult? = nil
            if !noDiarize {
                let tDiarize = CFAbsoluteTimeGetCurrent()
                do {
                    diarV2Result = try await runDiarPipeline(wavPath: wavPath) { stage, pct in
                        // Map the pipeline's 0-100 into the transcribe flow's 60-90 band.
                        writeProgress(stage, 60 + Int(Double(pct) * 0.3))
                    }
                } catch {
                    FileHandle.standardError.write(Data("diarization skipped: \(error)\n".utf8))
                }
                tick("diarizer.process", tDiarize)
                writeProgress("Diarization", 90)
            }

            // --emit-embeddings belonged to the retired FluidAudio diarizer
            // spike path; v2 exposes per-speaker centroids in `diarV2` instead.
            let diarEmbeddings: [DiarEmbedding]? = nil
            _ = emitEmbeddings

            writeProgress("Finalizing", 90)

            // Optional custom-vocabulary rescoring (Parakeet v3): re-insert
            // domain / English terms the TDT path mangled, via an auxiliary CTC
            // keyword spotter + rescorer over the existing token timings.
            // English-oriented spotter — aimed at boosting English insertions
            // inside Russian speech. Beta; unverified for mixed script.
            var fullText = asrResult.text
            var modelLabel = "parakeet-tdt-v3"
            if let vp = vocabPath {
                if useTranslit {
                    writeProgress("Transliteration matching", 92)
                    let canon = loadCanonicalTerms(vp)
                    let (newText, repls) = translitRescore(
                        text: asrResult.text, canonicals: canon, threshold: translitThreshold,
                        minLen: translitMinLen, matchAll: useTranslitAll)
                    fullText = newText
                    FileHandle.standardError.write(Data(
                        "TRANSLIT: \(repls.count) replacement(s) [threshold=\(translitThreshold), \(canon.count) terms]\n".utf8))
                    for (o, n, sim) in repls {
                        FileHandle.standardError.write(Data(
                            "TRANSLIT:  '\(o)' -> '\(n)' (sim=\(String(format: "%.2f", sim)))\n".utf8))
                    }
                    modelLabel = "parakeet-tdt-v3+translit"
                } else {
                writeProgress("Vocabulary boosting", 92)
                let (customVocab, ctcModels) = try await CustomVocabularyContext.loadWithCtcTokens(from: vp)
                let blankId = ctcModels.vocabulary.count
                let spotter = CtcKeywordSpotter(models: ctcModels, blankId: blankId)
                let vocabSamples = try AudioConverter().resampleAudioFile(path: wavPath)
                let spotResult = try await spotter.spotKeywordsWithLogProbs(
                    audioSamples: vocabSamples,
                    customVocabulary: customVocab,
                    minScore: vocabMinScore
                )
                if let tokenTimings = asrResult.tokenTimings,
                    !tokenTimings.isEmpty, !spotResult.logProbs.isEmpty
                {
                    let ctcModelDir = CtcModels.defaultCacheDirectory(for: ctcModels.variant)
                    let vocabConfig = ContextBiasingConstants.rescorerConfig(forVocabSize: customVocab.terms.count)
                    let cbw = vocabCbw ?? vocabConfig.cbw
                    let minSim = vocabMinSim ?? vocabConfig.minSimilarity
                    let rescorer = try await VocabularyRescorer.create(
                        spotter: spotter,
                        vocabulary: customVocab,
                        config: VocabularyRescorer.Config.default,
                        ctcModelDirectory: ctcModelDir
                    )
                    let rescoreOutput = rescorer.ctcTokenRescore(
                        transcript: asrResult.text,
                        tokenTimings: tokenTimings,
                        logProbs: spotResult.logProbs,
                        frameDuration: spotResult.frameDuration,
                        cbw: cbw,
                        marginSeconds: ContextBiasingConstants.defaultMarginSeconds,
                        minSimilarity: minSim
                    )
                    let scoreDesc = vocabMinScore.map { String($0) } ?? "default"
                    if rescoreOutput.wasModified {
                        fullText = rescoreOutput.text
                        FileHandle.standardError.write(Data(
                            "VOCAB: \(rescoreOutput.replacements.count) replacement(s) [cbw=\(cbw) minSim=\(minSim) minScore=\(scoreDesc)]\n".utf8))
                        for r in rescoreOutput.replacements where r.shouldReplace {
                            FileHandle.standardError.write(Data(
                                "VOCAB:  '\(r.originalWord)' -> '\(r.replacementWord ?? "")'\n".utf8))
                        }
                    } else {
                        FileHandle.standardError.write(Data(
                            "VOCAB: no replacements [cbw=\(cbw) minSim=\(minSim) minScore=\(scoreDesc)]\n".utf8))
                    }
                }
                modelLabel = "parakeet-tdt-v3+vocab"
                }
            }
            if let lc = langCode { modelLabel += "+lang:\(lc)" }

            let timings = asrResult.tokenTimings ?? []
            let segments: [SpeakerSegment]
            let speakerCount: Int
            var diarV2Payload: DiarV2Output? = nil
            if let diar = diarV2Result, !diar.segments.isEmpty {
                // One ASR pass feeds both outputs: speaker-attributed transcript
                // segments + the diarization payload (segments/centroids) that
                // the app persists as diarization.json.
                let textSegs = mergeTokensWithSpeakers(tokenTimings: timings, diar: diar.segments)
                segments = textSegs.map {
                    SpeakerSegment(
                        speakerId: "Speaker \($0.speaker + 1)",
                        startTime: $0.start,
                        endTime: $0.end,
                        text: $0.text)
                }
                speakerCount = diar.speakerCount
                diarV2Payload = DiarV2Output(
                    speakerCount: diar.speakerCount, segments: textSegs, centroids: diar.centroids)
            } else if let gap = segmentPause {
                // Timecoded, pause-split, no diarization (lifelog CLI path).
                segments = segmentByPause(
                    tokenTimings: timings,
                    gapThreshold: gap,
                    fallbackText: fullText
                )
                speakerCount = 1
            } else {
                (segments, speakerCount) = mergeAsrWithDiarization(
                    tokenTimings: timings,
                    fullText: fullText,
                    diarizationSegments: []
                )
            }

            let output = FluidAudioOutputJSON(
                text: fullText,
                speakerCount: speakerCount,
                model: modelLabel,
                segments: segments,
                diarEmbeddings: diarEmbeddings,
                diarV2: diarV2Payload
            )

            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let json = try encoder.encode(output)

            writeProgress("Complete", 100)
            FileHandle.standardOutput.write(json)
            FileHandle.standardOutput.write(Data("\n".utf8))

            // Drain pipes before exit so Tauri's shell plugin gets a clean
            // Terminated event. `exit(0)` is the C call — it doesn't await
            // Swift cleanup, but it does flush stdio. We also explicitly
            // close stdout/stderr to make Tauri see EOF on the read side
            // before SIGCHLD arrives — otherwise `rx.recv().await` on the
            // Rust side can hang indefinitely after a fast exit.
            try? FileHandle.standardOutput.close()
            try? FileHandle.standardError.close()

            // Skip the explicit `await asrManager.cleanup()` — it unloads the
            // 600MB CoreML model synchronously which delays process exit
            // (Rust sees Terminated late and counts it as transcribe time).
            // The OS frees everything on exit anyway.
            exit(0)
        } catch {
            writeError(error.localizedDescription)
        }
    }

    /// Qwen3-ASR engine path. Generative encoder-decoder (no script-suppression
    /// filter), the best on-device candidate for RU+EN code-switching. Plain
    /// text only — no diarization / token timings. Requires macOS 15+ (CoreML
    /// MLState for the decoder KV-cache).
    static func runQwen3(wavPath: String, variant variantStr: String, langCode: String?) async {
        guard #available(macOS 15, iOS 18, *) else {
            writeError("Qwen3-ASR requires macOS 15 or later")
        }
        let variant: Qwen3AsrVariant = (variantStr == "int8") ? .int8 : .f32
        let language: Qwen3AsrConfig.Language? = langCode.flatMap { Qwen3AsrConfig.Language(from: $0) }
        do {
            writeProgress("Preparing Qwen3 model", 0)
            let manager = Qwen3AsrManager()
            let cacheDir = try await Qwen3AsrModels.download(variant: variant)
            writeProgress("Loading Qwen3 model", 20)
            try await manager.loadModels(from: cacheDir)

            writeProgress("Transcribing", 40)
            let samples = try AudioConverter().resampleAudioFile(path: wavPath)
            let sr = Qwen3AsrConfig.sampleRate

            // Chunk long audio under the KV-cache budget (see chunkBySilence).
            // Per chunk, cap generated tokens so prompt + output stays < 512.
            let chunks = chunkBySilence(samples, sampleRate: sr, targetSec: 18.0, slackSec: 2.0)
            var pieces: [String] = []
            for (idx, chunk) in chunks.enumerated() {
                let promptEst = Int(14.0 * Double(chunk.count) / Double(sr))
                let maxNew = max(48, min(240, Qwen3AsrConfig.maxCacheSeqLen - promptEst - 24))
                let piece = try await manager.transcribe(
                    audioSamples: chunk,
                    language: language,
                    maxNewTokens: maxNew
                )
                pieces.append(piece.trimmingCharacters(in: .whitespacesAndNewlines))
                let pct = 40 + Int(Double(idx + 1) / Double(max(1, chunks.count)) * 55.0)
                writeProgress("Transcribing", min(95, pct))
            }
            let text = pieces.filter { !$0.isEmpty }.joined(separator: " ")

            let duration = samples.isEmpty ? 0 : Double(samples.count) / Double(sr)
            var modelLabel = "qwen3-asr-\(variant.rawValue)"
            if let lc = langCode { modelLabel += "+lang:\(lc)" }
            let output = FluidAudioOutputJSON(
                text: text,
                speakerCount: 1,
                model: modelLabel,
                segments: [
                    SpeakerSegment(speakerId: "Speaker 1", startTime: 0, endTime: duration, text: text)
                ],
                diarEmbeddings: nil,
                diarV2: nil
            )

            writeProgress("Complete", 100)
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let json = try encoder.encode(output)
            FileHandle.standardOutput.write(json)
            FileHandle.standardOutput.write(Data("\n".utf8))
            try? FileHandle.standardOutput.close()
            try? FileHandle.standardError.close()
            exit(0)
        } catch {
            writeError(error.localizedDescription)
        }
    }
}
