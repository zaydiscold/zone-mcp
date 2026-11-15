import AppKit
import SwiftUI

/// Zone's look on the Mac, taken from the iPhone app: warm paper and ink, one purple accent,
/// a serif for the wordmark and headlines. Flat surfaces with 1 pt rules and hard-edged
/// shadows. No blur, no gradients on surfaces. Dark is the default face.
enum Zone {
    static let accent = Color(hex: 0x9B7DFF)
    /// Errors and warnings only.
    static let alarm = Color(hex: 0xFF6B6B)
    /// One duration for every state change.
    static let motion: Animation = .easeInOut(duration: 0.2)
    /// zayd.wtf's ease-out-quint.
    static let easeOutQuint = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.22, y: 1), endControlPoint: UnitPoint(x: 0.36, y: 1))
    static let settle: Animation = .timingCurve(0.22, 1, 0.36, 1, duration: 0.45)

    static func paper(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(hex: 0x2A2A2A) : Color(hex: 0xF1E9D2)
    }
    static func card(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(hex: 0x333333) : Color(hex: 0xF7F2E4)
    }
    static func ink(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(hex: 0xF1E9D2) : Color(hex: 0x333333)
    }
    /// The label color on an ink surface (primary buttons).
    static func onInk(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(hex: 0x2A2A2A) : Color(hex: 0xF1E9D2)
    }
    static func muted(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(hex: 0xC9C1AB) : Color(hex: 0x555555)
    }
    static func faint(_ scheme: ColorScheme) -> Color {
        ink(scheme).opacity(scheme == .dark ? 0.42 : 0.5)
    }
    static func rule(_ scheme: ColorScheme) -> Color {
        ink(scheme).opacity(scheme == .dark ? 0.16 : 0.18)
    }
    static func field(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(hex: 0x262626) : Color(hex: 0xF6EFDB)
    }
    /// Purple as text. The accent reads 2.6:1 on cream, so light paper gets a deeper tone.
    static func accentText(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? accent : Color(hex: 0x6A4BDB)
    }
    /// Purple as a shape (the wave, dots, progress).
    static func accentGraphic(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? accent : Color(hex: 0x7A5AF0)
    }
}

extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 255) / 255,
            green: Double((hex >> 8) & 255) / 255,
            blue: Double(hex & 255) / 255,
            opacity: 1
        )
    }
}

extension Font {
    static func zoneSerif(_ size: CGFloat, weight: Font.Weight = .semibold) -> Font {
        .system(size: size, weight: weight, design: .serif)
    }
    /// Small label above a block.
    static let zoneEyebrow = Font.system(size: 11, weight: .medium)
}

// MARK: Surfaces

/// A quiet inset surface with a 1 pt rule and a hard-edged shadow.
struct ZoneCardModifier: ViewModifier {
    @Environment(\.colorScheme) private var scheme
    var padding: CGFloat = 14
    var radius: CGFloat = 12
    var shadow = true

    func body(content: Content) -> some View {
        content
            .padding(padding)
            .background {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(Zone.card(scheme))
                    .shadow(color: .black.opacity(shadow ? (scheme == .dark ? 0.5 : 0.16) : 0), radius: 0, x: 2, y: 3)
            }
            .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
    }
}

extension View {
    func zoneCard(padding: CGFloat = 14, radius: CGFloat = 12, shadow: Bool = true) -> some View {
        modifier(ZoneCardModifier(padding: padding, radius: radius, shadow: shadow))
    }
}

/// A 1 pt rule between sections.
struct Hairline: View {
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Rectangle().fill(Zone.rule(scheme)).frame(height: 1)
    }
}

/// A small label that introduces a block. Sentence case.
struct Eyebrow: View {
    @Environment(\.colorScheme) private var scheme
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.zoneEyebrow)
            .foregroundStyle(Zone.muted(scheme))
    }
}

/// The wordmark: a serif "zone" with the purple dot from the app icon.
struct Wordmark: View {
    @Environment(\.colorScheme) private var scheme
    var size: CGFloat = 22

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: size * 0.06) {
            Text("zone")
                .font(.zoneSerif(size))
                .foregroundStyle(Zone.ink(scheme))
            Circle()
                .fill(Zone.accentGraphic(scheme))
                .frame(width: size * 0.2, height: size * 0.2)
        }
        .accessibilityElement()
        .accessibilityLabel("Zone")
    }
}

// MARK: Buttons

/// Ink capsule, paper label. The one primary action on a screen.
struct ZonePrimaryButtonStyle: ButtonStyle {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.isEnabled) private var isEnabled
    var height: CGFloat = 40

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(Zone.onInk(scheme))
            .padding(.horizontal, 20)
            .frame(minHeight: height)
            .background(Zone.ink(scheme).opacity(isEnabled ? 1 : 0.4), in: Capsule())
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .animation(Zone.motion, value: configuration.isPressed)
            .contentShape(Capsule())
    }
}

/// Outlined capsule for secondary actions.
struct ZoneQuietButtonStyle: ButtonStyle {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.isEnabled) private var isEnabled
    var height: CGFloat = 40

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .medium))
            .foregroundStyle(Zone.ink(scheme).opacity(isEnabled ? 1 : 0.45))
            .padding(.horizontal, 18)
            .frame(minHeight: height)
            .background(Zone.card(scheme).opacity(configuration.isPressed ? 0.6 : 1), in: Capsule())
            .overlay(Capsule().strokeBorder(Zone.rule(scheme), lineWidth: 1))
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .animation(Zone.motion, value: configuration.isPressed)
            .contentShape(Capsule())
    }
}

/// Small inline capsule for row actions.
struct ZonePillButtonStyle: ButtonStyle {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.isEnabled) private var isEnabled
    var filled = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11.5, weight: filled ? .semibold : .medium))
            .foregroundStyle((filled ? Zone.onInk(scheme) : Zone.ink(scheme)).opacity(isEnabled ? 1 : 0.45))
            .padding(.horizontal, 11)
            .frame(minHeight: 26)
            .background((filled ? Zone.ink(scheme) : Zone.card(scheme)).opacity(configuration.isPressed ? 0.6 : 1), in: Capsule())
            .overlay(Capsule().strokeBorder(filled ? .clear : Zone.rule(scheme), lineWidth: 1))
            .contentShape(Capsule())
    }
}

/// A plain text button in the accent tone.
struct ZoneLinkButtonStyle: ButtonStyle {
    @Environment(\.colorScheme) private var scheme
    var size: CGFloat = 12.5

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: size, weight: .medium))
            .foregroundStyle(Zone.accentText(scheme).opacity(configuration.isPressed ? 0.6 : 1))
            .contentShape(Rectangle())
    }
}

/// A row that lights up under the pointer, for lists inside the popover.
struct HoverRow<Content: View>: View {
    @Environment(\.colorScheme) private var scheme
    var radius: CGFloat = 8
    @ViewBuilder var content: (Bool) -> Content
    @State private var hovering = false

    var body: some View {
        content(hovering)
            .background(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(Zone.ink(scheme).opacity(hovering ? 0.07 : 0))
            )
            .onHover { hovering = $0 }
            .animation(.easeOut(duration: 0.1), value: hovering)
    }
}

/// The switch used everywhere: the accent when on.
struct ZoneToggleStyle: ToggleStyle {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 10) {
            configuration.label
            Spacer(minLength: 8)
            ZStack(alignment: configuration.isOn ? .trailing : .leading) {
                Capsule()
                    .fill(configuration.isOn ? Zone.accentGraphic(scheme) : Zone.ink(scheme).opacity(0.18))
                    .frame(width: 34, height: 20)
                Circle()
                    .fill(configuration.isOn ? Color.white : Zone.ink(scheme).opacity(0.85))
                    .frame(width: 16, height: 16)
                    .padding(2)
            }
            .opacity(isEnabled ? 1 : 0.5)
            .animation(Zone.motion, value: configuration.isOn)
            .onTapGesture { configuration.isOn.toggle() }
            .accessibilityAddTraits(.isButton)
        }
    }
}

// MARK: State light

/// A state light. Purple and pulsing when on, a hollow grey ring when off, so the state
/// never rests on color alone. The pulse is zayd.wtf's live ring: a thin ring grows and
/// fades over two seconds. Reduce Motion holds it still.
struct StatusDot: View {
    let color: Color
    var pulses = false
    var hollow = false
    var size: CGFloat = 8
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack {
            if pulses {
                if reduceMotion {
                    Circle().strokeBorder(color.opacity(0.35), lineWidth: 1.5).frame(width: size * 2.2, height: size * 2.2)
                } else {
                    Circle()
                        .strokeBorder(color, lineWidth: 1.5)
                        .frame(width: size * 1.6, height: size * 1.6)
                        .keyframeAnimator(initialValue: PulseRing(), repeating: true) { ring, value in
                            ring.scaleEffect(value.scale).opacity(value.opacity)
                        } keyframes: { _ in
                            KeyframeTrack(\.scale) {
                                LinearKeyframe(1.8, duration: 2, timingCurve: Zone.easeOutQuint)
                            }
                            KeyframeTrack(\.opacity) {
                                LinearKeyframe(0, duration: 2, timingCurve: Zone.easeOutQuint)
                            }
                        }
                }
            }
            if hollow {
                Circle().strokeBorder(color, lineWidth: max(1.5, size / 6)).frame(width: size, height: size)
            } else {
                Circle().fill(color).frame(width: size, height: size)
            }
        }
        .frame(width: size * 2.6, height: size * 2.6)
        .accessibilityHidden(true)
    }
}

private struct PulseRing {
    var scale: Double = 1
    var opacity: Double = 0.8
}

/// A number that counts up to its value instead of jumping, in tabular serif digits.
struct CountingNumber: View {
    let value: Int
    var size: CGFloat = 28
    var color: Color?
    @State private var shown: Double = 0
    @State private var started = false

    var body: some View {
        CountingText(value: shown, size: size, color: color)
            .onAppear {
                // The first value is shown as it is: nothing has "arrived" yet.
                shown = Double(value)
                started = true
            }
            .onChange(of: value) { _, new in
                guard started else { return }
                withAnimation(.timingCurve(0.22, 1, 0.36, 1, duration: 1.1)) {
                    shown = Double(new)
                }
            }
            .accessibilityLabel(Text("\(value)"))
    }
}

/// SwiftUI hands this view every value on the way from the old number to the new one.
private struct CountingText: View, Animatable {
    @Environment(\.colorScheme) private var scheme
    var value: Double
    let size: CGFloat
    let color: Color?

    var animatableData: Double {
        get { value }
        set { value = newValue }
    }

    var body: some View {
        Text(Self.format(value))
            .font(.system(size: size, weight: .semibold, design: .serif).monospacedDigit())
            .foregroundStyle(color ?? Zone.ink(scheme))
            .lineLimit(1)
            .minimumScaleFactor(0.5)
    }

    private static func format(_ value: Double) -> String {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        formatter.locale = Locale(identifier: "en_US")
        return formatter.string(from: NSNumber(value: Int(value.rounded()))) ?? "\(Int(value))"
    }
}
