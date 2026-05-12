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
    let wavPath: String
    let lang: String?
}

func parseArgs() -> ParsedArgs {
    let args = CommandLine.arguments
    guard args.count >= 2 else {
        writeError("Usage: apple-speech-sidecar <path-to-wav> [--lang <bcp47>]")
    }
    let wavPath = args[1]
    var lang: String? = nil
    var i = 2
    while i < args.count {
        if args[i] == "--lang", i + 1 < args.count {
            let v = args[i + 1]
            if v != "auto" && !v.isEmpty { lang = v }
            i += 2
        } else {
            i += 1
        }
    }
    return ParsedArgs(wavPath: wavPath, lang: lang)
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

@main
struct AppleSpeechSidecar {
    static func main() async {
        let parsed = parseArgs()

        guard FileManager.default.fileExists(atPath: parsed.wavPath) else {
            writeError("File not found: \(parsed.wavPath)")
        }

        #if canImport(Speech)
        if #available(macOS 26.0, *) {
            await runAnalyzer(wavPath: parsed.wavPath, lang: parsed.lang)
            return
        }
        #endif
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
#endif
