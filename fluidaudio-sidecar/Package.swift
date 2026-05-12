// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "fluidaudio-sidecar",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", from: "0.14.5"),
    ],
    targets: [
        .executableTarget(
            name: "fluidaudio-sidecar",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio"),
            ],
            path: "Sources"
        ),
    ]
)
