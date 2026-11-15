// Draws the Zone app icon for the Mac: the serif Z and purple dot of the iPhone icon, on
// the rounded dark square macOS icons sit on. Run it to regenerate the icon set:
//
//   swift mac/scripts/make-icon.swift mac/Zone/Assets.xcassets/AppIcon.appiconset
import AppKit

let folder = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")

func draw(side: CGFloat) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: Int(side), pixelsHigh: Int(side), bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    rep.size = NSSize(width: side, height: side)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let context = NSGraphicsContext.current!.cgContext
    let unit = side / 1024

    // Apple's grid for a Mac icon: an 824 pt rounded square in a 1024 pt canvas.
    let plate = CGRect(x: 100 * unit, y: 100 * unit, width: 824 * unit, height: 824 * unit)
    let shape = NSBezierPath(roundedRect: plate, xRadius: 185.4 * unit, yRadius: 185.4 * unit)

    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -10 * unit), blur: 22 * unit, color: NSColor.black.withAlphaComponent(0.32).cgColor)
    NSColor(srgbRed: 0x2A / 255, green: 0x2A / 255, blue: 0x2A / 255, alpha: 1).setFill()
    shape.fill()
    context.restoreGState()

    // A hairline of light along the top edge, so the plate reads on a dark Dock.
    context.saveGState()
    shape.addClip()
    NSColor.white.withAlphaComponent(0.07).setStroke()
    let rim = NSBezierPath(roundedRect: plate.insetBy(dx: 1.5 * unit, dy: 1.5 * unit), xRadius: 184 * unit, yRadius: 184 * unit)
    rim.lineWidth = 3 * unit
    rim.stroke()
    context.restoreGState()

    // The Z, in New York.
    let size = 430 * unit
    let base = NSFont.systemFont(ofSize: size, weight: .semibold)
    let font = base.fontDescriptor.withDesign(.serif).flatMap { NSFont(descriptor: $0, size: size) } ?? base
    let cream = NSColor(srgbRed: 0xF1 / 255, green: 0xE9 / 255, blue: 0xD2 / 255, alpha: 1)
    let letter = NSAttributedString(string: "Z", attributes: [.font: font, .foregroundColor: cream])
    let letterSize = letter.size()
    let dot = 118 * unit
    let gap = 14 * unit
    let total = letterSize.width + gap + dot
    let left = (side - total) / 2
    let baseline = (side - font.capHeight) / 2
    letter.draw(at: NSPoint(x: left, y: baseline + font.descender))

    // The dot sits on the baseline, as a full stop would.
    NSColor(srgbRed: 0x9B / 255, green: 0x7D / 255, blue: 0xFF / 255, alpha: 1).setFill()
    NSBezierPath(ovalIn: CGRect(x: left + letterSize.width + gap, y: baseline, width: dot, height: dot)).fill()

    NSGraphicsContext.restoreGraphicsState()
    return rep
}

let sizes: [(points: Int, scale: Int)] = [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)]
var images: [[String: String]] = []
try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
for (points, scale) in sizes {
    let name = "icon-\(points)@\(scale)x.png"
    let rep = draw(side: CGFloat(points * scale))
    try rep.representation(using: .png, properties: [:])!.write(to: folder.appendingPathComponent(name))
    images.append(["filename": name, "idiom": "mac", "scale": "\(scale)x", "size": "\(points)x\(points)"])
}
let contents: [String: Any] = ["images": images, "info": ["author": "xcode", "version": 1]]
try JSONSerialization.data(withJSONObject: contents, options: [.prettyPrinted, .sortedKeys])
    .write(to: folder.appendingPathComponent("Contents.json"))
print("Wrote \(sizes.count) icons to \(folder.path)")
