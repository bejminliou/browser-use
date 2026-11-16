// swift-tools-version: 6.0
import Foundation
import PackageDescription

// The SwiftPM target and product are deliberately constant ("App") so that
// renaming the app touches exactly one place: APP_NAME in the root justfile.
//
// The app is the single owner of the in-process Rust hub: it statically links
// `libbrowser_use.a` and talks to it through the canonical `CBrowserUse`
// C-ABI header at `../rust/include/browser_use.h` (never a copy).
//
// Cargo profile + target directory:
//   * BROWSER_USE_CARGO_PROFILE selects the cargo profile (default "release";
//     `Scripts/build-app.sh` sets it to the matching CONFIG).
//   * The cargo target directory is derived from Context.packageDirectory, so
//     `swift build` works from any current working directory.
let packageDirectory = Context.packageDirectory
let repositoryRoot = URL(fileURLWithPath: packageDirectory).deletingLastPathComponent()

let cargoProfile = ProcessInfo.processInfo.environment["BROWSER_USE_CARGO_PROFILE"] ?? "release"
// Cargo's `dev` profile writes to target/debug; every other profile writes to a
// directory named after itself. Mirror that so `-L` finds the staticlib.
let cargoProfileDirectory: String
switch cargoProfile {
case "dev", "debug": cargoProfileDirectory = "debug"
case "release": cargoProfileDirectory = "release"
default: cargoProfileDirectory = cargoProfile
}
let cargoLinkDirectory = repositoryRoot
    .appendingPathComponent("target")
    .appendingPathComponent(cargoProfileDirectory)
    .path

let package = Package(
    name: "BrowserUseMenuBar",
    platforms: [
        // `.defaultLaunchBehavior(.suppressed)` (used so the main window does not
        // open at launch) requires macOS 15.
        .macOS(.v15)
    ],
    products: [
        .executable(name: "App", targets: ["App"])
    ],
    targets: [
        // The canonical Rust C-ABI header, exposed as a Clang module. The
        // modulemap walks back to `../rust/include/browser_use.h`, so the header
        // exists exactly once in the repository.
        .systemLibrary(
            name: "CBrowserUse",
            path: "Sources/CBrowserUse"
        ),
        .executableTarget(
            name: "App",
            dependencies: ["CBrowserUse"],
            path: "Sources/App",
            swiftSettings: [
                // Swift 5 language mode keeps the AppKit/SwiftUI glue free of
                // strict-concurrency ceremony. Change to `.v6` when you want it.
                .swiftLanguageMode(.v5)
            ],
            linkerSettings: [
                .unsafeFlags(["-L", cargoLinkDirectory]),
                .linkedLibrary("browser_use"),
                // Rust's staticlib leaves system-library resolution to us. These
                // are the frameworks the Rust core (tokio/hyper/rand) needs on
                // macOS; harmless if unused.
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
            ]
        )
    ]
)
