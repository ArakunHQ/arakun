// swift-tools-version: 6.0
// The account client and the protocol the iOS app speaks with Arakun through
// the relay: the same as core/src/cloud.rs, phone/relay.js and phone/e2e.js.
// Kept apart from the app so `swift test` checks it on a Mac.
import PackageDescription

let package = Package(
    name: "ArakunKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "ArakunKit", targets: ["ArakunKit"])],
    targets: [
        .target(name: "ArakunKit"),
        .testTarget(name: "ArakunKitTests", dependencies: ["ArakunKit"]),
    ]
)
