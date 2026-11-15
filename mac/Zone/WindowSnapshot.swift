import AppKit
import ImageIO
import UniformTypeIdentifiers

/// Pictures of the app's own windows, for the screenshots in the README. Only a test run
/// (ZONE_MAC_SANDBOX) ever calls this.
///
/// An app may photograph its own windows without the Screen Recording permission. The
/// call that does it, CGWindowListCreateImage, is no longer in the SDK's headers, so it is
/// looked up by name when needed. If it is ever gone, the window's views are drawn instead
/// (which loses the window's shadow, nothing else).
enum WindowSnapshot {
    private typealias CreateImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?

    static func capture(_ window: NSWindow, to file: URL, shadow: Bool = true) -> Bool {
        guard let image = serverImage(of: window, shadow: shadow) ?? drawnImage(of: window) else { return false }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        guard let destination = CGImageDestinationCreateWithURL(file as CFURL, UTType.png.identifier as CFString, 1, nil) else { return false }
        CGImageDestinationAddImage(destination, image, nil)
        return CGImageDestinationFinalize(destination)
    }

    /// The window as the window server shows it.
    private static func serverImage(of window: NSWindow, shadow: Bool) -> CGImage? {
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") else { return nil }
        let create = unsafeBitCast(symbol, to: CreateImage.self)
        let onlyThisWindow: UInt32 = 1 << 3
        let bestResolution: UInt32 = 1 << 3
        let withoutShadow: UInt32 = 1 << 0
        let options = shadow ? bestResolution : bestResolution | withoutShadow
        return create(.null, onlyThisWindow, UInt32(window.windowNumber), options)?.takeRetainedValue()
    }

    /// The window's views, drawn into a picture.
    private static func drawnImage(of window: NSWindow) -> CGImage? {
        guard let view = window.contentView?.superview ?? window.contentView,
              let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)
        else { return nil }
        view.cacheDisplay(in: view.bounds, to: rep)
        return rep.cgImage
    }
}
