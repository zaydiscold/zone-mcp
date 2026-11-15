// Draws the picture behind the icons in Zone.dmg: warm paper, the wordmark, and an arrow
// from the app to the Applications folder. Run it to regenerate both sizes:
//
//   swift mac/scripts/make-dmg-background.swift mac/dmg
//
// The icon positions here must match mac/dmg/settings.py.
import AppKit

let folder = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")
let size = NSSize(width: 640, height: 420)
let paper = NSColor(srgbRed: 0xF1 / 255, green: 0xE9 / 255, blue: 0xD2 / 255, alpha: 1)
let ink = NSColor(srgbRed: 0x33 / 255, green: 0x33 / 255, blue: 0x33 / 255, alpha: 1)
let muted = NSColor(srgbRed: 0x55 / 255, green: 0x55 / 255, blue: 0x55 / 255, alpha: 1)
let accent = NSColor(srgbRed: 0x7A / 255, green: 0x5A / 255, blue: 0xF0 / 255, alpha: 1)

func serif(_ points: CGFloat, _ weight: NSFont.Weight = .semibold) -> NSFont {
    let base = NSFont.systemFont(ofSize: points, weight: weight)
    return base.fontDescriptor.withDesign(.serif).flatMap { NSFont(descriptor: $0, size: points) } ?? base
}

func centered(_ text: String, font: NSFont, color: NSColor, y: CGFloat) {
    let string = NSAttributedString(string: text, attributes: [.font: font, .foregroundColor: color])
    string.draw(at: NSPoint(x: (size.width - string.size().width) / 2, y: y))
}

func draw(scale: CGFloat) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale), bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    rep.size = size
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

    paper.setFill()
    NSRect(origin: .zero, size: size).fill()

    // The wordmark, top center: "zone" and its dot.
    let word = NSAttributedString(string: "zone", attributes: [.font: serif(34), .foregroundColor: ink])
    let wordSize = word.size()
    let dot: CGFloat = 7
    let left = (size.width - wordSize.width - dot - 2) / 2
    let baseline: CGFloat = 352
    word.draw(at: NSPoint(x: left, y: baseline + serif(34).descender))
    accent.setFill()
    NSBezierPath(ovalIn: NSRect(x: left + wordSize.width + 2, y: baseline, width: dot, height: dot)).fill()

    centered("Drag Zone to Applications", font: serif(17, .medium), color: ink, y: 312)

    // A rule under the heading and one above the footer.
    ink.withAlphaComponent(0.18).setFill()
    NSRect(x: 40, y: 296, width: size.width - 80, height: 1).fill()
    NSRect(x: 40, y: 62, width: size.width - 80, height: 1).fill()

    // The arrow between the two icons (which Finder draws at x 170 and x 470).
    let y: CGFloat = 215
    let arrow = NSBezierPath()
    arrow.move(to: NSPoint(x: 262, y: y))
    arrow.line(to: NSPoint(x: 372, y: y))
    arrow.lineWidth = 2.5
    arrow.lineCapStyle = .round
    accent.setStroke()
    arrow.stroke()
    let head = NSBezierPath()
    head.move(to: NSPoint(x: 362, y: y + 10))
    head.line(to: NSPoint(x: 376, y: y))
    head.line(to: NSPoint(x: 362, y: y - 10))
    head.lineWidth = 2.5
    head.lineCapStyle = .round
    head.lineJoinStyle = .round
    head.stroke()

    centered("Then open Zone. It lives in the menu bar, at the top of your screen.", font: .systemFont(ofSize: 12), color: muted, y: 30)

    NSGraphicsContext.restoreGraphicsState()
    return rep
}

try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
for (scale, name) in [(CGFloat(1), "background.png"), (CGFloat(2), "background@2x.png")] {
    try draw(scale: scale).representation(using: .png, properties: [:])!.write(to: folder.appendingPathComponent(name))
}
print("Wrote the disk image background to \(folder.path)")
