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

/// The menu: how syncing stands in one line, a clip that just arrived, the latest things
/// said, and four buttons. Everything else (each phone, pairing another, the backup folder,
/// who runs zone, quitting) is under More, and the history page has the rest of the library.
private struct MainPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            VStack(alignment: .leading, spacing: 12) {
                if model.loginItemStalled {
                    stalledBanner
                }
                if case let .problem(message) = model.daemon {
                    problemBanner(message)
                }
                if let issue = model.permissionIssue {
                    permissionBanner(issue.text, issue.permission)
                }
                if model.status?.phones.isEmpty == true {
                    noPhone
                }
                if let clip = model.leadingClip {
                    ClipCard(model: model, clip: clip)
                        .transition(.opacity.combined(with: .offset(y: -6)))
                }
                recent
            }
            .animation(Zone.settle, value: model.leadingClip?.id)
            .padding(.horizontal, 14)
            .padding(.top, 12)
            .padding(.bottom, 10)
            if let notice = model.notice {
                Text(notice)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 22)
                    .padding(.bottom, 10)
                    .transition(.opacity)
            }
            Hairline()
            actions
        }
        .animation(Zone.motion, value: model.notice)
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            Wordmark(size: 24)
            Spacer()
            HStack(spacing: 7) {
                stateLight
                Text(model.headline)
                    .font(.system(size: 12, weight: .medium).monospacedDigit())
                    .foregroundStyle(isProblem ? Zone.alarm : Zone.ink(scheme))
                    .lineLimit(1)
            }
            .help(model.statusDetail)
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
        return model.loginItemStalled
    }

    @ViewBuilder
    private var stateLight: some View {
        switch model.daemon {
        case .running:
            if model.status?.daemon.relayConnected == false {
                StatusDot(color: Zone.muted(scheme), size: 8)
            } else {
                StatusDot(color: Zone.accentGraphic(scheme), pulses: model.isSyncing, size: 8)
            }
        case .checking, .starting, .waitingForLoginItem:
            StatusDot(color: model.loginItemStalled ? Zone.alarm : Zone.muted(scheme), hollow: !model.loginItemStalled, size: 8)
        case .problem:
            StatusDot(color: Zone.alarm, size: 8)
        }
    }

    // MARK: Something needs the person

    /// The login item was asked to start zone and has not, for most of a minute.
    private var stalledBanner: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("zone's login item is not starting it, so your iPhone cannot reach this Mac. This app can run zone instead.")
                .font(.system(size: 11.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button(model.working.contains("takeover") ? "Starting" : "Run zone in this app") {
                    Task { await model.takeOver() }
                }
                .buttonStyle(ZonePillButtonStyle(filled: true))
                .disabled(model.working.contains("takeover"))
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

    /// Before the first pairing, pairing is the one thing to do.
    private var noPhone: some View {
        HStack(spacing: 10) {
            Image(systemName: "iphone")
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
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 4)
            Button("Pair") {
                navigation.page = .pair
            }
            .buttonStyle(ZonePillButtonStyle(filled: true))
        }
        .padding(10)
        .zoneCard(padding: 0)
    }

    // MARK: Recent items

    private var recent: some View {
        // The clip in the card above is not listed again.
        let items = model.recent.filter { $0.id != model.leadingClip?.id }.prefix(4)
        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 2) {
                Eyebrow("Latest").padding(.leading, 8)
                Spacer()
                HideTextButton(model: model, compact: true)
                if !items.isEmpty {
                    IconButton(symbol: "macwindow.on.rectangle", help: "Keep the latest in a floating window") {
                        model.closePopover()
                        model.showFloating()
                    }
                }
            }
            if items.isEmpty {
                Text(model.status == nil ? "Reading your library" : "Nothing here yet. What you say into Zone on your iPhone shows up here.")
                    .font(.system(size: 12))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
            } else {
                ForEach(items) { item in
                    ItemRow(model: model, item: item)
                }
            }
        }
    }

    // MARK: Actions

    private var actions: some View {
        HStack(spacing: 0) {
            ActionButton(symbol: "clock.arrow.circlepath", title: "History", working: model.working.contains("history")) {
                model.openHistory()
                model.closePopover()
            }
            ActionButton(symbol: "bolt", title: "Flash") {
                model.closePopover()
                model.openFlash()
            }
            ActionButton(symbol: "sparkles", title: "Agents") { navigation.page = .agents }
            ActionButton(symbol: "ellipsis", title: "More") { navigation.page = .settings }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
    }
}

/// A small round button with only a symbol, for the corner of a block.
private struct IconButton: View {
    let symbol: String
    let help: String
    let action: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(action: action) {
            HoverRow(radius: 6) { _ in
                Image(systemName: symbol)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                    .frame(width: 24, height: 22)
                    .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}

/// One paired phone, under More: how its syncing stands, and what can be done about it.
private struct PhoneRow: View {
    let model: AppModel
    let phone: StatusReport.Phone
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "iphone")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                    .frame(width: 18, height: 18)
                VStack(alignment: .leading, spacing: 1) {
                    Text(phone.name)
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(Zone.ink(scheme))
                        .lineLimit(1)
                    Text(detail)
                        .font(.system(size: 11).monospacedDigit())
                        .foregroundStyle(Zone.muted(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 6)
            Menu {
                Button("Ask \(phone.name) to send everything again") { model.resendEverything() }
                Divider()
                Button("Forget \(phone.name)") { model.forget(phone) }
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 13))
                    .foregroundStyle(Zone.muted(scheme))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Send everything again, or forget this phone")
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
    }

    private var detail: String {
        let items = "\(Formats.number(phone.itemsReceived)) \(phone.itemsReceived == 1 ? "item" : "items")"
        guard let age = phone.lastSyncAge else { return "Paired, waiting for its first sync" }
        var line = "Synced \(Formats.ago(age)), \(items)"
        if let route = ActivityWords.route(phone.lastPath) { line += ", \(route)" }
        return line
    }
}

private struct ActionButton: View {
    let symbol: String
    let title: String
    var working = false
    let action: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(action: action) {
            HoverRow(radius: 9) { _ in
                VStack(spacing: 4) {
                    Group {
                        if working {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: symbol)
                                .font(.system(size: 15, weight: .regular))
                        }
                    }
                    .frame(width: 30, height: 22)
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
                Text("On the iPhone: Zone, Settings, Computers.")
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
                section("iPhone") {
                    ForEach(model.status?.phones ?? [], id: \.id) { phone in
                        PhoneRow(model: model, phone: phone)
                        Hairline().padding(.leading, 38)
                    }
                    row("qrcode", (model.status?.phones.isEmpty ?? true) ? "Pair an iPhone" : "Pair another iPhone", "Zone on the iPhone: Settings, Computers.") {
                        Button("Pair") { navigation.page = .pair }
                            .buttonStyle(ZonePillButtonStyle())
                    }
                }
                section("Sync") {
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
                    if model.hasClips {
                        Hairline().padding(.leading, 38)
                        Toggle(isOn: Binding(get: { model.clipboardCopyOn }, set: { model.setClipboardCopy($0) })) {
                            label("doc.on.clipboard", "Put clips on the clipboard", clipboardLine)
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
                    row("hand.wave", "Setup guide", "") {
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
            return "Not set. Choose Zone's backup folder in iCloud Drive."
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
        return "Text you send from the iPhone, ready to paste here."
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
        case nil: return ""
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
                if !detail.isEmpty {
                    Text(detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Zone.muted(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                }
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
