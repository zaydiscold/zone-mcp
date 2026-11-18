import AppKit
import SwiftUI
import ZoneKit

/// The first-run window: five steps, each one skippable, and the place the app comes back
/// to when the person opens the setup guide again from the menu.
struct WelcomeView: View {
    let model: AppModel
    /// Called when the person finishes or skips: the window closes.
    var onDone: (_ finished: Bool) -> Void
    @Environment(\.colorScheme) private var scheme
    @State private var step: Int
    @State private var forward = true
    @State private var startAtLogin = true
    @State private var addCommand = false
    @State private var copyClips = false

    static let size = CGSize(width: 820, height: 590)
    static let appStore = "https://apps.apple.com/app/id6830955448"
    private static let steps = 5

    init(model: AppModel, onDone: @escaping (_ finished: Bool) -> Void) {
        self.model = model
        self.onDone = onDone
        _step = State(initialValue: min(max(model.state.welcomeStep, 0), Self.steps - 1))
    }

    var body: some View {
        VStack(spacing: 0) {
            ZStack {
                switch step {
                case 0: WelcomeStep().transition(slide)
                case 1: PhoneStep(model: model).transition(slide)
                case 2: PairStep(model: model).transition(slide)
                case 3: AgentsStep(model: model).transition(slide)
                default: DoneStep(model: model, startAtLogin: $startAtLogin, addCommand: $addCommand, copyClips: $copyClips).transition(slide)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .clipped()
            Hairline()
            bar
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Zone.paper(scheme))
        .ignoresSafeArea()
        .tint(Zone.accentText(scheme))
        .onAppear {
            startAtLogin = model.loginItemOn || !model.state.welcomeDone
            addCommand = model.commandLinked
            copyClips = model.clipboardCopyOn
        }
    }

    private var slide: AnyTransition {
        .asymmetric(
            insertion: .offset(x: forward ? 40 : -40).combined(with: .opacity),
            removal: .offset(x: forward ? -40 : 40).combined(with: .opacity)
        )
    }

    // MARK: The bar at the bottom

    private var bar: some View {
        HStack(spacing: 12) {
            HStack(spacing: 6) {
                ForEach(0..<Self.steps, id: \.self) { index in
                    Capsule()
                        .fill(index == step ? Zone.accentGraphic(scheme) : Zone.ink(scheme).opacity(index < step ? 0.45 : 0.18))
                        .frame(width: index == step ? 22 : 7, height: 7)
                }
            }
            .animation(Zone.settle, value: step)
            .accessibilityElement()
            .accessibilityLabel("Step \(step + 1) of \(Self.steps)")

            Spacer()

            if step < Self.steps - 1 {
                Button("Skip setup") {
                    model.state.welcomeStep = step
                    model.finishWelcome()
                    onDone(false)
                }
                .buttonStyle(ZoneLinkButtonStyle())
                .help("You can open this guide again from the menu bar, under Settings.")
                .padding(.trailing, 6)
            }
            if step > 0 {
                Button("Back") { go(to: step - 1) }
                    .buttonStyle(ZoneQuietButtonStyle(height: 36))
            }
            Button(primaryTitle) { advance() }
                .buttonStyle(ZonePrimaryButtonStyle(height: 36))
                .keyboardShortcut(.defaultAction)
        }
        .padding(.horizontal, 28)
        .frame(height: 68)
    }

    private var primaryTitle: String {
        switch step {
        case 0: return "Get started"
        case 1: return "I have Zone"
        case 2:
            if case .paired = model.pairing { return "Continue" }
            return "Not now"
        case 3: return "Continue"
        default: return "Done"
        }
    }

    private func go(to next: Int) {
        forward = next > step
        withAnimation(Zone.settle) {
            step = next
        }
        model.state.welcomeStep = next
    }

    private func advance() {
        guard step == Self.steps - 1 else {
            go(to: step + 1)
            return
        }
        if startAtLogin != model.loginItemOn {
            model.setLoginItem(startAtLogin)
        }
        if addCommand, !model.commandLinked {
            model.installCommand()
        }
        if copyClips != model.clipboardCopyOn {
            model.setClipboardCopy(copyClips)
        }
        model.finishWelcome()
        onDone(true)
    }
}

// MARK: Step 1: Welcome

private struct WelcomeStep: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            Wordmark(size: 104)
            FlowingWave(color: Zone.accentGraphic(scheme), energy: 1, samples: 96)
                .frame(height: 64)
                .padding(.top, 6)
            VStack(spacing: 10) {
                Text("Everything you say into Zone, on this Mac.")
                    .font(.zoneSerif(27))
                    .foregroundStyle(Zone.ink(scheme))
                Text("Dictations, voice notes and meeting transcripts from your iPhone arrive here seconds after you make them. Read them, search them, and let your AI agents use them.")
                    .font(.system(size: 14))
                    .foregroundStyle(Zone.muted(scheme))
                    .multilineTextAlignment(.center)
                    .lineSpacing(3)
                    .frame(maxWidth: 560)
            }
            .padding(.top, 22)
            HStack(spacing: 12) {
                point("lock", "End to end encrypted")
                point("person.crop.circle.badge.xmark", "No account, no server")
                point("sparkles", "Works with 20 AI apps")
            }
            .padding(.top, 28)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 40)
    }

    private func point(_ symbol: String, _ text: String) -> some View {
        HStack(spacing: 7) {
            Image(systemName: symbol)
                .font(.system(size: 11.5, weight: .medium))
                .foregroundStyle(Zone.accentText(scheme))
            Text(text)
                .font(.system(size: 12.5, weight: .medium))
                .foregroundStyle(Zone.ink(scheme))
        }
        .padding(.horizontal, 13)
        .frame(height: 32)
        .background(Zone.card(scheme), in: Capsule())
        .overlay(Capsule().strokeBorder(Zone.rule(scheme), lineWidth: 1))
    }
}

/// The title and the sentence under it at the top of a step.
private struct StepHeading: View {
    let eyebrow: String
    let title: String
    let detail: String
    var alignment: HorizontalAlignment = .leading
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(alignment: alignment, spacing: 8) {
            Text(eyebrow)
                .font(.system(size: 11.5, weight: .semibold))
                .foregroundStyle(Zone.accentText(scheme))
            Text(title)
                .font(.zoneSerif(32))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            Text(detail)
                .font(.system(size: 14))
                .foregroundStyle(Zone.muted(scheme))
                .lineSpacing(3)
                .multilineTextAlignment(alignment == .center ? .center : .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

// MARK: Step 2: The iPhone app

private struct PhoneStep: View {
    let model: AppModel
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(alignment: .center, spacing: 52) {
            VStack(spacing: 14) {
                QRCodeView(text: WelcomeView.appStore, size: 224)
                HStack(spacing: 6) {
                    Image(systemName: "camera")
                        .font(.system(size: 11, weight: .medium))
                    Text("Point your iPhone camera here")
                        .font(.system(size: 12))
                }
                .foregroundStyle(Zone.muted(scheme))
            }
            VStack(alignment: .leading, spacing: 20) {
                StepHeading(
                    eyebrow: "Step 1 of 3",
                    title: "Get Zone on your iPhone",
                    detail: "Zone is a free dictation keyboard and app. Everything it hears is turned into text on the phone itself."
                )
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "info.circle")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Zone.accentText(scheme))
                        .padding(.top, 1)
                    Text("Pairing with a computer arrives with Zone's next update, under Settings > Computers. Until then, the backup folder works with the Zone on the App Store today: choose it under Settings in the menu bar.")
                        .font(.system(size: 12.5))
                        .foregroundStyle(Zone.ink(scheme))
                        .lineSpacing(2.5)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .zoneCard(padding: 14)
                Button {
                    if let url = URL(string: WelcomeView.appStore) { model.open(url) }
                } label: {
                    HStack(spacing: 5) {
                        Text("Open the App Store page")
                        Image(systemName: "arrow.up.right")
                            .font(.system(size: 10, weight: .semibold))
                    }
                }
                .buttonStyle(ZoneLinkButtonStyle(size: 13))
            }
            .frame(maxWidth: 360, alignment: .leading)
        }
        .padding(.horizontal, 48)
    }
}

// MARK: Step 3: Pair

private struct PairStep: View {
    let model: AppModel

    var body: some View {
        VStack(spacing: 30) {
            StepHeading(
                eyebrow: "Step 2 of 3",
                title: "Pair your iPhone",
                detail: "One scan. After that your phone finds this Mac by itself, on any network.",
                alignment: .center
            )
            PairPanel(model: model, wide: true)
                .frame(minHeight: 250)
        }
        .padding(.horizontal, 48)
    }
}

// MARK: Step 4: Agents

private struct AgentsStep: View {
    let model: AppModel
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(alignment: .center, spacing: 44) {
            VStack(alignment: .leading, spacing: 18) {
                StepHeading(
                    eyebrow: "Step 3 of 3",
                    title: "Connect your AI agents",
                    detail: "Then ask one \"what did I say about the lease on Wednesday?\" and it answers from your own words."
                )
                if model.agents.contains(where: { $0.installed && $0.standing != .connected }) {
                    Button(model.busyAgents.isEmpty ? "Connect all" : "Connecting") {
                        Task { await model.connectAllAgents() }
                    }
                    .buttonStyle(ZoneQuietButtonStyle(height: 34))
                    .disabled(!model.busyAgents.isEmpty)
                }
            }
            .frame(width: 290, alignment: .leading)
            AgentsPanel(model: model, wide: true)
                .frame(width: 360)
        }
        .padding(.horizontal, 48)
        .task { await model.loadAgents() }
    }
}

// MARK: Step 5: Done

private struct DoneStep: View {
    let model: AppModel
    @Binding var startAtLogin: Bool
    @Binding var addCommand: Bool
    @Binding var copyClips: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: model.hasClips ? 20 : 26) {
            MenuBarPicture()
            StepHeading(
                eyebrow: "All set",
                title: "Zone lives in your menu bar",
                detail: "Click the Z at the top of your screen for your last meeting, your notes and the latest things you said.",
                alignment: .center
            )
            .frame(maxWidth: 520)
            VStack(spacing: 0) {
                Toggle(isOn: $startAtLogin) {
                    option("power", "Open Zone when I log in", "So your iPhone can sync whenever this Mac is on.")
                }
                .toggleStyle(ZoneToggleStyle())
                .padding(.vertical, 10)
                .padding(.horizontal, 14)
                Hairline().padding(.leading, 44)
                Toggle(isOn: $addCommand) {
                    option("terminal", "Add the zone command to the terminal", commandDetail)
                }
                .toggleStyle(ZoneToggleStyle())
                .disabled(commandLocked)
                .padding(.vertical, 10)
                .padding(.horizontal, 14)
                if model.hasClips {
                    Hairline().padding(.leading, 44)
                    Toggle(isOn: $copyClips) {
                        option("doc.on.clipboard", "Copy what I send from my iPhone", "Text you send to this Mac from Zone on the iPhone goes on the clipboard, ready to paste.")
                    }
                    .toggleStyle(ZoneToggleStyle())
                    .disabled(model.clipboardCopy?.available == false)
                    .padding(.vertical, 10)
                    .padding(.horizontal, 14)
                }
            }
            .zoneCard(padding: 0)
            .frame(width: 470)
        }
        .padding(.horizontal, 48)
    }

    private var commandLocked: Bool {
        switch model.commandLink {
        case .absent: return false
        default: return true
        }
    }

    private var commandDetail: String {
        switch model.commandLink {
        case .linked: return "Already there."
        case let .foreign(existing):
            return "Another zone is at \(Formats.tilde(existing.path, home: model.env.home.path)). Change it under Settings."
        case let .absent(_, needsAdmin):
            return needsAdmin ? "For zone list, zone search and the rest. Asks for your password." : "For zone list, zone search and the rest."
        case nil: return "For zone list, zone search and the rest."
        }
    }

    private func option(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Zone.muted(scheme))
                .frame(width: 18, height: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Zone.ink(scheme))
                Text(detail)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.muted(scheme))
            }
        }
    }
}

/// A drawing of the right end of the menu bar, with the Z picked out.
private struct MenuBarPicture: View {
    @Environment(\.colorScheme) private var scheme
    @State private var lifted = false

    var body: some View {
        VStack(spacing: 6) {
            HStack(spacing: 15) {
                Spacer(minLength: 0)
                Image(nsImage: MenuBarIcon.image(.idle))
                    .renderingMode(.template)
                    .foregroundStyle(Zone.onInk(scheme))
                    .padding(.horizontal, 7)
                    .frame(height: 22)
                    .background(Zone.accentGraphic(scheme), in: RoundedRectangle(cornerRadius: 5, style: .continuous))
                Group {
                    Image(systemName: "battery.75percent")
                    Image(systemName: "wifi")
                    Image(systemName: "magnifyingglass")
                    Image(systemName: "switch.2")
                }
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Zone.ink(scheme).opacity(0.75))
                Text("Sun 9:41 AM")
                    .font(.system(size: 12.5, weight: .medium).monospacedDigit())
                    .foregroundStyle(Zone.ink(scheme).opacity(0.75))
            }
            .padding(.horizontal, 14)
            .frame(width: 400, height: 34)
            .background(Zone.card(scheme), in: UnevenRoundedRectangle(bottomLeadingRadius: 12, bottomTrailingRadius: 12, style: .continuous))
            .overlay(UnevenRoundedRectangle(bottomLeadingRadius: 12, bottomTrailingRadius: 12, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
            .mask(
                LinearGradient(
                    stops: [.init(color: .clear, location: 0), .init(color: .black, location: 0.3)],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            )
            Image(systemName: "arrow.up")
                .font(.system(size: 15, weight: .bold))
                .foregroundStyle(Zone.accentGraphic(scheme))
                .offset(x: 9, y: lifted ? -2 : 3)
                .animation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true), value: lifted)
                .onAppear { lifted = true }
        }
        .accessibilityHidden(true)
    }
}
