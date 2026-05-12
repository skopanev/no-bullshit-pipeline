// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "apple-speech-sidecar",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "apple-speech-sidecar",
            path: "Sources"
        ),
    ]
)
