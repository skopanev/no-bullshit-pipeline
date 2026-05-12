import Foundation
import AVFoundation
#if canImport(Speech)
import Speech
#endif

struct SpeakerSegment: Codable {
    let speakerId: String
    let startTime: Double
    let endTime: Double
    let text: String
}

struct AppleSpeechOutputJSON: Codable {
    let text: String
    let speakerCount: Int
    let model: String
    let segments: [SpeakerSegment]
}

func writeError(_ message: String) -> Never {
    FileHandle.standardError.write(Data("Error: \(message)\n".utf8))
    exit(1)
}

/// Write progress update to stderr (parsed by Rust side).
func writeProgress(_ stage: String, _ percent: Int) {
    FileHandle.standardError.write(Data("PROGRESS:\(stage):\(percent)\n".utf8))
}

struct ParsedArgs {
    let wavPath: String?
    let lang: String?
    let stream: Bool
}

func parseArgs() -> ParsedArgs {
    let args = CommandLine.arguments
    guard args.count >= 2 else {
        writeError("Usage: apple-speech-sidecar <path-to-wav> [--lang <bcp47>]  |  apple-speech-sidecar --stream [--lang <bcp47>]")
    }

    var stream = false
    var wavPath: String? = nil
    var lang: String? = nil
    var i = 1
    while i < args.count {
        let a = args[i]
        if a == "--stream" {
            stream = true
            i += 1
        } else if a == "--lang", i + 1 < args.count {
            let v = args[i + 1]
            if v != "auto" && !v.isEmpty { lang = v }
            i += 2
        } else if wavPath == nil {
            wavPath = a
            i += 1
        } else {
            i += 1
        }
    }
    if !stream && wavPath == nil {
        writeError("Missing <path-to-wav> (or pass --stream for live mode)")
    }
    return ParsedArgs(wavPath: wavPath, lang: lang, stream: stream)
}

func emitJSON(text: String, duration: Double) {
    let segment = SpeakerSegment(
        speakerId: "Speaker 1",
        startTime: 0,
        endTime: duration,
        text: text
    )
    let output = AppleSpeechOutputJSON(
        text: text,
        speakerCount: 1,
        model: "apple-speechanalyzer",
        segments: [segment]
    )
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    do {
        let json = try encoder.encode(output)
        FileHandle.standardOutput.write(json)
        FileHandle.standardOutput.write(Data("\n".utf8))
    } catch {
        writeError("Failed to encode output JSON: \(error.localizedDescription)")
    }
}

// MARK: - Streaming (NDJSON over stdin/stdout)

/// Emit a single JSON line to stdout. Used for streaming events.
func emitStreamJSON<T: Encodable>(_ value: T) {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(value) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

struct StreamReady: Encodable { let type = "ready" }
struct StreamPartial: Encodable { let type = "partial"; let text: String; let t: Double }
struct StreamFinal: Encodable { let type = "final"; let text: String; let start: Double; let end: Double }
struct StreamError: Encodable { let type = "error"; let message: String }

/// Convert raw interleaved f32 mono PCM bytes (little-endian) into an
/// AVAudioPCMBuffer at the given sample rate. Mirrors the helper in the
/// fluidaudio-sidecar streaming path so both sidecars speak the same protocol.
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

@main
struct AppleSpeechSidecar {
    static func main() async {
        let parsed = parseArgs()

        #if canImport(Speech)
        if #available(macOS 26.0, *) {
            if parsed.stream {
                await runStream(lang: parsed.lang)
                return
            }
            guard let wavPath = parsed.wavPath else {
                writeError("Batch mode requires a WAV path")
            }
            guard FileManager.default.fileExists(atPath: wavPath) else {
                writeError("File not found: \(wavPath)")
            }
            await runAnalyzer(wavPath: wavPath, lang: parsed.lang)
            return
        }
        #endif
        if parsed.stream {
            emitStreamJSON(StreamError(message: "SpeechAnalyzer requires macOS 26.0 or later"))
            exit(1)
        }
        writeError("SpeechAnalyzer requires macOS 26.0 or later")
    }
}

#if canImport(Speech)
@available(macOS 26.0, *)
func runAnalyzer(wavPath: String, lang: String?) async {
    let fileURL = URL(fileURLWithPath: wavPath)
    let locale: Locale = {
        if let l = lang, !l.isEmpty { return Locale(identifier: l) }
        return Locale.current
    }()

    writeProgress("Loading", 0)

    do {
        let supportedLocales = await SpeechTranscriber.supportedLocales
        let supported = supportedLocales.contains(where: {
            $0.identifier(.bcp47) == locale.identifier(.bcp47)
        })
        if !supported {
            writeError("Locale not supported by SpeechTranscriber: \(locale.identifier)")
        }

        let transcriber = SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [],
            attributeOptions: []
        )

        let installed = await SpeechTranscriber.installedLocales
        let isInstalled = installed.contains(where: {
            $0.identifier(.bcp47) == locale.identifier(.bcp47)
        })

        if !isInstalled {
            writeProgress("Downloading assets", 0)
            if let request = try await AssetInventory.assetInstallationRequest(
                supporting: [transcriber]
            ) {
                try await request.downloadAndInstall()
            }
            writeProgress("Downloading assets", 100)
        }

        writeProgress("Transcribing", 10)

        let analyzer = SpeechAnalyzer(modules: [transcriber])

        let audioFile = try AVAudioFile(forReading: fileURL)
        let sampleRate = audioFile.fileFormat.sampleRate
        let duration = sampleRate > 0
            ? Double(audioFile.length) / sampleRate
            : 0.0

        let collector = Task { () -> String in
            var accumulated = ""
            for try await result in transcriber.results {
                if result.isFinal {
                    accumulated += String(result.text.characters)
                }
            }
            return accumulated
        }

        if let lastSample = try await analyzer.analyzeSequence(from: audioFile) {
            try await analyzer.finalizeAndFinish(through: lastSample)
        } else {
            try await analyzer.finalizeAndFinishThroughEndOfInput()
        }

        let finalText = try await collector.value

        writeProgress("Complete", 100)
        emitJSON(text: finalText, duration: duration)
    } catch {
        let msg = error.localizedDescription.lowercased()
        if msg.contains("asset") && (msg.contains("not installed") || msg.contains("missing")) {
            writeProgress("Downloading assets", 0)
        }
        writeError(error.localizedDescription)
    }
}

@available(macOS 26.0, *)
func runStream(lang: String?) async {
    let locale: Locale = {
        if let l = lang, !l.isEmpty { return Locale(identifier: l) }
        return Locale.current
    }()

    do {
        let supportedLocales = await SpeechTranscriber.supportedLocales
        let supported = supportedLocales.contains(where: {
            $0.identifier(.bcp47) == locale.identifier(.bcp47)
        })
        if !supported {
            emitStreamJSON(StreamError(message: "Locale not supported by SpeechTranscriber: \(locale.identifier)"))
            exit(1)
        }

        // Volatile results give us partial (mid-utterance) text in addition to
        // the finalized phrase emitted on EOU.
        let transcriber = SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [.volatileResults],
            attributeOptions: []
        )

        // Asset install (one-time per locale; OS-managed cache, no model file in
        // ~/.nbp/models). Identical step as the batch path.
        let installed = await SpeechTranscriber.installedLocales
        let isInstalled = installed.contains(where: {
            $0.identifier(.bcp47) == locale.identifier(.bcp47)
        })
        if !isInstalled {
            if let request = try await AssetInventory.assetInstallationRequest(
                supporting: [transcriber]
            ) {
                try await request.downloadAndInstall()
            }
        }

        let analyzer = SpeechAnalyzer(modules: [transcriber])

        // Drain results in a child task so the main task can pump stdin →
        // analyzer. Finalized results are flushed on EOU; volatile results
        // appear in between.
        let resultsTask = Task {
            do {
                for try await result in transcriber.results {
                    let text = String(result.text.characters)
                    if result.isFinal {
                        let start = result.range.start.seconds
                        let end = result.range.end.seconds
                        emitStreamJSON(StreamFinal(text: text, start: start, end: end))
                    } else {
                        let t = result.range.end.seconds
                        emitStreamJSON(StreamPartial(text: text, t: t))
                    }
                }
            } catch {
                emitStreamJSON(StreamError(message: "Result stream error: \(error.localizedDescription)"))
            }
        }

        // Async input sequence — buffers are yielded into this stream and the
        // analyzer consumes from it in the background.
        let (inputStream, inputContinuation) = AsyncStream<AnalyzerInput>.makeStream()

        try await analyzer.start(inputSequence: inputStream)

        // Analyzer is up; signal readiness to the host so it can start piping
        // mic samples. Anything queued in the stdin pipe before this drains
        // through the loop below.
        emitStreamJSON(StreamReady())

        // Pump stdin → AnalyzerInput. 4096 bytes = 1024 f32 samples = 64 ms @
        // 16 kHz, matching the FluidAudio sidecar chunk size. SpeechAnalyzer
        // buffers internally so chunk size isn't critical.
        let stdin = FileHandle.standardInput
        let readSize = 4096
        while true {
            let data = try stdin.read(upToCount: readSize) ?? Data()
            if data.isEmpty {
                break
            }
            guard let buffer = pcmBufferFromF32Bytes(data, sampleRate: 16_000) else {
                continue
            }
            inputContinuation.yield(AnalyzerInput(buffer: buffer))
        }

        // EOF: close the input sequence, flush any in-flight audio, and wait
        // for the results consumer to drain. SpeechAnalyzer emits the final
        // result for the trailing audio before its result stream closes.
        inputContinuation.finish()
        try await analyzer.finalizeAndFinishThroughEndOfInput()
        await resultsTask.value
        exit(0)
    } catch {
        emitStreamJSON(StreamError(message: error.localizedDescription))
        exit(1)
    }
}
#endif
