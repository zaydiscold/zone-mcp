import SwiftUI
import ZoneKit

// The two pieces used both in the menu bar popover and in the welcome window: pairing a
// phone, and the list of AI agents.

/// Shows the pair code, waits for the phone, and then shows what arrives.
struct PairPanel: View {
    let model: AppModel
    /// The popover is narrow: the code sits above the words. The welcome window is wide:
    /// they sit side by side.
    var wide = false
    /// True where "Pair" always means a new code (the popover). The guide keeps showing a
    /// phone it just paired when the person steps back and forth.
    var startsFresh = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Group {
            switch model.pairing {
            case .idle, .loading:
                waitingForCode
            case let .waiting(code):
                layout(code: code)
            case let .paired(name, _):
                paired(name: name)
            case .expired:
                message(
                    symbol: "clock.badge.xmark",
                    title: "That code ran out",
                    body: "A pair code works once and lasts ten minutes.",
                    action: "Show a new code"
                )
            case let .failed(reason):
                message(symbol: "exclamationmark.triangle", title: "Could not make a code", body: reason, action: "Try again")
            }
        }
        .animation(Zone.settle, value: model.pairing)
        .task {
            if startsFresh, case .waiting = model.pairing {} else if startsFresh { model.stopPairing() }
            if model.pairing == .idle { await model.startPairing() }
        }
    }

    private var waitingForCode: some View {
        VStack(spacing: 12) {
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Zone.card(scheme))
                .frame(width: qrSize, height: qrSize)
                .overlay(ProgressView().controlSize(.small))
            Text("Making a code")
                .font(.system(size: 12))
                .foregroundStyle(Zone.muted(scheme))
        }
        .frame(maxWidth: .infinity)
    }

    private var qrSize: CGFloat { wide ? 236 : 208 }

    @ViewBuilder
    private func layout(code: PairCode) -> some View {
        if wide {
            HStack(alignment: .center, spacing: 36) {
                QRCodeView(text: code.url, size: qrSize)
                VStack(alignment: .leading, spacing: 16) {
                    steps
                    waitingLine(code: code)
                }
                .frame(maxWidth: 300, alignment: .leading)
            }
        } else {
            VStack(spacing: 16) {
                QRCodeView(text: code.url, size: qrSize)
                waitingLine(code: code)
                steps
            }
        }
    }

    private var steps: some View {
        VStack(alignment: .leading, spacing: 9) {
            step(1, "Open Zone on your iPhone")
            step(2, "Go to Settings, then Computers")
            step(3, "Tap Add a computer and scan this code")
        }
    }

    private func step(_ number: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text("\(number)")
                .font(.system(size: 11, weight: .semibold, design: .serif).monospacedDigit())
                .foregroundStyle(Zone.accentText(scheme))
                .frame(width: 18, height: 18)
                .overlay(Circle().strokeBorder(Zone.accentText(scheme).opacity(0.5), lineWidth: 1))
            Text(text)
                .font(.system(size: wide ? 13.5 : 12.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func waitingLine(code: PairCode) -> some View {
        HStack(spacing: 6) {
            StatusDot(color: Zone.accentGraphic(scheme), pulses: true, size: 8)
            VStack(alignment: .leading, spacing: 1) {
                Text("Waiting for your iPhone")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Zone.ink(scheme))
                Text("This code works once, for \(Formats.countdown(code.expiry.timeIntervalSince(model.now)))")
                    .font(.system(size: 11.5).monospacedDigit())
                    .foregroundStyle(Zone.muted(scheme))
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
        .background(Zone.field(scheme), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
        .frame(maxWidth: wide ? .infinity : 260)
    }

    private func paired(name: String) -> some View {
        let received = model.pairedPhone?.itemsReceived ?? 0
        return VStack(spacing: wide ? 18 : 14) {
            ZStack {
                Circle()
                    .fill(Zone.accentGraphic(scheme).opacity(0.16))
                    .frame(width: 72, height: 72)
                Circle()
                    .strokeBorder(Zone.accentGraphic(scheme), lineWidth: 1.5)
                    .frame(width: 72, height: 72)
                Image(systemName: "checkmark")
                    .font(.system(size: 26, weight: .bold))
                    .foregroundStyle(Zone.accentGraphic(scheme))
            }
            .transition(.scale(scale: 0.6).combined(with: .opacity))
            VStack(spacing: 4) {
                Text("\(name) is paired")
                    .font(.zoneSerif(wide ? 26 : 20))
                    .foregroundStyle(Zone.ink(scheme))
                Text(pairedLine(received: received))
                    .font(.system(size: 12.5))
                    .foregroundStyle(Zone.muted(scheme))
                    .multilineTextAlignment(.center)
            }
            HStack(alignment: .firstTextBaseline, spacing: 28) {
                figure(received, "from \(name)")
                figure(model.status?.items.total ?? 0, "in your library")
            }
            .padding(.vertical, 12)
            .padding(.horizontal, 22)
            .zoneCard(padding: 0)
            FlowingWave(color: Zone.accentGraphic(scheme), energy: model.isSyncing ? 1 : 0.25)
                .frame(height: 30)
                .frame(maxWidth: wide ? 360 : 260)
        }
        .frame(maxWidth: .infinity)
    }

    private func pairedLine(received: Int) -> String {
        if received == 0 { return "Its library arrives the next time Zone opens on the phone." }
        if model.isSyncing { return "Its library is arriving now." }
        return "Its library is here. New things follow seconds after you say them."
    }

    private func figure(_ value: Int, _ label: String) -> some View {
        VStack(spacing: 2) {
            CountingNumber(value: value, size: wide ? 34 : 26)
            Text(label)
                .font(.system(size: 11))
                .foregroundStyle(Zone.muted(scheme))
        }
        .frame(minWidth: 96)
    }

    private func message(symbol: String, title: String, body: String, action: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 26, weight: .regular))
                .foregroundStyle(Zone.muted(scheme))
            Text(title)
                .font(.zoneSerif(18))
                .foregroundStyle(Zone.ink(scheme))
            Text(body)
                .font(.system(size: 12.5))
                .foregroundStyle(Zone.muted(scheme))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Button(action) {
                Task { await model.startPairing() }
            }
            .buttonStyle(ZoneQuietButtonStyle(height: 32))
            .padding(.top, 4)
        }
        .frame(maxWidth: 300)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 18)
    }
}

/// Every AI client on this Mac with a switch, and one sentence on what a connected one can read.
struct AgentsPanel: View {
    let model: AppModel
    var wide = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !model.agentsLoaded {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Looking for AI apps on this Mac")
                        .font(.system(size: 12.5))
                        .foregroundStyle(Zone.muted(scheme))
                }
                .frame(maxWidth: .infinity, minHeight: 80)
            } else if model.agents.isEmpty {
                VStack(spacing: 6) {
                    Image(systemName: "sparkles")
                        .font(.system(size: 22))
                        .foregroundStyle(Zone.muted(scheme))
                    Text("No AI apps found on this Mac yet")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Zone.ink(scheme))
                    Text("Install Claude, Codex, Cursor or another one, then come back here from the menu.")
                        .font(.system(size: 12))
                        .foregroundStyle(Zone.muted(scheme))
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 18)
            } else {
                list
            }
            if let error = model.agentError {
                Text(error)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.alarm)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "eye")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                    .padding(.top, 1)
                Text("A connected agent can read your Zone library and mark items as handled. It cannot change or delete your words. To show one less, click the line under its name.")
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var list: some View {
        let rows = model.agents
        return ScrollView {
            VStack(spacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, agent in
                    AgentRow(model: model, agent: agent)
                    if index < rows.count - 1 {
                        Hairline().padding(.leading, 44)
                    }
                }
            }
        }
        .scrollIndicators(.never)
        .frame(maxHeight: wide ? 410 : 380)
        .fixedSize(horizontal: false, vertical: rows.count <= (wide ? 5 : 6))
        .zoneCard(padding: 4, shadow: wide)
    }
}

private struct AgentRow: View {
    let model: AppModel
    let agent: Agent
    @Environment(\.colorScheme) private var scheme
    /// True while the choices of what this agent may read are showing.
    @State private var choosing = false

    var body: some View {
        let busy = model.busyAgents.contains(agent.id)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                AgentBadge(id: agent.id)
                VStack(alignment: .leading, spacing: 1) {
                    Text(agent.title)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Zone.ink(scheme))
                    if agent.canLimitView, !busy {
                        Button {
                            withAnimation(Zone.motion) { choosing.toggle() }
                        } label: {
                            HStack(spacing: 3) {
                                Text(agent.note)
                                Image(systemName: "chevron.down")
                                    .font(.system(size: 7.5, weight: .bold))
                                    .rotationEffect(.degrees(choosing ? 180 : 0))
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(ZoneLinkButtonStyle(size: 11))
                        .help("Choose what \(agent.title) may read")
                        .accessibilityLabel("\(agent.note). Choose what \(agent.title) may read")
                    } else {
                        Text(busy ? "Working" : agent.note)
                            .font(.system(size: 11))
                            .foregroundStyle(Zone.muted(scheme))
                    }
                }
                Spacer(minLength: 6)
                if case .otherCopy = agent.standing, !busy, !agent.viewsDiffer {
                    Button("Use this app's") {
                        Task { await model.setAgent(agent, connected: true) }
                    }
                    .buttonStyle(ZonePillButtonStyle())
                    .help("This agent starts a zone that was installed from the command line. Point it at the one this app keeps up to date. What it may read stays the same.")
                }
                if case let .partly(places, total) = agent.standing, !busy, !agent.viewsDiffer {
                    Button("Connect all \(total)") {
                        Task { await model.setAgent(agent, connected: true) }
                    }
                    .buttonStyle(ZonePillButtonStyle())
                    .help("Connected in \(places) of \(total) profiles.")
                }
                if busy {
                    ProgressView().controlSize(.small).frame(width: 34)
                } else {
                    Toggle("", isOn: Binding(
                        get: { agent.isOn },
                        set: { on in Task { await model.setAgent(agent, connected: on) } }
                    ))
                    .toggleStyle(ZoneToggleStyle())
                    .labelsHidden()
                    .fixedSize()
                    .accessibilityLabel("Connect \(agent.title)")
                }
            }
            if choosing, agent.canLimitView, !busy {
                choices
                    .padding(.leading, 36)
                    .padding(.top, 6)
                    .transition(.opacity)
            }
        }
        .padding(.vertical, 7)
        .padding(.horizontal, 8)
        .onChange(of: agent.isOn) { _, on in
            if !on { choosing = false }
        }
    }

    /// The views on offer, with the one in force marked. A view set from the command line
    /// that is none of them is listed too, so it is seen and not lost by accident.
    private var choices: some View {
        var views = AgentView.choices
        if let current = agent.view, !views.contains(current) { views.append(current) }
        return VStack(alignment: .leading, spacing: 1) {
            ForEach(Array(views.enumerated()), id: \.offset) { _, view in
                let chosen = agent.view == view
                Button {
                    withAnimation(Zone.motion) { choosing = false }
                    Task { await model.setAgentView(agent, view) }
                } label: {
                    HoverRow(radius: 7) { _ in
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Image(systemName: chosen ? "checkmark.circle.fill" : "circle")
                                .font(.system(size: 11.5, weight: .medium))
                                .foregroundStyle(chosen ? Zone.accentGraphic(scheme) : Zone.faint(scheme))
                            VStack(alignment: .leading, spacing: 0) {
                                Text(view.title)
                                    .font(.system(size: 12, weight: chosen ? .semibold : .regular))
                                    .foregroundStyle(Zone.ink(scheme))
                                Text(view.detail)
                                    .font(.system(size: 10.5))
                                    .foregroundStyle(Zone.muted(scheme))
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.vertical, 4)
                        .padding(.horizontal, 6)
                        .contentShape(Rectangle())
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(view.title). \(view.detail)")
                .accessibilityAddTraits(chosen ? .isSelected : [])
            }
            if agent.viewsDiffer {
                Text("Its profiles read different things now. Choosing one sets it for all of them.")
                    .font(.system(size: 10.5))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 6)
                    .padding(.top, 2)
            }
        }
    }
}

/// A small square with a symbol for the kind of client: a terminal for command line
/// agents, a window for desktop apps, brackets for code editors. No logos are shipped.
struct AgentBadge: View {
    let id: String
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Image(systemName: Self.symbol(for: id))
            .font(.system(size: 11.5, weight: .semibold))
            .foregroundStyle(Zone.ink(scheme))
            .frame(width: 26, height: 26)
            .background(Zone.field(scheme), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 7, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
            .accessibilityHidden(true)
    }

    static func symbol(for id: String) -> String {
        switch id {
        case "claude-desktop", "lm-studio": return "macwindow"
        case "cursor", "vscode", "zed", "windsurf", "kiro", "cline", "roo": return "chevron.left.forwardslash.chevron.right"
        default: return "terminal"
        }
    }
}

/// One thing the person said, as a row: what kind, the words, when. Click to copy.
struct ItemRow: View {
    let model: AppModel
    let item: ZoneItem
    var lines = 2
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let copied = model.copiedID == item.id
        Button {
            model.copy(item)
        } label: {
            HoverRow { hovering in
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: Self.symbol(for: item.kind))
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Zone.muted(scheme))
                        .frame(width: 18, height: 18)
                        .padding(.top, 1)
                    VStack(alignment: .leading, spacing: 2) {
                        Group {
                            if model.state.hideText {
                                Text(item.preview)
                                    .redacted(reason: .placeholder)
                            } else {
                                Text(item.preview)
                            }
                        }
                        .font(.system(size: 12.5))
                        .foregroundStyle(Zone.ink(scheme))
                        .lineLimit(lines)
                        .multilineTextAlignment(.leading)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        HStack(spacing: 5) {
                            Text(Self.label(for: item))
                            Text("·")
                            Text(Formats.when(item.created, now: model.now))
                            if copied {
                                Text("·")
                                Text("Copied").foregroundStyle(Zone.accentText(scheme))
                            }
                        }
                        .font(.system(size: 10.5).monospacedDigit())
                        .foregroundStyle(Zone.muted(scheme))
                    }
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(copied ? Zone.accentText(scheme) : Zone.muted(scheme))
                        .opacity(hovering || copied ? 1 : 0)
                        .padding(.top, 2)
                }
                .padding(.vertical, 6)
                .padding(.horizontal, 8)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .help(model.state.hideText ? "Click to copy (text is hidden)" : "Click to copy")
        .accessibilityLabel(model.state.hideText ? "\(Self.label(for: item)), hidden" : item.preview)
    }

    static func symbol(for kind: String) -> String {
        switch kind {
        case "note": return "note.text"
        case "meeting": return "person.2"
        default: return "waveform"
        }
    }

    static func label(for item: ZoneItem) -> String {
        switch item.kind {
        case "note": return "Note"
        case "meeting": return "Meeting"
        default: return "Dictation"
        }
    }
}

/// The eye that hides what was said, for sharing a screen.
struct HideTextButton: View {
    let model: AppModel
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button {
            model.state.hideText.toggle()
        } label: {
            HStack(spacing: 5) {
                Image(systemName: model.state.hideText ? "eye.slash" : "eye")
                    .font(.system(size: 10.5, weight: .medium))
                Text(model.state.hideText ? "Text hidden" : "Hide text")
                    .font(.system(size: 11, weight: .medium))
            }
            .foregroundStyle(model.state.hideText ? Zone.onInk(scheme) : Zone.ink(scheme))
            .padding(.horizontal, 9)
            .frame(height: 22)
            .background(model.state.hideText ? Zone.ink(scheme) : Zone.card(scheme), in: Capsule())
            .overlay(Capsule().strokeBorder(model.state.hideText ? .clear : Zone.rule(scheme), lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help("Hide what you said, for when you share your screen")
        .accessibilityLabel(model.state.hideText ? "Show text" : "Hide text")
    }
}
