import Foundation
import FluidAudio

private struct ResidentRequest: Decodable {
    let id: String
    let operation: String
    let wavPath: String?
    let vocabPath: String?
    let translitThreshold: Float?
    let translitMinLen: Int?
}

private struct ResidentEvent: Encodable {
    let type: String
    let id: String?
    let text: String?
    let model: String?
    let message: String?
}

private func emitResidentEvent(_ event: ResidentEvent) {
    guard let data = try? JSONEncoder().encode(event) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

func runResidentParakeet() async {
    do {
        let cached = modelsAreCached(requireDiarizer: false)
        let models = cached
            ? try await AsrModels.loadFromCache(version: .v3)
            : try await AsrModels.downloadAndLoad(version: .v3)
        let manager = AsrManager(config: .default)
        try await manager.loadModels(models)
        emitResidentEvent(.init(type: "ready", id: nil, text: nil, model: nil, message: nil))

        while let line = readLine() {
            guard let data = line.data(using: .utf8),
                  let request = try? JSONDecoder().decode(ResidentRequest.self, from: data)
            else {
                emitResidentEvent(.init(type: "error", id: nil, text: nil, model: nil,
                                        message: "invalid request"))
                continue
            }
            if request.operation == "ping" {
                emitResidentEvent(.init(type: "pong", id: request.id, text: nil,
                                        model: nil, message: nil))
                continue
            }
            await transcribeResidentRequest(request, manager: manager)
        }
    } catch {
        emitResidentEvent(.init(type: "fatal", id: nil, text: nil, model: nil,
                                message: error.localizedDescription))
    }
}

private func transcribeResidentRequest(_ request: ResidentRequest, manager: AsrManager) async {
    guard request.operation == "transcribe" else {
        emitResidentEvent(.init(type: "error", id: request.id, text: nil, model: nil,
                                message: "unknown operation: \(request.operation)"))
        return
    }
    guard let wavPath = request.wavPath,
          FileManager.default.fileExists(atPath: wavPath)
    else {
        emitResidentEvent(.init(type: "error", id: request.id, text: nil, model: nil,
                                message: "WAV file not found"))
        return
    }

    let started = Date()
    defer {
        let elapsed = Date().timeIntervalSince(started)
        let line = String(format: "TIMING:resident.transcribe:%.3fs\n", elapsed)
        FileHandle.standardError.write(Data(line.utf8))
    }

    do {
        var decoderState = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
        let result = try await manager.transcribe(
            URL(fileURLWithPath: wavPath),
            decoderState: &decoderState,
            language: nil
        )
        var text = result.text
        var model = "parakeet-tdt-v3"
        if let path = request.vocabPath {
            let terms = loadCanonicalTerms(path)
            let recovered = translitRescore(
                text: result.text,
                canonicals: terms,
                threshold: request.translitThreshold ?? 0.72,
                minLen: request.translitMinLen ?? 4,
                matchAll: true
            )
            text = recovered.0
            model += "+translit"
        }
        emitResidentEvent(.init(type: "result", id: request.id, text: text,
                                model: model, message: nil))
    } catch {
        emitResidentEvent(.init(type: "error", id: request.id, text: nil, model: nil,
                                message: error.localizedDescription))
    }
}
