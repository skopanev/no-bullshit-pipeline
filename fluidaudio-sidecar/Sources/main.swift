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

struct FluidAudioOutputJSON: Codable {
    let text: String
    let speakerCount: Int
    let model: String
    let segments: [SpeakerSegment]
}

func writeError(_ message: String) -> Never {
    FileHandle.standardError.write(Data("Error: \(message)\n".utf8))
    exit(1)
}

/// Write progress update to stderr (parsed by Rust side)
func writeProgress(_ stage: String, _ percent: Int) {
    FileHandle.standardError.write(Data("PROGRESS:\(stage):\(percent)\n".utf8))
}

/// Check if FluidAudio models are already cached
func modelsAreCached() -> Bool {
    let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
    guard let modelsDir = appSupport?.appendingPathComponent("FluidAudio/Models") else { return false }

    let asrModel = modelsDir.appendingPathComponent("parakeet-tdt-0.6b-v3-coreml")
    let diarModel = modelsDir.appendingPathComponent("speaker-diarization-coreml")

    return FileManager.default.fileExists(atPath: asrModel.path) &&
           FileManager.default.fileExists(atPath: diarModel.path)
}

/// Join sub-word tokens into text, respecting word boundaries.
func joinTokens(_ tokens: [String]) -> String {
    tokens.joined()
        .replacingOccurrences(of: "\u{2581}", with: " ")
        .trimmingCharacters(in: .whitespaces)
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

// MARK: - Main

@main
struct FluidAudioSidecar {
    static func main() async {
        guard CommandLine.arguments.count >= 2 else {
            writeError("Usage: fluidaudio-sidecar <path-to-wav>  |  fluidaudio-sidecar --stream")
        }

        if CommandLine.arguments[1] == "--stream" {
            await runStreamMode()
            return
        }

        let wavPath = CommandLine.arguments[1]
        let fileURL = URL(fileURLWithPath: wavPath)

        guard FileManager.default.fileExists(atPath: wavPath) else {
            writeError("File not found: \(wavPath)")
        }

        do {
            let cached = modelsAreCached()

            writeProgress("Preparing models", 0)

            if !cached { writeProgress("Downloading ASR model", 5) }
            let asrModels = try await AsrModels.downloadAndLoad(version: .v3)
            let asrManager = AsrManager(config: .default)
            // FluidAudio 0.14.5: loadModels replaces the old initialize(models:).
            try await asrManager.loadModels(asrModels)
            writeProgress("Preparing models", 15)

            if !cached { writeProgress("Downloading diarizer", 20) }
            var diarizerConfig = OfflineDiarizerConfig()
            diarizerConfig.clusteringThreshold = 0.12
            diarizerConfig.embeddingExcludeOverlap = false
            diarizerConfig.minSegmentDuration = 0.1
            diarizerConfig.minGapDuration = 0.3
            diarizerConfig.clustering.minSpeakers = 2
            let diarizerManager = OfflineDiarizerManager(config: diarizerConfig)
            try await diarizerManager.prepareModels()
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
            var decoderState = try TdtDecoderState()
            let asrResult = try await asrManager.transcribe(fileURL, decoderState: &decoderState)
            progressTask.cancel()
            writeProgress("Transcribing", 60)

            writeProgress("Diarization", 60)
            let diarizationResult = try await diarizerManager.process(fileURL)
            writeProgress("Diarization", 90)

            writeProgress("Finalizing", 90)
            let timings = asrResult.tokenTimings ?? []
            let (segments, speakerCount) = mergeAsrWithDiarization(
                tokenTimings: timings,
                fullText: asrResult.text,
                diarizationSegments: diarizationResult.segments
            )

            let output = FluidAudioOutputJSON(
                text: asrResult.text,
                speakerCount: speakerCount,
                model: "parakeet-tdt-v3",
                segments: segments
            )

            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let json = try encoder.encode(output)

            writeProgress("Complete", 100)
            FileHandle.standardOutput.write(json)
            FileHandle.standardOutput.write(Data("\n".utf8))

            await asrManager.cleanup()
        } catch {
            writeError(error.localizedDescription)
        }
    }
}
