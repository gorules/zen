// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "ZenUniffiTests",
    platforms: [
        .macOS(.v13)
    ],
    targets: [
        .systemLibrary(
            name: "zen_uniffiFFI",
            path: "Sources/zen_uniffiFFI"
        ),
        .target(
            name: "ZenUniffi",
            dependencies: ["zen_uniffiFFI"],
            path: "Sources/ZenUniffi",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .testTarget(
            name: "ZenEngineTests",
            dependencies: ["ZenUniffi"],
            path: "Tests/ZenEngineTests",
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation")
            ]
        )
    ]
)
