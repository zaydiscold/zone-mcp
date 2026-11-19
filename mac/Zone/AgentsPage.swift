import AppKit
import SwiftUI
import ZoneKit

// The Agents page of the menu, top to bottom: one switch for every agent, what agents can
// read as five choices, the connected apps with a menu to connect another (or paste zone
// into an app it does not know), and things to ask an agent. A part whose command this zone
// does not have is left out.

/// The icons of the installed apps behind the clients zone knows, read once from their
/// bundles. Nil for a client that is a command line tool, or an app that is not installed.
@MainActor
enum AppIcons {
    private static var cache: [String: NSImage?] = [:]

    static func icon(for client: String) -> NSImage? {
        if let known = cache[client] { return known }
        let url = AgentApps.bundleIDs[client]?.lazy.compactMap { NSWorkspace.shared.urlForApplication(withBundleIdentifier: $0) }.first
        let icon = url.map { NSWorkspace.shared.icon(forFile: $0.path) }
        cache[client] = icon
        return icon
    }
}

/// "Connect an app": each AI app zone knows that is on this Mac and not connected, with its
/// own icon, then "Other app..." for any app that takes MCP servers.
@MainActor
final class ConnectMenu: NSObject {
    private var actions: [() -> Void] = []

    static func show(model: AppModel, navigation: PopoverNavigation, at point: NSPoint) {
        let owner = ConnectMenu()
        let menu = owner.make(model: model, navigation: navigation)
        // Blocks until the menu closes; `owner` keeps the actions alive until then.
        menu.popUp(positioning: nil, at: point, in: nil)
        withExtendedLifetime(owner) {}
    }

    /// The apps it lists: on this Mac, zone not in them, in zone's order.
    static func available(_ agents: [Agent]) -> [Agent] {
        AgentSections(agents).off.filter(\.installed)
    }

    func make(model: AppModel, navigation: PopoverNavigation) -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        for agent in Self.available(model.agents) {
            let item = add(to: menu, agent.removedByYou ? "\(agent.title) (removed by you)" : agent.title) {
                Task { await model.setAgent(agent, connected: true) }
            }
            item.image = AppIcons.icon(for: agent.id).map(Self.small)
                ?? NSImage(systemSymbolName: AgentBadge.symbol(for: agent.id), accessibilityDescription: nil)
        }
        if model.otherAppConfig != nil {
            if !menu.items.isEmpty { menu.addItem(.separator()) }
            let item = add(to: menu, "Other app\u{2026}") { navigation.otherAppOpen = true }
            item.image = NSImage(systemSymbolName: "square.dashed", accessibilityDescription: nil)
        }
        return menu
    }

    @discardableResult
    private func add(to menu: NSMenu, _ title: String, _ action: @escaping () -> Void) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: #selector(pick(_:)), keyEquivalent: "")
        item.target = self
        item.tag = actions.count
        actions.append(action)
        menu.addItem(item)
        return item
    }

    @objc private func pick(_ item: NSMenuItem) {
        guard actions.indices.contains(item.tag) else { return }
        actions[item.tag]()
    }

    /// An app's icon at menu size.
    private static func small(_ icon: NSImage) -> NSImage {
        let small = icon.copy() as? NSImage ?? icon
        small.size = NSSize(width: 16, height: 16)
        return small
    }
}

struct AgentsPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @State private var contentHeight: CGFloat = 400
    @Environment(\.colorScheme) private var scheme

    /// The page scrolls inside the window when it is taller than the screen allows.
    private var room: CGFloat { max(300, navigation.maxHeight - 60) }

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "AI agents", navigation: navigation)
            ScrollViewReader { proxy in
                ScrollView {
                    content
                        .background(GeometryReader { proxy in
                            Color.clear.preference(key: ContentHeight.self, value: proxy.size.height)
                        })
                }
                .scrollIndicators(.automatic)
                // Opening the whole list of things to ask brings it into view.
                .onChange(of: navigation.allPrompts) { _, all in
                    guard all else { return }
                    withAnimation(Zone.motion) { proxy.scrollTo("prompts", anchor: .top) }
                }
            }
            .frame(height: min(contentHeight, room))
            .onPreferenceChange(ContentHeight.self) { height in
                if height > 0 { contentHeight = height }
            }
        }
        .task { await model.loadAgents() }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let access = model.agentAccess {
                masterSwitch(access)
                InsetHairline()
            }
            // With the switch off, what is under it still works and is kept, and looks it.
            VStack(alignment: .leading, spacing: 0) {
                if model.agentsLoaded {
                    readSection
                    InsetHairline()
                }
                appsSection
                if let prompts = model.prompts, !prompts.prompts.isEmpty {
                    InsetHairline()
                    promptsSection(prompts.prompts)
                        .id("prompts")
                }
            }
            .opacity(model.agentAccess?.enabled == false ? 0.5 : 1)
        }
        .padding(.bottom, 8)
        .animation(Zone.motion, value: navigation.otherAppOpen)
        .animation(Zone.motion, value: navigation.allPrompts)
        .animation(Zone.motion, value: model.agentAccess)
    }

    // MARK: The switch

    private func masterSwitch(_ access: AgentAccess) -> some View {
        Toggle(isOn: Binding(get: { access.enabled }, set: { on in Task { await model.setAgentAccess(on) } })) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Let AI agents read my library")
                    .font(.system(size: 13.5, weight: .semibold))
                    .foregroundStyle(Zone.ink(scheme))
                Text(access.enabled
                    ? "Each connected app reads what you allow below. None can change or delete your words."
                    : "Off. Every app below is told access is off, and gets nothing from your library.")
                    .font(.system(size: 11.5))
                    .foregroundStyle(access.enabled ? Zone.muted(scheme) : Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .toggleStyle(ZoneToggleStyle())
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .accessibilityLabel("Let AI agents read my library")
    }

    // MARK: What agents can read

    private var readSection: some View {
        let choices = model.sharedChoices
        let shown = choices ?? ReadChoices()
        let followers = SharedView.followers(model.agents, shared: model.sharedView)
        let exceptions = SharedView.exceptions(model.agents, shared: model.sharedView)
        return VStack(alignment: .leading, spacing: 4) {
            Eyebrow("What agents can read")
                .padding(.horizontal, 8)
            VStack(spacing: 0) {
                choice(\.keyboard, "Dictations into other apps", "Messages, emails, searches", shown,
                       help: "What you typed by voice with the Zone keyboard, in any app")
                choice(\.inZone, "Dictations in Zone", "With the recordings behind notes", shown,
                       help: "Dictated inside the Zone app, and the recording behind each voice note")
                choice(\.notes, "Notes", "Voice notes you saved", shown)
                choice(\.meetings, "Meetings", "Transcripts", shown)
                if model.hasClips {
                    choice(\.clips, "Clips", "Text you sent to this Mac", shown)
                }
            }
            .disabled(model.applyingView || followers.isEmpty && model.agents.contains(where: \.isOn))
            Text(readLine(choices: choices, followers: followers, exceptions: exceptions))
                .font(.system(size: 11))
                .foregroundStyle(Zone.quiet(scheme))
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 8)
                .padding(.top, 2)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 12)
    }

    /// One choice: a check, its name, and what it covers.
    private func choice(_ path: WritableKeyPath<ReadChoices, Bool>, _ title: String, _ detail: String, _ choices: ReadChoices, help: String? = nil) -> some View {
        let on = choices[keyPath: path]
        let last = choices.isLast(path, knowsClips: model.hasClips)
        return Button {
            var next = choices
            next[keyPath: path].toggle()
            Task { await model.setSharedView(next.view(knowsClips: model.hasClips)) }
        } label: {
            HoverRow(radius: 7) { _ in
                HStack(alignment: .firstTextBaseline, spacing: 9) {
                    Image(systemName: on ? "checkmark.square.fill" : "square")
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(on ? Zone.accentGraphic(scheme) : Zone.faint(scheme))
                        .frame(width: 18)
                    Text(title)
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(Zone.ink(scheme))
                    Text(detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Zone.quiet(scheme))
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 8)
                .frame(height: 26)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .disabled(last)
        .help(last ? "To let agents read nothing, turn off the switch at the top" : help ?? detail)
        .accessibilityLabel("\(title). \(help ?? detail)")
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    /// Exactly what a change here does: which apps it is written into and when they notice,
    /// and which apps keep a limit of their own.
    private func readLine(choices: ReadChoices?, followers: [Agent], exceptions: [Agent]) -> String {
        if model.applyingView { return "Writing it into \(count(followers, "app")) settings." }
        var lines: [String] = []
        if choices == nil {
            lines.append("Your apps read a view set from the command line: \(model.sharedView.words). Choosing here replaces it.")
        }
        if followers.isEmpty {
            lines.append(model.agents.contains(where: \.isOn)
                ? "Every connected app has a limit of its own, set under its menu."
                : "An app you connect reads this.")
        } else {
            let places = followers.reduce(0) { $0 + $1.connectedPlaces.count }
            let profiles = places > followers.count ? " (\(places) profiles)" : ""
            lines.append("Applies to \(count(followers, "connected app"))\(profiles) and any you connect. Each reads this way from its next new chat or session.")
        }
        for agent in exceptions.prefix(3) {
            lines.append(agent.viewsDiffer
                ? "\(agent.title)'s profiles read different things."
                : "\(agent.title) keeps its own limit: \(agent.view?.words ?? "its own").")
        }
        if exceptions.count > 3 { lines.append("\(exceptions.count - 3) more apps keep their own.") }
        return lines.joined(separator: " ")
    }

    private func count(_ agents: [Agent], _ noun: String) -> String {
        "\(agents.count) \(noun)\(agents.count == 1 ? "" : "s")"
    }

    // MARK: Connected apps

    private var appsSection: some View {
        let sections = AgentSections(model.agents)
        let connected = sections.on
        let available = ConnectMenu.available(model.agents)
        return VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Eyebrow("Connected apps")
                Spacer(minLength: 8)
                if model.agentsLoaded {
                    connectMenu(available)
                }
            }
            .padding(.horizontal, 8)
            if !model.agentsLoaded {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Looking for AI apps on this Mac")
                        .font(.system(size: 12))
                        .foregroundStyle(Zone.muted(scheme))
                }
                .padding(8)
            } else if connected.isEmpty {
                Text(available.isEmpty
                    ? "No AI apps found on this Mac yet. Install Claude, Codex, Cursor or another one, or use Other app."
                    : "None yet. Connect an app above: it starts zone by itself when it needs your library.")
                    .font(.system(size: 12))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(8)
            } else {
                Text("Each app has its own entry, so disconnecting one leaves the others as they are.")
                    .font(.system(size: 11))
                    .foregroundStyle(Zone.quiet(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 8)
                VStack(spacing: 0) {
                    ForEach(Array(connected.enumerated()), id: \.element.id) { index, agent in
                        AgentRow(model: model, agent: agent, state: navigation, calm: true)
                        if index < connected.count - 1 {
                            Hairline().padding(.leading, 44).padding(.trailing, 8)
                        }
                    }
                }
            }
            if navigation.otherAppOpen, let config = model.otherAppConfig {
                otherApp(config)
                    .transition(.opacity)
            }
            if let error = model.agentError {
                Text(error)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.alarm)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 8)
            } else if let notice = model.agentNotice {
                Text(notice)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 8)
            }
            if model.agentsLoaded {
                zoneItself
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 12)
    }

    /// The apps zone knows that are on this Mac and not connected, then Other app. The menu
    /// is `ConnectMenu`'s, the same one a test run opens.
    private func connectMenu(_ available: [Agent]) -> some View {
        Button {
            ConnectMenu.show(model: model, navigation: navigation, at: NSEvent.mouseLocation)
        } label: {
            HStack(spacing: 4) {
                Image(systemName: "plus")
                    .font(.system(size: 10, weight: .bold))
                Text("Connect an app")
            }
            .font(.system(size: 12, weight: .medium))
            .contentShape(Rectangle())
        }
        .buttonStyle(ZoneLinkButtonStyle(size: 12))
        .disabled(available.isEmpty && model.otherAppConfig == nil)
        .help(available.isEmpty ? "Every AI app zone knows on this Mac is connected" : "Apps on this Mac that zone knows")
    }

    /// For an app zone does not know: the entry to paste, a Copy button, and where it goes.
    private func otherApp(_ config: McpConfig) -> some View {
        let copied = model.copiedID == "other-app"
        return VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text("Other app")
                    .font(.system(size: 12.5, weight: .semibold))
                    .foregroundStyle(Zone.ink(scheme))
                Spacer()
                Button(copied ? "Copied" : "Copy") { model.copyText(config.snippet, id: "other-app") }
                    .buttonStyle(ZoneKeyButtonStyle(ink: true, height: 26))
                Button {
                    navigation.otherAppOpen = false
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Zone.quiet(scheme))
                        .frame(width: 22, height: 22)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Close")
            }
            Text("Paste this where the app lists its MCP servers: usually a file named mcp.json, or a Settings page called MCP. It reads what you chose above.")
                .font(.system(size: 11))
                .foregroundStyle(Zone.muted(scheme))
                .fixedSize(horizontal: false, vertical: true)
            ScrollView(.horizontal) {
                Text(config.snippet.trimmingCharacters(in: .newlines))
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(Zone.ink(scheme))
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(10)
            }
            .scrollIndicators(.never)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Zone.field(scheme), in: RoundedRectangle(cornerRadius: Zone.keyRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Zone.keyRadius, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 8)
    }

    /// The zone the apps are pointed at, and the same check with no app in between.
    private var zoneItself: some View {
        let test = model.agentTests["zone"]
        return HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text("zone on this Mac")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Zone.ink(scheme))
                Text(Formats.tilde(model.agentsZone ?? model.agentsBinary.path, home: model.env.home.path))
                    .font(.system(size: 10.5))
                    .foregroundStyle(Zone.quiet(scheme))
                    .lineLimit(1)
                    .truncationMode(.head)
                if let test {
                    TestLine(test: test)
                }
            }
            Spacer(minLength: 6)
            if AgentSections(model.agents).on.contains(where: \.isOn) {
                Button("Test all") {
                    model.testZone()
                    for agent in model.agents where agent.isOn { model.test(agent) }
                }
                .buttonStyle(ZoneLinkButtonStyle(size: 11.5))
                .help("Starts zone the way each app would and asks it for its tools")
            }
            Button("Test") { model.testZone() }
                .buttonStyle(ZonePillButtonStyle())
                .disabled(test == .running)
                .help("Starts this zone as an MCP server and asks it for its tools")
        }
        .padding(.horizontal, 8)
        .padding(.top, 8)
    }

    // MARK: Things to ask

    private func promptsSection(_ prompts: [PromptList.Prompt]) -> some View {
        let shown = navigation.allPrompts ? prompts : Array(prompts.prefix(3))
        return VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Eyebrow("Things to ask")
                Spacer(minLength: 8)
                if prompts.count > 3 {
                    Button(navigation.allPrompts ? "Fewer" : "All \(prompts.count)") { navigation.allPrompts.toggle() }
                        .buttonStyle(ZoneLinkButtonStyle(size: 12))
                }
            }
            .padding(.horizontal, 8)
            Text("Copy one into a connected app.")
                .font(.system(size: 11))
                .foregroundStyle(Zone.quiet(scheme))
                .padding(.horizontal, 8)
            VStack(spacing: 0) {
                ForEach(Array(shown.enumerated()), id: \.element.id) { index, prompt in
                    PromptRow(model: model, prompt: prompt)
                    if index < shown.count - 1 {
                        Hairline().padding(.horizontal, 8)
                    }
                }
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 12)
    }
}

/// One thing to ask: its title, one line, and Copy. Copying says so for a moment.
private struct PromptRow: View {
    let model: AppModel
    let prompt: PromptList.Prompt
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let copied = model.copiedID == "prompt-\(prompt.id)"
        Button {
            model.copyText(prompt.text, id: "prompt-\(prompt.id)")
        } label: {
            HoverRow(radius: 7) { hovering in
                HStack(alignment: .center, spacing: 10) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(prompt.title)
                            .font(.system(size: 12.5, weight: .semibold))
                            .foregroundStyle(Zone.ink(scheme))
                        Text(prompt.description)
                            .font(.system(size: 11))
                            .foregroundStyle(Zone.quiet(scheme))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    HStack(spacing: 3) {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                            .font(.system(size: 11, weight: .medium))
                        if copied || hovering {
                            Text(copied ? "Copied" : "Copy")
                                .font(.system(size: 11, weight: .semibold))
                        }
                    }
                    .foregroundStyle(copied || hovering ? Zone.accentText(scheme) : Zone.muted(scheme))
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 7)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .help("Copy this prompt")
        .accessibilityLabel("\(prompt.title). \(prompt.description). \(copied ? "Copied" : "Copy")")
    }
}

private struct InsetHairline: View {
    var body: some View { Hairline().padding(.horizontal, 16) }
}

private struct ContentHeight: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
