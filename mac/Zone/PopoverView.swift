import AppKit
import SwiftUI
import ZoneKit

/// Which page of the popover is showing.
@MainActor
@Observable
final class PopoverNavigation {
    enum Page: String {
        case main, pair, agents, settings
    }

    var page: Page = .main
}

/// What opens when the Z in the menu bar is clicked.
struct PopoverView: View {
    let model: AppModel
    let navigation: PopoverNavigation
    /// Tells the window how tall the content wants to be.
    var onHeight: (CGFloat) -> Void = { _ in }
    @Environment(\.colorScheme) private var scheme

    static let width: CGFloat = 372

    var body: some View {
        VStack(spacing: 0) {
            switch navigation.page {
            case .main: MainPage(model: model, navigation: navigation)
            case .pair: PairPage(model: model, navigation: navigation)
            case .agents: AgentsPage(model: model, navigation: navigation)
            case .settings: SettingsPage(model: model, navigation: navigation)
            }
        }
        .frame(width: Self.width)
        .fixedSize(horizontal: false, vertical: true)
        .background(Zone.paper(scheme))
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
        .background(GeometryReader { proxy in
            Color.clear.preference(key: HeightKey.self, value: proxy.size.height)
        })
        .onPreferenceChange(HeightKey.self) { height in
            if height > 0 { onHeight(height) }
        }
        .tint(Zone.accentText(scheme))
    }
}

private struct HeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

// MARK: Main page

private struct MainPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            VStack(alignment: .leading, spacing: 14) {
                if let banner = takeoverBanner {
                    banner
                }
                if case let .problem(message) = model.daemon {
                    problemBanner(message)
                }
                if let issue = model.permissionIssue {
                    permissionBanner(issue.text, issue.permission)
                }
                phones
                if let clip = model.leadingClip {
                    ClipCard(model: model, clip: clip)
                        .transition(.opacity.combined(with: .offset(y: -6)))
                }
                figures
                activity
                recent
            }
            .animation(Zone.settle, value: model.leadingClip?.id)
            .padding(.horizontal, 14)
            .padding(.top, 14)
            .padding(.bottom, 12)
            Hairline()
            actions
            Hairline()
            footer
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            Wordmark(size: 24)
            Spacer()
            HStack(spacing: 2) {
                stateLight
                Text(model.headline)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(isProblem ? Zone.alarm : Zone.ink(scheme))
                    .lineLimit(2)
                    .multilineTextAlignment(.trailing)
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 11)
        .background(alignment: .bottom) {
            // The voice arriving: the wave wakes while something is being received.
            FlowingWave(color: Zone.accentGraphic(scheme), energy: model.isSyncing ? 1 : 0)
                .frame(height: 26)
                .padding(.leading, 96)
                .padding(.trailing, 110)
                .opacity(model.isSyncing ? 0.9 : 0)
                .animation(.easeInOut(duration: 0.5), value: model.isSyncing)
                .offset(y: -8)
        }
    }

    private var isProblem: Bool {
        if case .problem = model.daemon { return true }
        return false
    }

    @ViewBuilder
    private var stateLight: some View {
        switch model.daemon {
        case .running:
            StatusDot(color: Zone.accentGraphic(scheme), pulses: model.isSyncing, size: 8)
        case .checking, .starting, .waitingForLoginItem:
            StatusDot(color: Zone.muted(scheme), hollow: true, size: 8)
        case .problem:
            StatusDot(color: Zone.alarm, size: 8)
        }
    }

    // MARK: Someone else runs zone

    private var takeoverBanner: AnyView? {
        let waiting = model.daemon == .waitingForLoginItem
        guard waiting || ((model.owner == .loginItem || model.owner == .other) && !model.state.declinedTakeover) else { return nil }
        let text: String
        if waiting {
            text = "zone is set to start at login from the command line, and has not started yet. This app is waiting for it."
        } else if model.owner == .loginItem {
            text = "zone was already set to start at login from the command line. This app is using that one."
        } else {
            text = "A zone started somewhere else (a terminal, most likely) is running. This app is using that one."
        }
        return AnyView(
            VStack(alignment: .leading, spacing: 8) {
                Text(text)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button(model.working.contains("takeover") ? "Taking over" : "Let this app run zone") {
                        Task { await model.takeOver() }
                    }
                    .buttonStyle(ZonePillButtonStyle(filled: true))
                    .disabled(model.working.contains("takeover"))
                    if !waiting {
                        Button("Keep it this way") {
                            model.state.declinedTakeover = true
                        }
                        .buttonStyle(ZonePillButtonStyle())
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(10)
            .background(Zone.accentGraphic(scheme).opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.accentGraphic(scheme).opacity(0.35), lineWidth: 1))
        )
    }

    // MARK: Something needs the person

    /// zone would not start: try again, or see why, without a terminal.
    private func problemBanner(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(message)
                .font(.system(size: 11.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button("Start again") { model.startAgain() }
                    .buttonStyle(ZonePillButtonStyle(filled: true))
                Button("Show the log") {
                    model.closePopover()
                    model.showLog()
                }
                .buttonStyle(ZonePillButtonStyle())
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(Zone.alarm.opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.alarm.opacity(0.4), lineWidth: 1))
    }

    /// Something macOS has to allow, with the button that shows where.
    private func permissionBanner(_ text: String, _ permission: Permission) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: Self.symbol(for: permission))
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Zone.accentText(scheme))
                .frame(width: 22)
            Text(text)
                .font(.system(size: 11.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button("Allow") {
                model.closePopover()
                model.allow(permission)
            }
            .buttonStyle(ZonePillButtonStyle(filled: true))
            .help("Opens System Settings and shows what to switch on")
        }
        .padding(10)
        .background(Zone.accentGraphic(scheme).opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.accentGraphic(scheme).opacity(0.35), lineWidth: 1))
    }

    static func symbol(for permission: Permission) -> String {
        switch permission {
        case .camera: return "camera"
        case .localNetwork: return "wifi"
        case .filesAndFolders: return "folder"
        case .loginItems: return "power"
        }
    }

    // MARK: Phones

    private var phones: some View {
        VStack(alignment: .leading, spacing: 0) {
            let phones = model.status?.phones ?? []
            if phones.isEmpty {
                HStack(spacing: 10) {
                    Image(systemName: "iphone.slash")
                        .font(.system(size: 17))
                        .foregroundStyle(Zone.muted(scheme))
                        .frame(width: 26)
                    VStack(alignment: .leading, spacing: 1) {
                        Text("No iPhone paired yet")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Zone.ink(scheme))
                        Text("Scan one code and your dictations start arriving.")
                            .font(.system(size: 11.5))
                            .foregroundStyle(Zone.muted(scheme))
                    }
                    Spacer(minLength: 4)
                    Button("Pair") {
                        navigation.page = .pair
                    }
                    .buttonStyle(ZonePillButtonStyle(filled: true))
                }
                .padding(.vertical, 10)
                .padding(.horizontal, 10)
            } else {
                ForEach(Array(phones.enumerated()), id: \.element.id) { index, phone in
                    PhoneRow(model: model, phone: phone)
                    if index < phones.count - 1 {
                        Hairline().padding(.leading, 46)
                    }
                }
            }
            Hairline()
            HStack(spacing: 6) {
                Image(systemName: reachSymbol)
                    .font(.system(size: 10, weight: .medium))
                Text(reachText)
                    .font(.system(size: 11))
                Spacer(minLength: 0)
            }
            .foregroundStyle(Zone.muted(scheme))
            .padding(.vertical, 7)
            .padding(.horizontal, 12)
        }
        .zoneCard(padding: 0)
    }

    private var reachSymbol: String {
        guard model.isRunning else { return "moon.zzz" }
        return model.status?.daemon.relayConnected == true ? "globe" : "wifi"
    }

    private var reachText: String {
        guard model.isRunning else { return "Not receiving while zone is not running" }
        if model.status?.daemon.relayConnected == true {
            return "Reachable from any network, end to end encrypted"
        }
        return "Reachable on this network. Connecting to the relay"
    }

    // MARK: Numbers

    private var figures: some View {
        HStack(alignment: .top, spacing: 0) {
            figure(model.todayCount, "today")
            divider
            figure(model.status?.items.total ?? 0, "in your library")
            divider
            VStack(alignment: .leading, spacing: 3) {
                kindLine("waveform", "dictation")
                kindLine("note.text", "note")
                kindLine("person.2", "meeting")
                if (model.status?.items.byKind["clip"] ?? 0) > 0 {
                    kindLine("doc.on.clipboard", "clip")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.leading, 14)
        }
    }

    private var divider: some View {
        Rectangle().fill(Zone.rule(scheme)).frame(width: 1, height: 40).padding(.top, 2)
    }

    private func figure(_ value: Int, _ label: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            CountingNumber(value: value, size: 27)
            Text(label)
                .font(.system(size: 11))
                .foregroundStyle(Zone.muted(scheme))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
    }

    private func kindLine(_ symbol: String, _ kind: String) -> some View {
        let count = model.status?.items.byKind[kind] ?? 0
        return HStack(spacing: 6) {
            Image(systemName: symbol)
                .font(.system(size: 9.5, weight: .medium))
                .frame(width: 13)
            Text("\(Formats.number(count)) \(count == 1 ? kind : kind + "s")")
                .font(.system(size: 11).monospacedDigit())
        }
        .foregroundStyle(Zone.muted(scheme))
    }

    // MARK: Activity

    @ViewBuilder
    private var activity: some View {
        let news = Array(model.feed.news.prefix(3))
        if !news.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Eyebrow("What just happened").padding(.leading, 8)
                ForEach(news) { event in
                    ActivityRow(event: event, now: model.now)
                }
            }
        }
    }

    // MARK: Recent items

    private var recent: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Eyebrow("Latest").padding(.leading, 8)
                Spacer()
                HideTextButton(model: model)
            }
            if model.recent.isEmpty {
                Text(model.status == nil ? "Reading your library" : "Nothing here yet. What you say into Zone on your iPhone shows up here.")
                    .font(.system(size: 12))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
            } else {
                ForEach(model.recent.prefix(4)) { item in
                    ItemRow(model: model, item: item)
                }
                Button {
                    model.closePopover()
                    model.showFloating()
                } label: {
                    HStack(spacing: 4) {
                        Text("Keep these in a floating window")
                        Image(systemName: "arrow.up.forward.app")
                            .font(.system(size: 10, weight: .medium))
                    }
                }
                .buttonStyle(ZoneLinkButtonStyle(size: 11.5))
                .padding(.leading, 8)
                .padding(.top, 2)
            }
        }
    }

    // MARK: Actions

    private var actions: some View {
        HStack(spacing: 0) {
            ActionButton(symbol: "qrcode", title: "Pair") { navigation.page = .pair }
            ActionButton(symbol: "clock.arrow.circlepath", title: "History", working: model.working.contains("history")) {
                model.openHistory()
                model.closePopover()
            }
            ActionButton(symbol: "bolt", title: "Flash", working: model.working.contains("flash")) {
                model.openFlash()
                model.closePopover()
            }
            ActionButton(symbol: "sparkles", title: "Agents", badge: agentBadge) { navigation.page = .agents }
            ActionButton(symbol: "ellipsis", title: "More") { navigation.page = .settings }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
    }

    private var agentBadge: String? {
        let on = model.agents.filter(\.isOn).count
        return on > 0 ? "\(on)" : nil
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if let notice = model.notice {
                Text(notice)
                    .font(.system(size: 11))
                    .foregroundStyle(Zone.ink(scheme))
                    .lineLimit(2)
                    .transition(.opacity)
            } else {
                Text(lastSyncLine)
                    .font(.system(size: 11).monospacedDigit())
                    .foregroundStyle(Zone.muted(scheme))
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            Button("Quit") {
                NSApp.terminate(nil)
            }
            .buttonStyle(ZoneLinkButtonStyle(size: 11.5))
            .help("Quit Zone. Syncing stops until you open it again.")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 9)
        .animation(Zone.motion, value: model.notice)
    }

    private var lastSyncLine: String {
        guard let status = model.status else { return " " }
        let ages = [status.lastSync.link?.age, status.lastSync.flash?.age, status.lastSync.backupFolder?.age].compactMap { $0 }
        guard let newest = ages.min() else { return "Nothing has synced yet" }
        return "Last sync \(Formats.ago(newest))"
    }
}

private struct PhoneRow: View {
    let model: AppModel
    let phone: StatusReport.Phone
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "iphone")
                .font(.system(size: 18))
                .foregroundStyle(Zone.ink(scheme))
                .frame(width: 26)
            VStack(alignment: .leading, spacing: 1) {
                Text(phone.name)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Zone.ink(scheme))
                    .lineLimit(1)
                Text(detail)
                    .font(.system(size: 11.5).monospacedDigit())
                    .foregroundStyle(Zone.muted(scheme))
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if let route = ActivityWords.route(phone.lastPath) {
                Text(route)
                    .font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                    .padding(.horizontal, 7)
                    .frame(height: 19)
                    .overlay(Capsule().strokeBorder(Zone.rule(scheme), lineWidth: 1))
                    .help("How the last sync travelled")
            }
        }
        .padding(.vertical, 9)
        .padding(.horizontal, 10)
        .contextMenu {
            Button("Ask \(phone.name) to send everything again") { model.resendEverything() }
            Button("Forget \(phone.name)") { model.forget(phone) }
        }
    }

    private var detail: String {
        let items = "\(Formats.number(phone.itemsReceived)) \(phone.itemsReceived == 1 ? "item" : "items")"
        guard let age = phone.lastSyncAge else { return "Paired, waiting for its first sync" }
        return "Synced \(Formats.ago(age)) · \(items)"
    }
}

private struct ActivityRow: View {
    let event: ActivityEvent
    let now: Date
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 10.5, weight: .semibold))
                .foregroundStyle(event.broughtItems ? Zone.accentGraphic(scheme) : Zone.muted(scheme))
                .frame(width: 18)
            Text(ActivityWords.headline(event))
                .font(.system(size: 12.5, weight: event.broughtItems ? .medium : .regular))
                .foregroundStyle(Zone.ink(scheme))
                .lineLimit(1)
            Spacer(minLength: 6)
            Text(trailing)
                .font(.system(size: 11).monospacedDigit())
                .foregroundStyle(Zone.muted(scheme))
                .lineLimit(1)
                .layoutPriority(1)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 2)
    }

    private var trailing: String {
        Formats.ago(event.date, now: now)
    }

    private var symbol: String {
        switch event.kind {
        case .linkSession: return event.broughtItems ? "arrow.down" : "checkmark"
        case .flashReceived: return "bolt.fill"
        case .backupImport: return "folder"
        case .paired: return "link"
        case .unpaired: return "link.badge.plus"
        case .daemonStarted: return "power"
        case .daemonStopped: return "moon.zzz"
        case .relayConnected: return "globe"
        case .relayLost: return "wifi.exclamationmark"
        case .linkRefused, .flashFailed: return "exclamationmark.triangle"
        case .resyncRequested: return "arrow.triangle.2.circlepath"
        case .clip: return "doc.on.clipboard"
        case .pairOffered, .unknown: return "circle"
        }
    }
}

/// The clip that just arrived, at the top of the menu: where it came from, one line of it,
/// and a button that copies it.
private struct ClipCard: View {
    let model: AppModel
    let clip: Clip
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let copied = model.copiedID == clip.id
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: "doc.on.clipboard")
                .font(.system(size: 16))
                .foregroundStyle(Zone.accentText(scheme))
                .frame(width: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text(clip.title(phone: model.phoneName(for: clip), now: model.now))
                    .font(.system(size: 11.5, weight: .medium).monospacedDigit())
                    .foregroundStyle(Zone.muted(scheme))
                    .lineLimit(1)
                Group {
                    if model.state.hideText {
                        Text(clip.preview).redacted(reason: .placeholder)
                    } else {
                        Text(clip.preview)
                    }
                }
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Zone.ink(scheme))
                .lineLimit(1)
                .truncationMode(.tail)
                if model.wasPutOnClipboard(clip) {
                    Text("Already on this Mac's clipboard")
                        .font(.system(size: 10.5))
                        .foregroundStyle(Zone.muted(scheme))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button(copied ? "Copied" : "Copy") {
                model.copy(clip)
            }
            .buttonStyle(ZonePillButtonStyle(filled: true))
            .help("Copy this clip")
        }
        .padding(.vertical, 9)
        .padding(.leading, 6)
        .padding(.trailing, 10)
        .background(Zone.accentGraphic(scheme).opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Zone.accentGraphic(scheme).opacity(0.35), lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Clip. \(clip.title(phone: model.phoneName(for: clip), now: model.now))")
    }
}

private struct ActionButton: View {
    let symbol: String
    let title: String
    var badge: String?
    var working = false
    let action: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(action: action) {
            HoverRow(radius: 9) { _ in
                VStack(spacing: 4) {
                    ZStack(alignment: .topTrailing) {
                        Group {
                            if working {
                                ProgressView().controlSize(.small)
                            } else {
                                Image(systemName: symbol)
                                    .font(.system(size: 15, weight: .regular))
                            }
                        }
                        .frame(width: 30, height: 22)
                        if let badge {
                            Text(badge)
                                .font(.system(size: 9, weight: .bold).monospacedDigit())
                                .foregroundStyle(.white)
                                .padding(.horizontal, 4)
                                .frame(minWidth: 14, minHeight: 14)
                                .background(Zone.accentGraphic(scheme), in: Capsule())
                                .offset(x: 7, y: -4)
                        }
                    }
                    Text(title)
                        .font(.system(size: 11, weight: .medium))
                }
                .foregroundStyle(Zone.ink(scheme))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 7)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
    }
}

// MARK: Sub pages

/// The top of a page inside the popover: a way back and a title.
private struct PageHeader: View {
    let title: String
    let navigation: PopoverNavigation
    var onBack: () -> Void = {}
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    onBack()
                    navigation.page = .main
                } label: {
                    HStack(spacing: 3) {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 11, weight: .semibold))
                        Text("Back")
                    }
                    .padding(.vertical, 4)
                    .padding(.trailing, 6)
                    .contentShape(Rectangle())
                }
                .buttonStyle(ZoneLinkButtonStyle())
                .keyboardShortcut(.cancelAction)
                Spacer()
                Text(title)
                    .font(.zoneSerif(16))
                    .foregroundStyle(Zone.ink(scheme))
                Spacer()
                // Balances the Back button so the title sits in the middle.
                Color.clear.frame(width: 48, height: 1)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Hairline()
        }
    }
}

private struct PairPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "Pair an iPhone", navigation: navigation) {
                model.stopPairing()
            }
            PairPanel(model: model, startsFresh: true)
                .padding(.horizontal, 20)
                .padding(.vertical, 20)
            Hairline()
            HStack {
                Text("Pairing comes with Zone's next iPhone update.")
                    .font(.system(size: 11))
                    .foregroundStyle(Zone.muted(scheme))
                Spacer()
                if case .paired = model.pairing {
                    Button("Done") {
                        model.stopPairing()
                        navigation.page = .main
                    }
                    .buttonStyle(ZonePillButtonStyle(filled: true))
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
        }
    }
}

private struct AgentsPage: View {
    let model: AppModel
    let navigation: PopoverNavigation

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "AI agents", navigation: navigation)
            AgentsPanel(model: model)
                .padding(14)
        }
        .task { await model.loadAgents() }
    }
}

private struct SettingsPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "More", navigation: navigation)
            VStack(alignment: .leading, spacing: 14) {
                section("Sync") {
                    row("arrow.triangle.2.circlepath", "Ask the phone to resend everything", "For when something looks missing. It happens at the phone's next sync.") {
                        Button(model.working.contains("resync") ? "Asking" : "Ask") { model.resendEverything() }
                            .buttonStyle(ZonePillButtonStyle())
                            .disabled(model.working.contains("resync"))
                    }
                    Hairline().padding(.leading, 38)
                    row("folder", "Backup folder", backupLine) {
                        HStack(spacing: 6) {
                            if model.backupFolderBlocked {
                                Button("Allow") {
                                    model.closePopover()
                                    model.allowBackupFolder()
                                }
                                .buttonStyle(ZonePillButtonStyle(filled: true))
                                .help("Opens System Settings and shows what to switch on")
                            }
                            if model.status?.backupFolder != nil {
                                Button("Stop") { model.setBackupFolder(nil) }
                                    .buttonStyle(ZonePillButtonStyle())
                            }
                            Button(model.working.contains("backup") ? "Reading" : "Choose") { chooseBackupFolder() }
                                .buttonStyle(ZonePillButtonStyle())
                                .disabled(model.working.contains("backup"))
                        }
                    }
                }
                if model.hasClips {
                    section("Clips") {
                        Toggle(isOn: Binding(get: { model.clipboardCopyOn }, set: { model.setClipboardCopy($0) })) {
                            label("doc.on.clipboard", "Copy to this Mac's clipboard when a clip arrives", clipboardLine)
                        }
                        .toggleStyle(ZoneToggleStyle())
                        .disabled(model.clipboardCopy?.available == false || model.working.contains("clipboard"))
                        .padding(.vertical, 8)
                        .padding(.horizontal, 10)
                    }
                }
                section("This Mac") {
                    if model.loginItemNeedsApproval {
                        row("power", "Start Zone when you log in", "macOS wants you to allow it in Login Items first.") {
                            Button("Allow") {
                                model.closePopover()
                                model.approveLoginItem()
                            }
                            .buttonStyle(ZonePillButtonStyle(filled: true))
                        }
                    } else {
                        Toggle(isOn: Binding(get: { model.loginItemOn }, set: { model.setLoginItem($0) })) {
                            label("power", "Start Zone when you log in", model.env.isSandbox ? "Test run: no real login item is added." : "So your phone can always reach this Mac.")
                        }
                        .toggleStyle(ZoneToggleStyle())
                        .padding(.vertical, 8)
                        .padding(.horizontal, 10)
                    }
                    if model.runsZone, model.localNetwork == .denied {
                        Hairline().padding(.leading, 38)
                        row("wifi", "Direct to your iPhone", "Not allowed, so syncs go through the relay. Allow Local Network for Zone to connect straight to the phone on the same network.") {
                            Button("Allow") {
                                model.closePopover()
                                model.allowLocalNetwork()
                            }
                            .buttonStyle(ZonePillButtonStyle(filled: true))
                        }
                    }
                    Hairline().padding(.leading, 38)
                    commandRow
                    Hairline().padding(.leading, 38)
                    row("gearshape.2", "Who runs zone", ownerLine) {
                        if model.owner == .loginItem || model.owner == .other || model.daemon == .waitingForLoginItem {
                            Button(model.working.contains("takeover") ? "Taking over" : "Take over") {
                                Task { await model.takeOver() }
                            }
                            .buttonStyle(ZonePillButtonStyle())
                            .disabled(model.working.contains("takeover"))
                        }
                    }
                }
                section("About") {
                    row("arrow.down.circle", "Check for updates", updateLine) {
                        Button(model.checkingUpdate ? "Checking" : "Check") {
                            Task { await model.checkForUpdates(openPage: true) }
                        }
                        .buttonStyle(ZonePillButtonStyle())
                        .disabled(model.checkingUpdate)
                    }
                    Hairline().padding(.leading, 38)
                    row("hand.wave", "Setup guide", "Pairing, agents and the rest, step by step.") {
                        Button("Open") {
                            model.closePopover()
                            model.showWelcome()
                        }
                        .buttonStyle(ZonePillButtonStyle())
                    }
                    Hairline().padding(.leading, 38)
                    row("externaldrive", "Your library", Formats.tilde(model.env.dataDirectory.path, home: model.env.home.path)) {
                        Button("Show") {
                            model.closePopover()
                            model.open(model.env.dataDirectory)
                        }
                        .buttonStyle(ZonePillButtonStyle())
                    }
                }
                if let notice = model.notice {
                    Text(notice)
                        .font(.system(size: 11.5))
                        .foregroundStyle(Zone.ink(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 4)
                }
            }
            .padding(14)
            Hairline()
            HStack {
                Text("Zone \(model.appVersion)\(model.zoneVersion.map { ", zone \($0)" } ?? "")")
                    .font(.system(size: 11).monospacedDigit())
                    .foregroundStyle(Zone.muted(scheme))
                Spacer()
                Button("Quit Zone") { NSApp.terminate(nil) }
                    .buttonStyle(ZoneLinkButtonStyle(size: 11.5))
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
        }
        .animation(Zone.motion, value: model.notice)
        .task {
            await model.refreshClipboardSetting()
            model.refreshLoginItem()
            await model.lookAtLocalNetwork()
        }
    }

    private var backupLine: String {
        guard let folder = model.status?.backupFolder else {
            return "Not set. Zone on the iPhone can keep a backup in iCloud Drive: choose that folder and it is read whenever it changes."
        }
        let path = Formats.tilde(folder, home: model.env.home.path)
        switch model.status?.backupState?.state {
        case "waiting": return "\(path). macOS is asking whether Zone may read it: look for its question, or click Allow."
        case "error":
            if model.backupFolderBlocked { return "\(path). macOS has not let Zone read it. Click Allow to see where to switch it on." }
            return "\(path). Cannot be read: \(model.status?.backupState?.message ?? "")"
        default: return path
        }
    }

    private var clipboardLine: String {
        if let setting = model.clipboardCopy, !setting.available {
            return setting.fix ?? "This Mac has no tool that can write the clipboard."
        }
        return "A clip is text you send from Zone on your iPhone. With this on, it is ready to paste here a moment later."
    }

    private var ownerLine: String {
        switch model.daemon {
        case .running(.app): return "This app. It stops when you quit Zone."
        case .running(.adopted): return "This app (zone was still running from last time)."
        case .running(.loginItem):
            let program = model.legacyProgram.map { Formats.tilde($0, home: model.env.home.path) } ?? "zone install"
            return "The login item from the command line (\(program)). It keeps running without this app."
        case .running(.other): return "A zone started outside this app, most likely in a terminal."
        case .starting, .checking: return "Starting."
        case .waitingForLoginItem: return "The login item from the command line, which has not started zone yet."
        case let .problem(message): return message
        }
    }

    private var updateLine: String {
        switch model.update {
        case let .newer(version, _): return "\(version) is out."
        case .upToDate: return "You have the latest."
        case let .unknown(why): return why
        case nil: return "Opens the releases page when there is something newer."
        }
    }

    @ViewBuilder
    private var commandRow: some View {
        switch model.commandLink {
        case let .linked(link):
            row("terminal", "zone in the terminal", "On your PATH at \(Formats.tilde(link.path, home: model.env.home.path)).") {
                Button("Remove") { model.removeCommand() }
                    .buttonStyle(ZonePillButtonStyle())
            }
        case let .foreign(existing):
            row("terminal", "zone in the terminal", "Another zone is at \(Formats.tilde(existing.path, home: model.env.home.path)). It is left alone unless you say so.") {
                Button("Use this app's") { model.installCommand(replace: true) }
                    .buttonStyle(ZonePillButtonStyle())
            }
        case let .absent(suggested, needsAdmin):
            row("terminal", "zone in the terminal", "Adds the command to \(Formats.tilde(suggested.deletingLastPathComponent().path, home: model.env.home.path))\(needsAdmin ? " (asks for your password)" : "").") {
                Button("Add") { model.installCommand() }
                    .buttonStyle(ZonePillButtonStyle())
            }
        case nil:
            row("terminal", "zone in the terminal", "Looking at your PATH.") { EmptyView() }
        }
    }

    private func chooseBackupFolder() {
        model.closePopover()
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Watch this folder"
        panel.message = "Choose the folder that holds Zone's backups (the one with a .zonebackup inside)."
        NSApp.activate(ignoringOtherApps: true)
        if panel.runModal() == .OK, let folder = panel.url {
            model.setBackupFolder(folder)
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Eyebrow(title).padding(.leading, 6)
            VStack(spacing: 0) { content() }
                .zoneCard(padding: 0, shadow: false)
        }
    }

    private func label(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Zone.muted(scheme))
                .frame(width: 18, height: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundStyle(Zone.ink(scheme))
                Text(detail)
                    .font(.system(size: 11))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func row<Trailing: View>(_ symbol: String, _ title: String, _ detail: String, @ViewBuilder trailing: () -> Trailing) -> some View {
        HStack(alignment: .center, spacing: 8) {
            label(symbol, title, detail)
            Spacer(minLength: 6)
            trailing()
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
    }
}
