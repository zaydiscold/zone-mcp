// swift-tools-version: 5.9
import PackageDescription

// Everything in Zone for Mac that is not a view: reading what `zone` prints, deciding who
// runs the background service, watching for new arrivals. Kept apart so it can be tested
// with `swift test`, without building the app.
let package = Package(
    name: "ZoneKit",
    platforms: [.macOS(.v14)],
    products: [.library(name: "ZoneKit", targets: ["ZoneKit"])],
    targets: [
        .target(name: "ZoneKit"),
        .testTarget(name: "ZoneKitTests", dependencies: ["ZoneKit"]),
    ]
)
