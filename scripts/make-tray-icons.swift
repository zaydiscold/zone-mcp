// Draws the icons of Zone for Windows: the serif Z and its dot, the mark of the iPhone app
// and of the Mac's menu bar icon, as .ico files that hold each taskbar size as its own
// drawing (16, 20, 24, 32, 40 and 48 pixels: 100% to 300% scaling). Run on a Mac:
//
//   swift scripts/make-tray-icons.swift assets/windows
//
// It writes ten tray icons, tray-<face>-<taskbar>.ico (faces: running, receiving, receiving2,
// stopped, attention; taskbars: dark, light), and zone.ico, the program's own icon (the dark
// plate of the Mac app icon). Add `--preview FILE.png` to also write one sheet of every
// drawing, enlarged, on both taskbar colors, to look at.
import AppKit

let arguments = CommandLine.arguments
let folder = URL(fileURLWithPath: arguments.count > 1 ? arguments[1] : ".")
let previewPath: String? = arguments.firstIndex(of: "--preview").flatMap { arguments.count > $0 + 1 ? arguments[$0 + 1] : nil }

enum Face: String, CaseIterable { case running, receiving, receiving2, stopped, attention }
enum Taskbar: String, CaseIterable { case dark, light }

func color(_ hex: Int, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
}

func bitmap(_ side: Int, _ draw: (CGFloat) -> Void) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: side, pixelsHigh: side, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    rep.size = NSSize(width: side, height: side)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current!.cgContext.setShouldSmoothFonts(false)
    draw(CGFloat(side))
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

func serif(_ size: CGFloat, _ weight: NSFont.Weight) -> NSFont {
    let base = NSFont.systemFont(ofSize: size, weight: weight)
    return base.fontDescriptor.withDesign(.serif).flatMap { NSFont(descriptor: $0, size: size) } ?? base
}

func circle(_ x: CGFloat, _ y: CGFloat, _ r: CGFloat) -> NSBezierPath {
    NSBezierPath(ovalIn: NSRect(x: x - r, y: y - r, width: r * 2, height: r * 2))
}

/// One tray icon. The Z fills the height, a little left of center; the dot sits on the
/// baseline at its right, as a full stop would. Everything is placed on whole pixels, so the
/// small sizes stay sharp.
func tray(_ face: Face, _ taskbar: Taskbar, _ side: Int) -> NSBitmapImageRep {
    bitmap(side) { s in
        let faded = face == .stopped
        let ink = (taskbar == .dark ? color(0xFFFFFF) : color(0x1A1A1A)).withAlphaComponent(faded ? 0.55 : 1)
        let accent = taskbar == .dark ? color(0xA98FFF) : color(0x6A4BDB)
        let warn = taskbar == .dark ? color(0xFFB938) : color(0xB86A00)

        // The dot's size first, in whole pixels: it is what makes the mark Zone's.
        let dot = max(4, (s * 0.25).rounded())
        let radius = dot / 2
        let margin = (s / 16).rounded(.down)
        let bottom = max(1, (s * 0.09).rounded())
        let gap = max(1, (s * 0.04).rounded())

        // The Z: as large as leaves room for the gap and the dot.
        let room = s - margin - gap - dot
        var size = s * 1.05
        var font = serif(size, .bold)
        var letter = NSAttributedString(string: "Z", attributes: [.font: font, .foregroundColor: ink])
        // The letter's own ink is narrower than its advance: measure what is drawn.
        func inkBounds() -> NSRect { letter.boundingRect(with: NSSize(width: 1000, height: 1000), options: [.usesDeviceMetrics]) }
        while (inkBounds().width > room || font.capHeight > s - bottom - margin) && size > 4 {
            size -= 0.25
            font = serif(size, .bold)
            letter = NSAttributedString(string: "Z", attributes: [.font: font, .foregroundColor: ink])
        }
        let bounds = inkBounds()
        let baseline = bottom
        let letterX = (margin - bounds.minX).rounded()
        letter.draw(at: NSPoint(x: letterX, y: baseline + font.descender))

        let cx = s - radius - (side >= 24 ? 1 : 0)
        let cy = baseline + radius
        switch face {
        case .running:
            accent.setFill()
            circle(cx, cy, radius).fill()
        case .receiving, .receiving2:
            // The dot with a ring leaving it. The second drawing has the ring further out.
            // Both are moved in from the corner by the ring's reach, so nothing is cut off.
            let far = face == .receiving2
            let stroke = max(1, s / 18)
            let core = radius * 0.85
            let reach = core + max(1.25, s * 0.08)
            let (rx, ry) = (s - reach - stroke / 2, reach + stroke / 2)
            accent.setFill()
            circle(rx, ry, far ? core * 0.7 : core * 0.9).fill()
            let ring = circle(rx, ry, far ? reach : core + max(0.75, s * 0.04))
            ring.lineWidth = stroke
            accent.withAlphaComponent(far ? 0.65 : 0.95).setStroke()
            ring.stroke()
        case .stopped:
            let width = max(1, (s / 14).rounded())
            let ring = circle(cx, cy, radius - width / 2)
            ring.lineWidth = width
            ink.setStroke()
            ring.stroke()
        case .attention:
            // An exclamation mark where the dot is: a bar and a point, drawn as shapes so the
            // smallest size is still two clean marks.
            let w = max(2, (s * 0.14).rounded())
            let x = (s - w - (side >= 24 ? 1 : 0)).rounded()
            warn.setFill()
            NSBezierPath(roundedRect: NSRect(x: x, y: baseline, width: w, height: w), xRadius: w * 0.3, yRadius: w * 0.3).fill()
            let barBottom = baseline + w + max(1, (s * 0.06).rounded())
            let barTop = max(barBottom + w, (s * 0.72).rounded())
            NSBezierPath(roundedRect: NSRect(x: x, y: barBottom, width: w, height: barTop - barBottom), xRadius: w * 0.3, yRadius: w * 0.3).fill()
        }
    }
}

/// The program's icon: the dark plate, cream Z and purple dot of the Mac app icon, without
/// the Mac's margin and shadow (Windows icons fill their square).
func plate(_ side: Int) -> NSBitmapImageRep {
    bitmap(side) { s in
        let inset = s >= 32 ? s * 0.03 : 0
        let rect = NSRect(x: inset, y: inset, width: s - inset * 2, height: s - inset * 2)
        color(0x2A2A2A).setFill()
        NSBezierPath(roundedRect: rect, xRadius: s * 0.2, yRadius: s * 0.2).fill()

        let dot = max(3, (s * 0.15).rounded())
        let gap = max(1, (s * 0.03).rounded())
        var size = s * 0.74
        var font = serif(size, .semibold)
        var letter = NSAttributedString(string: "Z", attributes: [.font: font, .foregroundColor: color(0xF1E9D2)])
        func inkBounds() -> NSRect { letter.boundingRect(with: NSSize(width: 1000, height: 1000), options: [.usesDeviceMetrics]) }
        while inkBounds().width + gap + dot > s * 0.74 && size > 4 {
            size -= 0.25
            font = serif(size, .semibold)
            letter = NSAttributedString(string: "Z", attributes: [.font: font, .foregroundColor: color(0xF1E9D2)])
        }
        let bounds = inkBounds()
        let total = bounds.width + gap + dot
        let left = ((s - total) / 2).rounded()
        let baseline = ((s - font.capHeight) / 2).rounded()
        letter.draw(at: NSPoint(x: left - bounds.minX, y: baseline + font.descender))
        color(0x9B7DFF).setFill()
        circle(left + bounds.width + gap + dot / 2, baseline + dot / 2, dot / 2).fill()
    }
}

/// Writes an .ico file: a list of pictures, each stored as a PNG.
func writeIcon(_ pictures: [NSBitmapImageRep], to url: URL) throws {
    let images = pictures.map { $0.representation(using: .png, properties: [:])! }
    var data = Data()
    func put16(_ value: Int) { data.append(contentsOf: [UInt8(value & 0xFF), UInt8((value >> 8) & 0xFF)]) }
    func put32(_ value: Int) { put16(value & 0xFFFF); put16((value >> 16) & 0xFFFF) }
    put16(0); put16(1); put16(pictures.count)
    var offset = 6 + 16 * pictures.count
    for (picture, image) in zip(pictures, images) {
        let side = picture.pixelsWide
        data.append(contentsOf: [UInt8(side >= 256 ? 0 : side), UInt8(side >= 256 ? 0 : side), 0, 0])
        put16(1); put16(32)
        put32(image.count); put32(offset)
        offset += image.count
    }
    for image in images { data.append(image) }
    try data.write(to: url)
}

let traySizes = [16, 20, 24, 32, 40, 48]
try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
for taskbar in Taskbar.allCases {
    for face in Face.allCases {
        try writeIcon(traySizes.map { tray(face, taskbar, $0) }, to: folder.appendingPathComponent("tray-\(face.rawValue)-\(taskbar.rawValue).ico"))
    }
}
try writeIcon([16, 20, 24, 32, 40, 48, 64, 256].map(plate), to: folder.appendingPathComponent("zone.ico"))

// A sheet to look at: every face at 16, 20, 24 and 32 pixels, eight times enlarged with hard
// pixel edges, and at its real size beside it, on a dark and a light taskbar.
if let previewPath {
    let shown = [16, 20, 24, 32]
    let scale = 8
    let cell = 32 * scale + 24
    let width = 20 + Face.allCases.count * cell + cell
    let rowHeight = 32 * scale + 20
    let height = 20 + Taskbar.allCases.count * shown.count * rowHeight
    let sheet = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: sheet)
    NSGraphicsContext.current!.imageInterpolation = .none
    var y = height - 10
    for taskbar in Taskbar.allCases {
        let back = taskbar == .dark ? color(0x1C1C1C) : color(0xEEEEEE)
        back.setFill()
        NSRect(x: 0, y: y - shown.count * rowHeight - 10, width: width, height: shown.count * rowHeight + 10).fill()
        for side in shown {
            y -= rowHeight
            var x = 20
            for face in Face.allCases {
                let picture = tray(face, taskbar, side)
                picture.draw(in: NSRect(x: x, y: y, width: side * scale, height: side * scale), from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: false, hints: [.interpolation: NSImageInterpolation.none.rawValue])
                picture.draw(in: NSRect(x: x + side * scale + 6, y: y, width: side, height: side), from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: false, hints: [.interpolation: NSImageInterpolation.none.rawValue])
                x += cell
            }
            let app = plate(side)
            app.draw(in: NSRect(x: x, y: y, width: side * scale, height: side * scale), from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: false, hints: [.interpolation: NSImageInterpolation.none.rawValue])
        }
    }
    NSGraphicsContext.restoreGraphicsState()
    try sheet.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: previewPath))
}
print("Wrote \(Taskbar.allCases.count * Face.allCases.count) tray icons and zone.ico to \(folder.path)")
