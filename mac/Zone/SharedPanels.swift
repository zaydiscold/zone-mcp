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

/// The AI apps on this Mac, each once however many profiles it has: whether zone is in
/// it (as `zone mcp status` says, place by place), what it may read, and a check that
/// starts zone the way that app would and asks it what an agent asks first.
struct AgentsPanel: View {
    let model: AppModel
    /// The popover's own, so a page keeps what was opened. The guide has none.
    var navigation: PopoverNavigation?
    var wide = false
    @State private var own = PopoverNavigation()
    @Environment(\.colorScheme) private var scheme

    private var state: PopoverNavigation { navigation ?? own }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
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
            } else if let notice = model.agentNotice {
                Text(notice)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
            }
            if model.agentsLoaded {
                zoneItself
            }
        }
    }

    private var list: some View {
        let sections = AgentSections(model.agents)
        return VStack(alignment: .leading, spacing: 6) {
            HStack {
                Eyebrow(sections.summary).padding(.leading, 6)
                Spacer()
                if sections.on.contains(where: \.isOn) {
                    Button("Test all") {
                        model.testZone()
                        for agent in sections.on where agent.isOn { model.test(agent) }
                    }
                    .buttonStyle(ZoneLinkButtonStyle(size: 11))
                    .help("Starts zone the way each app would and asks it for its tools")
                }
            }
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(Array(sections.on.enumerated()), id: \.element.id) { index, agent in
                        AgentRow(model: model, agent: agent, state: state)
                        if index < sections.on.count - 1 || !sections.off.isEmpty {
                            Hairline().padding(.leading, index < sections.on.count - 1 ? 44 : 0)
                        }
                    }
                    if !sections.off.isEmpty, sections.on.isEmpty {
                        // Nothing is connected yet: the apps are the list, with nothing to fold.
                        ForEach(Array(sections.off.enumerated()), id: \.element.id) { index, agent in
                            AgentRow(model: model, agent: agent, state: state)
                            if index < sections.off.count - 1 {
                                Hairline().padding(.leading, 44)
                            }
                        }
                    } else if !sections.off.isEmpty {
                        Button {
                            withAnimation(Zone.motion) { state.offAgentsOpen.toggle() }
                        } label: {
                            HStack(spacing: 6) {
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 8.5, weight: .bold))
                                    .rotationEffect(.degrees(state.offAgentsOpen ? 90 : 0))
                                    .frame(width: 26)
                                Text(sections.offTitle)
                                    .font(.system(size: 12, weight: .medium))
                                Spacer()
                            }
                            .foregroundStyle(Zone.muted(scheme))
                            .padding(.vertical, 8)
                            .padding(.horizontal, 8)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(sections.offTitle). \(state.offAgentsOpen ? "Hide them" : "Show them")")
                        if state.offAgentsOpen {
                            ForEach(sections.off) { agent in
                                Hairline().padding(.leading, 44)
                                AgentRow(model: model, agent: agent, state: state)
                            }
                        }
                    }
                }
            }
            .scrollIndicators(.automatic)
            .frame(maxHeight: min(wide ? 300 : 520, max(160, state.maxHeight - 250)))
            .fixedSize(horizontal: false, vertical: rowCount(sections) <= (wide ? 5 : 6) && state.openAgent == nil)
            .zoneCard(padding: 4, shadow: wide)
        }
    }

    private func rowCount(_ sections: AgentSections) -> Int {
        if sections.on.isEmpty { return sections.off.count }
        return sections.on.count + (sections.off.isEmpty ? 0 : 1) + (state.offAgentsOpen ? sections.off.count : 0)
    }

    /// The zone the apps are pointed at, with the same check and no app in between, and
    /// one sentence on what a connected agent can do.
    private var zoneItself: some View {
        let test = model.agentTests["zone"]
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                VStack(alignment: .leading, spacing: 1) {
                    Text("zone on this Mac")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Zone.ink(scheme))
                    Text(Formats.tilde(model.agentsZone ?? model.agentsBinary.path, home: model.env.home.path))
                        .font(.system(size: 10.5))
                        .foregroundStyle(Zone.muted(scheme))
                        .lineLimit(1)
                        .truncationMode(.head)
                    if let test {
                        TestLine(test: test)
                    }
                }
                Spacer(minLength: 6)
                Button("Test") { model.testZone() }
                    .buttonStyle(ZonePillButtonStyle())
                    .disabled(test == .running)
                    .help("Starts this zone as an MCP server and asks it for its tools")
            }
            .padding(.horizontal, 6)
            Text("A connected agent can read your library and mark items as handled. It cannot change or delete your words. To show one less, click the line under its name.")
                .font(.system(size: 11))
                .foregroundStyle(Zone.muted(scheme))
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 6)
        }
    }
}

/// What a check found, in one line: a tick and "Answers: 6 tools", or what went wrong.
struct TestLine: View {
    let test: AgentTest
    var text: String?
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            switch test {
            case .running:
                ProgressView().controlSize(.mini)
                Text("Asking")
            case let .done(outcome):
                Image(systemName: outcome.answered ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                    .font(.system(size: 9.5))
                Text(text ?? outcome.words)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .font(.system(size: 10.5, weight: .medium).monospacedDigit())
        .foregroundStyle(color)
    }

    private var color: Color {
        switch test {
        case .running: return Zone.muted(scheme)
        case let .done(outcome): return outcome.answered ? Zone.accentText(scheme) : Zone.alarm
        }
    }
}

struct AgentRow: View {
    let model: AppModel
    let agent: Agent
    let state: PopoverNavigation
    /// The Agents page's form: the app's own icon, a line that names a limit only when it
    /// differs from the other apps', and a menu instead of a switch. The guide keeps the switch.
    var calm = false
    @Environment(\.colorScheme) private var scheme
    /// True while the choices of what this agent may read are showing.
    @State private var choosing = false

    private var open: Bool { state.openAgent == agent.id }
    private var home: String { model.env.home.path }

    var body: some View {
        let busy = model.busyAgents.contains(agent.id)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                AgentBadge(id: agent.id, appIcon: calm)
                VStack(alignment: .leading, spacing: 1) {
                    Button {
                        withAnimation(Zone.motion) { state.openAgent = open ? nil : agent.id }
                    } label: {
                        HStack(spacing: 4) {
                            Text(agent.title)
                                .font(.system(size: 13, weight: .medium))
                                .foregroundStyle(Zone.ink(scheme))
                            if agent.isOn {
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 7.5, weight: .bold))
                                    .foregroundStyle(Zone.muted(scheme))
                                    .rotationEffect(.degrees(open ? 90 : 0))
                            }
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .allowsHitTesting(agent.isOn)
                    .help(agent.places.count > 1 ? "Show its \(agent.places.count) profiles" : "Show where its settings are")
                    if calm {
                        Text(busy ? "Working" : calmNote)
                            .font(.system(size: 11))
                            .foregroundStyle(Zone.muted(scheme))
                            .fixedSize(horizontal: false, vertical: true)
                    } else if agent.canLimitView, !busy {
                        Button {
                            withAnimation(Zone.motion) { choosing.toggle() }
                        } label: {
                            HStack(spacing: 3) {
                                Text(agent.note)
                                    .multilineTextAlignment(.leading)
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
                    if let summary = model.testSummary(agent), !open {
                        TestLine(test: summary.running ? .running : .done(summary.good ? .answers(.init(tools: [])) : .failed("")), text: summary.text)
                    }
                }
                Spacer(minLength: 6)
                if agent.isOn, !busy {
                    Button("Test") { model.test(agent) }
                        .buttonStyle(ZonePillButtonStyle())
                        .help("Starts zone the way \(agent.title) would, from its own settings, and asks it for its tools")
                }
                if busy {
                    ProgressView().controlSize(.small).frame(width: 34)
                } else if calm {
                    menu
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
            if !busy, !agent.viewsDiffer, needsFix {
                fix
                    .padding(.leading, 36)
                    .padding(.top, 6)
            }
            if choosing, agent.canLimitView, !busy {
                choices
                    .padding(.leading, 36)
                    .padding(.top, 6)
                    .transition(.opacity)
            }
            if open, agent.isOn {
                places
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

    /// Under the name on the Agents page: profiles, and what it reads only when that is not
    /// what the other apps read.
    private var calmNote: String {
        let profiles = agent.places.count > 1 ? "\(agent.connectedPlaces.count) of \(agent.places.count) profiles" : nil
        switch agent.standing {
        case .connected:
            var parts: [String] = []
            if let profiles { parts.append(agent.connectedPlaces.count == agent.places.count ? "\(agent.places.count) profiles" : profiles) }
            if agent.viewsDiffer {
                parts.append("its profiles read different things")
            } else if let view = agent.view, view != model.sharedView {
                parts.append("its own limit: \(view.words)")
            }
            if agent.readOnly { parts.append("cannot mark items") }
            let line = parts.joined(separator: ", ")
            return line.isEmpty ? "Connected" : line.prefix(1).uppercased() + line.dropFirst()
        default:
            return agent.note
        }
    }

    /// The Agents page's menu for one app: its profiles, what it reads, a check, disconnect.
    private var menu: some View {
        Menu {
            if agent.isOn {
                Button(open ? "Hide profiles" : (agent.places.count > 1 ? "Show its \(agent.places.count) profiles" : "Show where its settings are")) {
                    withAnimation(Zone.motion) { state.openAgent = open ? nil : agent.id }
                }
                if agent.canLimitView {
                    Menu("What it reads") {
                        let shared = model.sharedView
                        Button {
                            Task { await model.setAgentView(agent, shared) }
                        } label: {
                            if agent.view == shared, !agent.viewsDiffer { Image(systemName: "checkmark") }
                            Text("Same as the other apps")
                        }
                        Divider()
                        ForEach(Array(AgentView.choices.enumerated()), id: \.offset) { _, view in
                            Button {
                                Task { await model.setAgentView(agent, view) }
                            } label: {
                                if agent.view == view, view != shared { Image(systemName: "checkmark") }
                                Text("Only for this one: \(view.title.prefix(1).lowercased() + view.title.dropFirst())")
                            }
                        }
                    }
                }
                Button("Test") { model.test(agent) }
                Divider()
                Button("Disconnect \(agent.title)") { Task { await model.setAgent(agent, connected: false) } }
            } else {
                Button("Connect \(agent.title)") { Task { await model.setAgent(agent, connected: true) } }
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Zone.muted(scheme))
                .frame(width: 24, height: 22)
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Profiles, what it reads, disconnect")
        .accessibilityLabel("\(agent.title): more")
    }

    private var needsFix: Bool {
        if !agent.otherCopyPlaces.isEmpty { return true }
        if case .partly = agent.standing { return true }
        return false
    }

    /// An entry that starts another copy of zone, or a profile without zone: what that
    /// means in a sentence, and the one click that puts it right.
    private var fix: some View {
        let other = !agent.otherCopyPlaces.isEmpty
        return VStack(alignment: .leading, spacing: 6) {
            Text(other ? agent.otherCopyExplanation(home: home) : "zone is not in every profile of \(agent.title).")
                .font(.system(size: 11))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            Button(other ? "Use this one" : "Connect all \(agent.places.count)") {
                Task { await model.setAgent(agent, connected: true) }
            }
            .buttonStyle(ZonePillButtonStyle(filled: true))
            .help(other
                ? "Points \(agent.title) at the zone that runs on this Mac (zone mcp install \(agent.id)). " + (agent.otherCopyKeepsView ? "What it may read stays the same." : "It will be able to read everything until you limit it again.")
                : "Adds zone to the rest (zone mcp install \(agent.id)), with the same view.")
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Zone.accentGraphic(scheme).opacity(0.10), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }

    /// Every place this app keeps its settings in: its name, how zone stands there, what
    /// an agent there may read, and what the last check found.
    private var places: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(agent.places, id: \.path) { place in
                VStack(alignment: .leading, spacing: 1) {
                    HStack(alignment: .firstTextBaseline, spacing: 5) {
                        Image(systemName: place.hasZone ? "circle.fill" : "circle")
                            .font(.system(size: 5))
                            .foregroundStyle(place.hasZone ? Zone.accentGraphic(scheme) : Zone.faint(scheme))
                            .offset(y: -2)
                        Text(agent.places.count > 1 ? place.name : Formats.tilde(place.path, home: home))
                            .font(.system(size: 11.5, weight: .medium))
                            .foregroundStyle(Zone.ink(scheme))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer(minLength: 6)
                        if let test = model.agentTests[place.path], test == .running || test.outcome?.answered == true {
                            TestLine(test: test)
                        }
                    }
                    Text(([place.words(home: home)] + [place.limit(knowsClips: agent.knowsClips)].compactMap { $0 }).joined(separator: ", "))
                        .font(.system(size: 10.5))
                        .foregroundStyle(Zone.muted(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.leading, 10)
                    // What went wrong takes a line of its own: it can be a sentence.
                    if let test = model.agentTests[place.path], test.outcome?.answered == false {
                        TestLine(test: test)
                            .padding(.leading, 10)
                    }
                }
                .help(Formats.tilde(place.path, home: home))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
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
    /// Use the installed app's own icon when there is one.
    var appIcon = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if appIcon, let icon = AppIcons.icon(for: id) {
            Image(nsImage: icon)
                .resizable()
                .interpolation(.high)
                .frame(width: 26, height: 26)
                .accessibilityHidden(true)
        } else {
            symbolBadge
        }
    }

    private var symbolBadge: some View {
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

/// One thing the person said (or sent as a clip), as a row: what kind, the words, when.
/// Click to copy.
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
                                .foregroundStyle(item.kind == "clip" ? Zone.accentText(scheme) : Zone.muted(scheme))
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
        case "clip": return "doc.on.clipboard"
        default: return "waveform"
        }
    }

    static func label(for item: ZoneItem) -> String {
        switch item.kind {
        case "note": return "Note"
        case "meeting": return "Meeting"
        case "clip": return "Clip"
        default: return "Dictation"
        }
    }
}

/// The eye that hides what was said, for sharing a screen.
struct HideTextButton: View {
    let model: AppModel
    /// Only the eye, for the corner of a block.
    var compact = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if compact {
            Button {
                model.state.hideText.toggle()
            } label: {
                HoverRow(radius: 6) { _ in
                    Image(systemName: model.state.hideText ? "eye.slash" : "eye")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(model.state.hideText ? Zone.accentText(scheme) : Zone.muted(scheme))
                        .frame(width: 24, height: 22)
                        .contentShape(Rectangle())
                }
            }
            .buttonStyle(.plain)
            .help(model.state.hideText ? "Show what you said" : "Hide what you said, for when you share your screen")
            .accessibilityLabel(model.state.hideText ? "Show text" : "Hide text")
        } else {
            full
        }
    }

    private var full: some View {
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
