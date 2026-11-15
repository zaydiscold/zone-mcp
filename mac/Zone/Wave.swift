import SwiftUI

/// The voice as one wave flowing right to left, as in the iPhone app: the newest sound
/// enters at the right edge and glides left as it ages, fading out. A smooth closed curve,
/// mirrored about a center line, pale at the center and deeper toward its reaches.
///
/// Nobody is speaking into the Mac, so the wave here is drawn from a made-up voice: phrases
/// with pauses between them. `energy` scales it (0 lies flat as a hairline). Reduce Motion
/// holds it still.
struct FlowingWave: View {
    var color: Color
    /// 0 to 1: how loud the made-up voice is.
    var energy: Double = 1
    /// How many samples span the width.
    var samples = 56
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 60, paused: reduceMotion || energy <= 0)) { timeline in
            let time = reduceMotion ? 12.0 : timeline.date.timeIntervalSinceReferenceDate
            Canvas { context, size in
                draw(in: &context, size: size, time: time)
            }
        }
        .mask(
            // Older sound fades out toward the left, and the newest edge softens.
            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0),
                    .init(color: .black.opacity(0.55), location: 0.3),
                    .init(color: .black, location: 0.6),
                    .init(color: .black, location: 0.93),
                    .init(color: .clear, location: 1),
                ],
                startPoint: .leading,
                endPoint: .trailing
            )
        )
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }

    /// The loudness of the made-up voice at a moment: syllables inside phrases, with pauses.
    static func level(at t: Double) -> Double {
        let phrase = max(0, sin(t * 0.83) * 0.75 + sin(t * 0.31 + 1.7) * 0.45)
        let syllable = 0.55 + 0.45 * sin(t * 6.1 + sin(t * 1.3) * 1.5)
        let texture = 0.8 + 0.2 * sin(t * 17.3 + 0.6)
        return min(1, max(0, phrase * syllable * texture))
    }

    private func draw(in context: inout GraphicsContext, size: CGSize, time: Double) {
        guard size.width > 0, size.height > 0 else { return }
        let mid = size.height / 2
        let amplitude = size.height / 2 - 1
        let count = max(samples, 4)
        // Each sample is a twentieth of a second of sound; the wave scrolls one step per sample.
        let interval = 0.05
        let phase = (time / interval).truncatingRemainder(dividingBy: 1)
        let newest = (time / interval).rounded(.down) * interval
        let step = size.width / CGFloat(count - 2)

        var top: [CGPoint] = []
        top.reserveCapacity(count)
        for index in 0..<count {
            let age = Double(count - 1 - index) * interval
            let value = Self.level(at: newest - age) * energy
            let x = (CGFloat(index) - 0.5 - CGFloat(phase)) * step
            // The newest sample eases in over its first step instead of popping up.
            let entry = index == count - 1 ? max(phase, 0.15) : 1
            let height = max(0.75, CGFloat(pow(value, 0.62)) * amplitude * CGFloat(entry))
            top.append(CGPoint(x: x, y: mid - height))
        }
        let bottom = top.reversed().map { CGPoint(x: $0.x, y: 2 * mid - $0.y) }

        var body = Path()
        Self.append(top, to: &body, moving: true)
        Self.append(bottom, to: &body, moving: false)
        body.closeSubpath()

        let gradient = Gradient(stops: [
            .init(color: color, location: 0),
            .init(color: color.opacity(0.85), location: 0.32),
            .init(color: color.opacity(0.18), location: 0.5),
            .init(color: color.opacity(0.85), location: 0.68),
            .init(color: color, location: 1),
        ])
        context.fill(body, with: .linearGradient(gradient, startPoint: CGPoint(x: 0, y: 0), endPoint: CGPoint(x: 0, y: size.height)))

        var crest = Path()
        Self.append(top, to: &crest, moving: true)
        Self.append(bottom, to: &crest, moving: true)
        context.stroke(crest, with: .color(color.opacity(0.9)), style: StrokeStyle(lineWidth: 1.25, lineCap: .round, lineJoin: .round))
    }

    /// Catmull-Rom through the points as cubic Béziers, so the curve passes through every
    /// sample. With `moving` false the run continues the current subpath with a line first.
    private static func append(_ points: [CGPoint], to path: inout Path, moving: Bool) {
        guard let first = points.first else { return }
        if moving { path.move(to: first) } else { path.addLine(to: first) }
        guard points.count > 2 else {
            points.dropFirst().forEach { path.addLine(to: $0) }
            return
        }
        for i in 0..<(points.count - 1) {
            let p0 = points[max(i - 1, 0)], p1 = points[i]
            let p2 = points[i + 1], p3 = points[min(i + 2, points.count - 1)]
            let c1 = CGPoint(x: p1.x + (p2.x - p0.x) / 6, y: p1.y + (p2.y - p0.y) / 6)
            let c2 = CGPoint(x: p2.x - (p3.x - p1.x) / 6, y: p2.y - (p3.y - p1.y) / 6)
            path.addCurve(to: p2, control1: c1, control2: c2)
        }
    }
}
