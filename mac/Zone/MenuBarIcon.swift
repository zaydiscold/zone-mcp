import AppKit

/// What the menu bar icon is saying.
enum MenuBarState: Equatable {
    /// Running and reachable: the Z with its dot.
    case idle
    /// Something is arriving: the dot pulses.
    case syncing
    /// zone is not running: a faded Z with a hollow dot.
    case stopped
    /// Something needs the person: the dot becomes an exclamation mark.
    case problem
}

/// Draws the menu bar icon: the serif Z and dot from the app icon, as a template image so
/// macOS colors it for a light or dark menu bar.
enum MenuBarIcon {
    static let size = NSSize(width: 24, height: 18)

    /// `phase` runs 0 to 1 and only matters while syncing.
    static func image(_ state: MenuBarState, phase: Double = 0) -> NSImage {
        let image = NSImage(size: size, flipped: false) { rect in
            draw(state, phase: phase, in: rect)
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = description(state)
        return image
    }

    static func description(_ state: MenuBarState) -> String {
        switch state {
        case .idle: return "Zone is running"
        case .syncing: return "Zone is receiving"
        case .stopped: return "Zone is not running"
        case .problem: return "Zone needs attention"
        }
    }

    private static func draw(_ state: MenuBarState, phase: Double, in rect: NSRect) {
        let faded = state == .stopped
        let ink = NSColor.black.withAlphaComponent(faded ? 0.45 : 1)

        // The Z, in New York, sitting a little left of center to leave room for the dot.
        let base = NSFont.systemFont(ofSize: 16.5, weight: .semibold)
        let font = base.fontDescriptor.withDesign(.serif).flatMap { NSFont(descriptor: $0, size: 16.5) } ?? base
        let letter = NSAttributedString(string: "Z", attributes: [.font: font, .foregroundColor: ink])
        let letterSize = letter.size()
        let baselineY = (rect.height - font.capHeight) / 2
        let letterX: CGFloat = 2
        letter.draw(at: NSPoint(x: letterX, y: baselineY + font.descender))

        // The dot sits on the baseline to the right of the Z, as in the app icon.
        let radius: CGFloat = 2.6
        let center = NSPoint(x: min(letterX + letterSize.width + radius + 1, rect.width - radius - 0.8), y: baselineY + radius)

        switch state {
        case .idle:
            ink.setFill()
            NSBezierPath(ovalIn: circle(center, radius)).fill()
        case .syncing:
            // The dot breathes and a ring leaves it, like the live ring in the app.
            let breathe = 0.75 + 0.25 * cos(phase * 2 * .pi)
            ink.setFill()
            NSBezierPath(ovalIn: circle(center, radius * breathe)).fill()
            let ringRadius = radius + 0.5 + 2.4 * phase
            let ring = NSBezierPath(ovalIn: circle(center, ringRadius))
            ring.lineWidth = 1
            NSColor.black.withAlphaComponent(0.85 * (1 - phase)).setStroke()
            ring.stroke()
        case .stopped:
            let ring = NSBezierPath(ovalIn: circle(center, radius - 0.6))
            ring.lineWidth = 1.2
            ink.setStroke()
            ring.stroke()
        case .problem:
            let mark = NSAttributedString(string: "!", attributes: [
                .font: NSFont.systemFont(ofSize: 12.5, weight: .heavy),
                .foregroundColor: NSColor.black,
            ])
            mark.draw(at: NSPoint(x: center.x - mark.size().width / 2, y: baselineY - 1.5))
        }
    }

    private static func circle(_ center: NSPoint, _ radius: CGFloat) -> NSRect {
        NSRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)
    }
}
