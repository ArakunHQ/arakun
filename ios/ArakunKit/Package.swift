// swift-tools-version: 6.0
// The protocol the iOS app speaks with Arakun through the relay: the same as
// phone/relay.js and phone/e2e.js, so the computer and the relay need nothing
// new. Kept apart from the app so `swift test` checks it on a Mac.
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
