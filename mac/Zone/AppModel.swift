import AppKit
import Observation
import ServiceManagement
import SwiftUI
import ZoneKit

/// Where pairing a phone stands.
enum PairingState: Equatable {
    case idle
    case loading
    /// The code is on screen and no phone has used it yet.
    case waiting(PairCode)
    case paired(name: String, deviceID: String)
    case expired
    case failed(String)
}

/// A check that an agent's entry really starts a zone that answers.
enum AgentTest: Equatable {
    case running
    case done(MCPProbe.Outcome)

    var outcome: MCPProbe.Outcome? {
        if case let .done(outcome) = self { return outcome }
        return nil
    }
}

/// Where zone's background service stands.
enum DaemonState: Equatable {
    /// The app just opened and has not looked yet.
    case checking
    case starting
    /// The login item from `zone install` is loaded and is expected to start zone.
    case waitingForLoginItem
    case running(DaemonOwner)
    case problem(String)
}

/// Everything the app knows and can do. The views read it and call it. All of zone's own
/// work is done by the `zone` program: this class runs it and reads what it prints.
@MainActor
@Observable
final class AppModel {
    // MARK: What the views read

    private(set) var env: ZoneEnvironment
    var state: AppState {
        didSet {
            if state != oldValue { state.save(to: env.appStateFile) }
        }
    }

    private(set) var status: StatusReport?
    private(set) var daemon: DaemonState = .checking
    private(set) var feed = ActivityFeed()
    private(set) var recent: [ZoneItem] = []
    private(set) var todayCount = 0
    /// The newest meeting, however long ago it was.
    private(set) var latestMeeting: ZoneItem?
    /// The newest notes no agent has flagged as handled, and how many such notes there are.
    private(set) var openNotes: [ZoneItem] = []
    private(set) var openNotesTotal = 0
    /// The newest items of one kind, read when the menu's list is narrowed to it.
    private(set) var kindLists: [String: [ZoneItem]] = [:]
    private(set) var agents: [Agent] = []
    /// The zone program the agents were asked about (see `agentsBinary`).
    private(set) var agentsZone: String?
    /// The last check of each place an agent starts zone from, by its settings file. The
    /// check of zone itself is under "zone".
    private(set) var agentTests: [String: AgentTest] = [:]
    private(set) var agentsLoaded = false
    private(set) var busyAgents: Set<String> = []
    var agentError: String?
    /// One plain line about the last change to an agent, when there is something to know.
    private(set) var agentNotice: String?
    private(set) var pairing: PairingState = .idle
    private(set) var syncingUntil = Date.distantPast
    /// One line shown for a few seconds after an action ("Copied").
    private(set) var notice: String?
    private(set) var copiedID: String?
    private(set) var loginItemOn = false
    /// macOS has the login item but wants the person to approve it in System Settings.
    private(set) var loginItemNeedsApproval = false
    /// Whether the zone this app runs may reach the phone directly on this network. Nil
    /// until Zone has looked (it looks only once it has asked once, after a pairing).
    private(set) var localNetwork: LocalNetworkAccess?
    private(set) var commandLink: CommandLink.State?
    private(set) var update: UpdateCheck.Answer?
    private(set) var checkingUpdate = false
    private(set) var zoneVersion: String?
    /// True when this app's zone has the `events` command.
    private(set) var usesEvents = false
    /// True when this app's zone has clips (text sent from the phone on purpose).
    private(set) var hasClips = false
    /// The newest clip, whenever it arrived.
    private(set) var latestClip: Clip?
    /// Whether a clip goes on this Mac's clipboard as it arrives. Nil until zone has said
    /// (and for a zone without clips).
    private(set) var clipboardCopy: ClipboardSetting?
    /// The program the old login item starts, when there is one.
    private(set) var legacyProgram: String?
    private(set) var working: Set<String> = []
    /// Ticks once a second so "2 min ago" stays true.
    private(set) var now = Date()

    /// Set by the windows: while someone is looking, the app asks zone more often.
    var watchers = 0

    // MARK: Hooks the app delegate fills in

    @ObservationIgnored var showWelcome: () -> Void = {}
    @ObservationIgnored var showFloating: () -> Void = {}
    @ObservationIgnored var showPopover: () -> Void = {}
    @ObservationIgnored var showPopoverPage: (PopoverNavigation.Page) -> Void = { _ in }
    @ObservationIgnored var closePopover: () -> Void = {}
    @ObservationIgnored var showFlash: () -> Void = {}
    /// Opens the history page in a Zone window, at a place in it.
    @ObservationIgnored var showHistory: (PageRoute) -> Void = { _ in }
    /// Opens a page of System Settings with the card that says what to switch on.
    @ObservationIgnored var showGuide: (GuideRequest) -> Void = { _ in }

    // MARK: Private

    @ObservationIgnored private let bundledBinary: URL?
    @ObservationIgnored private let daemonProcess = DaemonProcess()
    @ObservationIgnored private var source: ActivitySource?
    @ObservationIgnored private var sourceFollowsEvents: Bool?
    @ObservationIgnored private var recordsEventsCache: (pid: Int, records: Bool)?
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private let launched = Date()
    @ObservationIgnored private var refreshing = false
    @ObservationIgnored private var refreshQueued = false
    @ObservationIgnored private var libraryDirty = true
    @ObservationIgnored private var lastRefresh = Date.distantPast
    @ObservationIgnored private var ownerCache: (pid: Int, owner: DaemonOwner)?
    @ObservationIgnored private var daemonStartedAt = Date.distantPast
    @ObservationIgnored private var failures = 0
    @ObservationIgnored private var nextStartAllowed = Date.distantPast
    @ObservationIgnored private var quitting = false
    @ObservationIgnored private var canOpenFlash = false
    /// Since when the login item has been loaded with no zone running, and when the app last
    /// asked launchd to start it.
    @ObservationIgnored private var loginItemIdleSince: Date?
    @ObservationIgnored private var lastKickstart = Date.distantPast
    @ObservationIgnored private var pairingStarted = Date.distantPast
    @ObservationIgnored private var pairingKnown: [String: Double] = [:]
    @ObservationIgnored private var noticeToken = 0
    @ObservationIgnored private var userPath: String
    @ObservationIgnored private var lastLocalNetworkLook = Date.distantPast
    @ObservationIgnored private var lookingAtLocalNetwork = false
    @ObservationIgnored private var readingBackupFolder = false
    @ObservationIgnored private var backupFolderReadable = false
    @ObservationIgnored private var lastUpdateCheck = Date.distantPast
    /// The kind the menu's list is narrowed to, so it is read again when the library changes.
    @ObservationIgnored private var watchedKind: String?

    init(env: ZoneEnvironment, bundledBinary: URL?) {
        self.env = env
        self.bundledBinary = bundledBinary
        self.state = AppState.load(from: env.appStateFile)
        self.userPath = env.isSandbox
            ? "\(env.home.appendingPathComponent(".local/bin").path):/usr/bin:/bin"
            : env.searchPath
    }

    var client: ZoneClient {
        ZoneClient(runner: ZoneProcessRunner(binary: env.stableBinary, environment: env.childEnvironment()))
    }

    var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    }

    var isRunning: Bool {
        if case .running = daemon { return true }
        return false
    }

    var owner: DaemonOwner? {
        if case let .running(owner) = daemon { return owner }
        return nil
    }

    var isSyncing: Bool { now < syncingUntil }

    var menuBarState: MenuBarState {
        switch daemon {
        case .problem: return .problem
        case .checking, .starting, .waitingForLoginItem: return .stopped
        case .running: return isSyncing ? .syncing : .idle
        }
    }

    /// One short line for the top of the popover: which phone synced last and when, or
    /// what zone is doing instead.
    var headline: String {
        switch daemon {
        case .checking: return "Looking"
        case .starting: return "Starting"
        case .waitingForLoginItem: return loginItemStalled ? "zone is not starting" : "Starting"
        case let .problem(message): return message
        case .running:
            if isSyncing { return "Receiving" }
            guard let status else { return "Running" }
            return MenuContent.syncLine(status)
        }
    }

    /// What the headline leaves out, a line each: the phones, how the last sync travelled,
    /// what the library holds.
    var statusDetails: [String] {
        guard isRunning, let status else {
            return [loginItemStalled
                ? "zone is not running, so your iPhone cannot reach this Mac."
                : "zone is starting. Your iPhone reaches this Mac once it runs."]
        }
        return MenuContent.syncDetails(status, today: todayCount)
    }

    /// "Zone 0.1.2". The zone inside is named only when it is another version.
    var versionLine: String {
        Versions.line(app: appVersion, zone: zoneVersion, running: status?.daemon.running == true ? status?.daemon.version : nil)
    }

    // MARK: Starting up

    func start() async {
        var outcome: BinaryInstaller.Outcome?
        do {
            try FileManager.default.createDirectory(at: env.dataDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            if let bundledBinary {
                outcome = try BinaryInstaller.ensure(bundled: bundledBinary, stable: env.stableBinary)
            }
        } catch {
            daemon = .problem("Could not set up zone: \(error.localizedDescription)")
        }
        guard FileManager.default.isExecutableFile(atPath: env.stableBinary.path) else {
            daemon = .problem("This copy of Zone is missing its zone program.")
            startTimer()
            return
        }

        zoneVersion = try? await client.version()
        usesEvents = await client.hasEvents()
        hasClips = await client.hasClips()
        if hasClips { clipboardCopy = try? await client.clipboardSetting() }
        canOpenFlash = await client.hasFlashOpen()
        refreshLoginItem()
        legacyProgram = env.isSandbox ? nil : ProcessLookup.program(inLaunchAgent: env.legacyLaunchAgent)

        await refresh()
        // A copy of zone this app left running earlier is the old version now: start the new one.
        if outcome == .updated, case .running(.adopted) = daemon, let pid = status?.daemon.pid {
            await Task.detached { DaemonProcess.stop(pid: pid) }.value
            ownerCache = nil
            await refresh()
        }
        startTimer()

        if env.isSandbox {
            refreshCommandLink()
        } else {
            // An app opened from the Finder gets a short PATH. The person's terminal has the
            // real one, which is where their AI clients' commands are.
            let home = env.home
            let shell = ProcessInfo.processInfo.environment["SHELL"]
            if let path = await Task.detached(operation: { LoginShell.path(shell: shell, home: home) }).value {
                userPath = path
                env.searchPath = path
            }
            refreshCommandLink()
        }
        await loadAgents()
        await lookAtLocalNetwork()
    }

    /// Listens for what zone does, the way that fits the zone that is running. A zone that
    /// writes its events down is followed with `zone events`. A zone the app did not start
    /// may be an older one that only writes its log, so then the log is read: every version
    /// writes that. Changing over (after taking over, say) never shows a thing twice.
    private func chooseActivitySource() async {
        var followEvents = usesEvents
        if usesEvents, owner == .loginItem || owner == .other, let pid = status?.daemon.pid {
            if recordsEventsCache?.pid != pid {
                recordsEventsCache = (pid, await client.daemonRecordsEvents(pid: pid))
            }
            followEvents = recordsEventsCache?.records ?? false
        }
        guard followEvents != sourceFollowsEvents else { return }
        sourceFollowsEvents = followEvents
        source?.stop()
        let source: ActivitySource = followEvents
            ? EventsCommandSource(binary: env.stableBinary, environment: env.childEnvironment())
            : LogTailSource(file: env.logFile)
        self.source = source
        source.start { [weak self] event in
            Task { @MainActor in self?.take(event) }
        }
    }

    private func startTimer() {
        timer?.invalidate()
        let timer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func tick() {
        now = Date()
        var interval: TimeInterval = watchers > 0 ? 3 : 15
        if case .waiting = pairing { interval = 1 }
        if case .starting = daemon { interval = 1 }
        if case .waitingForLoginItem = daemon { interval = 2 }
        if now.timeIntervalSince(lastRefresh) >= interval {
            Task { await refresh() }
        }
        if case let .waiting(code) = pairing, now >= code.expiry {
            pairing = .expired
        }
        if now.timeIntervalSince(lastLocalNetworkLook) >= 300 {
            lastLocalNetworkLook = now
            Task { await lookAtLocalNetwork() }
        }
    }

    // MARK: Asking zone how things are

    func refresh() async {
        if refreshing {
            refreshQueued = true
            return
        }
        refreshing = true
        defer { refreshing = false }
        repeat {
            refreshQueued = false
            lastRefresh = Date()
            do {
                let fresh = try await client.status()
                if fresh.items.total != status?.items.total { libraryDirty = true }
                status = fresh
                await supervise(fresh)
                await chooseActivitySource()
            } catch {
                if case .checking = daemon {
                    daemon = .problem(error.localizedDescription)
                }
            }
            if libraryDirty {
                libraryDirty = false
                if let list = try? await client.recent(limit: 100) {
                    recent = list.items
                    nameKinds()
                }
                await readLatestClip()
                await readPinned()
            }
            if let today = try? await client.today() {
                todayCount = today.items
            }
            await watchPairing()
        } while refreshQueued
    }

    /// Decides who runs the service: starts the app's own when nothing is running, and
    /// otherwise works out whose the running one is.
    private func supervise(_ status: StatusReport) async {
        guard !quitting else { return }
        var seen = DaemonObservation(
            running: status.daemon.running,
            pid: status.daemon.pid,
            childPID: daemonProcess.pid,
            stableBinary: env.stableBinary.path
        )
        if seen.running, let pid = seen.pid {
            if let cached = ownerCache, cached.pid == pid {
                daemon = .running(cached.owner)
                return
            }
            seen.program = ProcessLookup.program(of: pid)
        }
        // A test run never looks at launchd: the old login item is part of the real setup.
        if seen.pid != seen.childPID || !seen.running, !env.isSandbox, FileManager.default.fileExists(atPath: env.legacyLaunchAgent.path) {
            let agent = await Task.detached { ProcessLookup.launchdAgent(label: ZoneEnvironment.legacyLabel) }.value
            seen.loginItemPID = agent.pid
            seen.loginItemLoaded = agent.loaded
        }
        let plan = DaemonPlanner.plan(seen)
        if plan != .waitForLoginItem { loginItemIdleSince = nil }
        switch plan {
        case let .attach(owner):
            if let pid = seen.pid { ownerCache = (pid, owner) }
            daemon = .running(owner)
        case .waitForLoginItem:
            ownerCache = nil
            daemon = .waitingForLoginItem
            await startLoginItemIfIdle()
        case .start:
            ownerCache = nil
            startOwnDaemon()
        }
    }

    /// The login item is loaded but no zone runs. launchd restarts a zone that crashed, ten
    /// seconds later, but leaves one stopped that exited successfully (on 2026-11-16 a test
    /// step's `pkill -f "zone serve"` stopped the real one, and it stayed stopped for five
    /// hours). After a short look the app asks launchd to start it, and again every 20
    /// seconds while it stays down.
    private func startLoginItemIfIdle() async {
        guard !env.isSandbox else { return }
        let since = loginItemIdleSince ?? Date()
        loginItemIdleSince = since
        guard Date().timeIntervalSince(since) >= 3, Date().timeIntervalSince(lastKickstart) >= 20 else { return }
        lastKickstart = Date()
        let label = ZoneEnvironment.legacyLabel
        await Task.detached { ProcessLookup.kickstartLaunchdAgent(label: label) }.value
        Task {
            try? await Task.sleep(for: .milliseconds(800))
            await refresh()
        }
    }

    /// The login item has been asked to start zone and still has not, for long enough that
    /// the person should know (and be offered to let this app run zone instead).
    var loginItemStalled: Bool {
        guard daemon == .waitingForLoginItem, let since = loginItemIdleSince else { return false }
        return now.timeIntervalSince(since) > 45
    }

    /// Waits a short while for zone to be running, for something asked for while it starts.
    func waitUntilRunning(seconds: Double = 30) async -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while !isRunning, Date() < deadline {
            await refresh()
            if isRunning { break }
            try? await Task.sleep(for: .milliseconds(500))
        }
        return isRunning
    }

    private func startOwnDaemon() {
        guard daemonProcess.pid == nil else {
            // Started a moment ago and not yet holding the data folder.
            daemon = .starting
            return
        }
        guard Date() >= nextStartAllowed else {
            daemon = failures >= 3 ? .problem("zone keeps stopping. Its log is in the data folder.") : .starting
            return
        }
        do {
            daemonStartedAt = Date()
            try daemonProcess.start(binary: env.stableBinary, environment: env.childEnvironment()) { [weak self] _ in
                Task { @MainActor in self?.daemonExited() }
            }
            daemon = .starting
            Task {
                try? await Task.sleep(for: .milliseconds(600))
                await refresh()
            }
        } catch {
            daemon = .problem("Could not start zone: \(error.localizedDescription)")
        }
    }

    private func daemonExited() {
        guard !quitting else { return }
        let ranFor = Date().timeIntervalSince(daemonStartedAt)
        failures = ranFor > 60 ? 0 : failures + 1
        nextStartAllowed = Date().addingTimeInterval(DaemonPlanner.restartDelay(afterFailures: failures - 1))
        ownerCache = nil
        Task { await refresh() }
    }

    /// Stops what this app runs. Called when the app quits.
    func shutdown() {
        quitting = true
        timer?.invalidate()
        source?.stop()
        switch daemon {
        case .running(.adopted):
            // The app's own copy from an earlier run: it is the app's to stop.
            daemonProcess.stop()
            if let pid = status?.daemon.pid { DaemonProcess.stop(pid: pid) }
        case .running(.loginItem), .running(.other):
            // Someone else's: it keeps running without the app.
            break
        default:
            daemonProcess.stop()
        }
    }

    /// Starts zone again after it kept stopping, without waiting out the pause.
    func startAgain() {
        failures = 0
        nextStartAllowed = .distantPast
        ownerCache = nil
        daemon = .starting
        Task { await refresh() }
    }

    /// Shows zone's log in the Finder (it is what explains a zone that keeps stopping).
    func showLog() {
        open(FileManager.default.fileExists(atPath: env.logFile.path) ? env.logFile : env.dataDirectory)
    }

    /// Takes over from a zone the app did not start: stops it (and removes the old login
    /// item when that is what started it), then starts the app's own.
    func takeOver() async {
        let fromLoginItem = owner == .loginItem || daemon == .waitingForLoginItem
        guard fromLoginItem || owner == .other else { return }
        working.insert("takeover")
        defer { working.remove("takeover") }
        if fromLoginItem {
            guard !env.isSandbox else { return }
            do {
                try await client.removeLoginItem()
            } catch {
                say("Could not remove the old login item: \(error.localizedDescription)")
                return
            }
        }
        if let pid = status?.daemon.pid, status?.daemon.running == true {
            await Task.detached { DaemonProcess.stop(pid: pid) }.value
        }
        legacyProgram = env.isSandbox ? nil : ProcessLookup.program(inLaunchAgent: env.legacyLaunchAgent)
        ownerCache = nil
        nextStartAllowed = .distantPast
        await refresh()
        say("This app now runs zone.")
    }

    // MARK: What just happened

    private func take(_ event: ActivityEvent) {
        guard feed.add(event) else { return }
        // Events from before the app opened are history: they fill the list, nothing more.
        guard event.date > launched.addingTimeInterval(-3) else { return }
        if event.broughtItems {
            syncingUntil = Date().addingTimeInterval(4)
            libraryDirty = true
        }
        if event.kind == .paired, case let .waiting(code) = pairing, event.date >= pairingStarted.addingTimeInterval(-1) {
            // When zone names its codes, only the phone that used this very code counts.
            let thisCode = code.id == nil || event.offer == nil || code.id == event.offer
            if thisCode {
                pairing = .paired(name: event.device ?? "Your iPhone", deviceID: event.deviceID ?? "")
                syncingUntil = Date().addingTimeInterval(4)
                Task { await askForLocalNetwork() }
            }
        }
        Task { await refresh() }
    }

    /// Turns "3 items" into "3 dictations" for small arrivals, by looking at what the
    /// library received around then.
    private func nameKinds() {
        for event in feed.events where event.kinds.isEmpty && event.broughtItems && event.kind != .backupImport {
            guard let arrived = event.arrived, arrived <= 60 else { continue }
            let kinds = ActivityWords.kinds(of: recent, receivedSince: event.date.addingTimeInterval(-8))
            if kinds.values.reduce(0, +) == arrived {
                feed.update(id: event.id) { $0.kinds = kinds }
            }
        }
    }

    // MARK: The last meeting, the open notes, a list of one kind

    /// What has a place of its own above the list: the newest meeting and the newest notes
    /// that nobody has handled yet. Read with `zone list --kind`, which every zone has.
    private func readPinned() async {
        if let meetings = try? await client.list(kinds: ["meeting"], limit: 1) {
            latestMeeting = meetings.items.first
        }
        if let notes = try? await client.list(kinds: ["note"], limit: 3, openOnly: true) {
            openNotes = notes.items
            openNotesTotal = notes.total
        }
        kindLists = [:]
        if let watchedKind { await loadKind(watchedKind) }
    }

    /// The menu's list was narrowed to a kind: read that kind's newest items. The latest
    /// hundred of everything rarely hold more than a few notes or meetings.
    func showKind(_ kind: String?) {
        watchedKind = kind
        guard let kind, kindLists[kind] == nil else { return }
        Task { await loadKind(kind) }
    }

    private func loadKind(_ kind: String) async {
        if let list = try? await client.list(kinds: [kind], limit: 60) {
            kindLists[kind] = list.items
        }
    }

    /// What is shown above the menu's list, so "All" does not list it a second time.
    var shownAboveList: Set<String> {
        var ids = Set(openNotes.map(\.id))
        if let latestMeeting { ids.insert(latestMeeting.id) }
        if let leadingClip { ids.insert(leadingClip.id) }
        return ids
    }

    // MARK: Clips

    /// The newest clip: from `zone clip`, or out of the latest items when that says nothing.
    private func readLatestClip() async {
        guard hasClips else { return }
        if let answer = try? await client.latestClip() {
            latestClip = answer
        } else if let item = recent.first(where: { $0.kind == "clip" }) {
            latestClip = Clip(item)
        }
    }

    /// The clip that leads the menu: the newest one, while it is fresh.
    var leadingClip: Clip? {
        guard let latestClip, latestClip.isRecent(now: now) else { return nil }
        return latestClip
    }

    /// The name of the phone a clip came from, when zone knows it.
    func phoneName(for clip: Clip) -> String? {
        let phones = status?.phones ?? []
        if let id = clip.deviceID, let phone = phones.first(where: { $0.endpointID == id }) { return phone.name }
        if let event = feed.events.first(where: { $0.kind == .clip && $0.clipID == clip.id }), let name = event.device { return name }
        return phones.count == 1 ? phones[0].name : nil
    }

    func copy(_ clip: Clip) {
        guard let text = clip.text else { return }
        putOnClipboard(text)
        markCopied(clip.id)
    }

    /// True when zone itself put this clip on the clipboard as it arrived.
    func wasPutOnClipboard(_ clip: Clip) -> Bool {
        feed.events.contains { $0.kind == .clip && $0.clipID == clip.id && $0.copied == true }
    }

    /// Asks zone how the setting stands (it can be changed from the command line too).
    func refreshClipboardSetting() async {
        guard hasClips, let setting = try? await client.clipboardSetting() else { return }
        clipboardCopy = setting
    }

    /// True when clips go on this Mac's clipboard as they arrive.
    var clipboardCopyOn: Bool { clipboardCopy?.enabled ?? false }

    /// The newest clip zone itself tried to put on the clipboard, and whether it could.
    var lastClipCopy: (copied: Bool, date: Date)? {
        guard let event = feed.events.first(where: { $0.kind == .clip && ($0.copied == true || $0.notCopied == "failed") }) else { return nil }
        return (event.copied == true, event.date)
    }

    func setClipboardCopy(_ on: Bool) {
        guard hasClips else { return }
        act("clipboard") { [self] in
            clipboardCopy = try await client.setClipboard(on)
        }
    }

    // MARK: Pairing

    func startPairing() async {
        guard pairing != .loading else { return }
        pairing = .loading
        do {
            let known = (try? await client.devices()) ?? []
            pairingKnown = Dictionary(uniqueKeysWithValues: known.map { ($0.endpointID, $0.pairedAt ?? 0) })
            pairingStarted = Date()
            let code = try await client.pair()
            if env.isSandbox {
                // A test run has no phone to scan the code, so the link is left where a
                // stand-in phone (scripts/dev-sandbox.sh phone) can pick it up.
                let file = env.dataDirectory.deletingLastPathComponent().appendingPathComponent("pair-url")
                try? code.url.write(to: file, atomically: true, encoding: .utf8)
            }
            pairing = .waiting(code)
        } catch {
            pairing = .failed(error.localizedDescription)
        }
    }

    func stopPairing() {
        pairing = .idle
    }

    /// While a code is on screen: has a phone that was not paired before turned up?
    private func watchPairing() async {
        guard case .waiting = pairing, let devices = try? await client.devices() else { return }
        let fresh = devices.first { device in
            guard let before = pairingKnown[device.endpointID] else { return true }
            return (device.pairedAt ?? 0) > before + 0.5
        }
        if let fresh, case .waiting = pairing {
            pairing = .paired(name: fresh.name, deviceID: fresh.endpointID)
            syncingUntil = Date().addingTimeInterval(4)
            libraryDirty = true
            refreshQueued = true
            Task { await askForLocalNetwork() }
        }
    }

    /// The phone that was just paired, as zone reports it now.
    var pairedPhone: StatusReport.Phone? {
        guard case let .paired(name, deviceID) = pairing else { return nil }
        return status?.phones.first { $0.endpointID == deviceID } ?? status?.phones.first { $0.name == name }
    }

    // MARK: AI agents

    /// The zone the agents are asked about and registered with: the one that runs on this
    /// Mac. That is this app's own copy, unless a login item from the command line runs
    /// zone, in which case it is that program (so the panel says what `zone mcp status`
    /// says in the terminal, and "Use this one" points an agent at the zone that runs).
    var agentsBinary: URL {
        guard !env.isSandbox, owner == .loginItem || daemon == .waitingForLoginItem,
              let legacyProgram, FileManager.default.isExecutableFile(atPath: legacyProgram)
        else { return env.stableBinary }
        return URL(fileURLWithPath: legacyProgram)
    }

    private var agentsClient: ZoneClient {
        ZoneClient(runner: ZoneProcessRunner(binary: agentsBinary, environment: env.childEnvironment()))
    }

    func loadAgents() async {
        var survey = try? await agentsClient.agents()
        if survey == nil, agentsBinary != env.stableBinary {
            // The other zone is too old to answer: this app's own can.
            survey = try? await client.agents()
        }
        if let survey {
            agents = Agent.list(from: survey, knowsClips: hasClips)
            agentsZone = survey.zone ?? agentsBinary.path
        }
        agentsLoaded = true
    }

    // MARK: Checking that an agent reaches the library

    /// What a program started by an AI client gets from its surroundings: the person's
    /// home and PATH, and nothing of zone's. Where the library is must come from the
    /// client's own entry, as it does when the client starts zone.
    private var probeEnvironment: [String: String] {
        env.childEnvironment().filter { !$0.key.hasPrefix("ZONE_") }
    }

    /// Starts zone exactly as each place of this agent would (the program and arguments in
    /// its settings file) and asks it what an MCP client asks first.
    func test(_ agent: Agent) {
        for place in agent.connectedPlaces {
            test(place: place.path) { AgentConfig.launch(client: place.client, path: place.path) }
        }
    }

    /// The same check of the zone that runs on this Mac, with no agent in between.
    func testZone() {
        let launch = AgentLaunch(command: agentsBinary.path, arguments: ["mcp"], environment: env.zoneHome.map { ["ZONE_HOME": $0.path] } ?? [:])
        test(place: "zone") { .success(launch) }
    }

    private func test(place: String, _ read: @escaping @Sendable () -> Result<AgentLaunch, AgentConfigError>) {
        guard agentTests[place] != .running else { return }
        agentTests[place] = .running
        let base = probeEnvironment, version = appVersion
        Task {
            let outcome = await Task.detached { () -> MCPProbe.Outcome in
                switch read() {
                case let .success(launch): return MCPProbe.run(launch, baseEnvironment: base, clientVersion: version)
                case let .failure(error): return .failed(error.localizedDescription)
                }
            }.value
            agentTests[place] = .done(outcome)
        }
    }

    /// The checks of an agent's places, in one line: "Answers: 6 tools", or what went
    /// wrong and in how many. Nil before any check.
    func testSummary(_ agent: Agent) -> (text: String, good: Bool, running: Bool)? {
        let places = agent.connectedPlaces
        let tests = places.compactMap { agentTests[$0.path] }
        guard !tests.isEmpty else { return nil }
        if tests.contains(.running) { return ("Asking", true, true) }
        let outcomes = tests.compactMap(\.outcome)
        let failed = outcomes.filter { !$0.answered }
        if failed.isEmpty, let first = outcomes.first {
            return (places.count > 1 ? "\(first.words) in all \(places.count) profiles" : first.words, true, false)
        }
        if places.count == 1, let only = failed.first { return (only.words, false, false) }
        return ("\(failed.count) of \(places.count) profiles gave no answer", false, false)
    }

    /// Connects or disconnects an agent. Connecting one that is already connected (to point
    /// it at this app's zone, or to add its other profiles) keeps what it may read.
    func setAgent(_ agent: Agent, connected: Bool) async {
        // zone cannot say what an entry that starts another copy of zone may read, so the
        // app cannot carry a limit over. It says so instead of widening it silently.
        var unknownView = false
        if connected, !agent.otherCopyPlaces.isEmpty, !agent.otherCopyKeepsView { unknownView = true }
        await changeAgent(agent) { [client = agentsClient] in
            if connected {
                try await client.connect(agent)
            } else {
                try await client.disconnect(agent: agent.id)
            }
        }
        if unknownView, agentError == nil {
            agentNotice = "\(agent.title) now starts the zone that runs on this Mac and can read everything. If it was limited before, limit it again under its name."
        }
    }

    /// Changes what a connected agent may read (in every profile it has).
    func setAgentView(_ agent: Agent, _ view: AgentView) async {
        guard agent.isOn, agent.view != view || agent.viewsDiffer else { return }
        await changeAgent(agent) { [client = agentsClient] in
            try await client.connect(agent, view: view)
        }
    }

    private func changeAgent(_ agent: Agent, _ work: @escaping () async throws -> Void) async {
        guard !busyAgents.contains(agent.id) else { return }
        busyAgents.insert(agent.id)
        agentError = nil
        agentNotice = nil
        do {
            try await work()
        } catch {
            agentError = "\(agent.title): \(error.localizedDescription)"
        }
        await loadAgents()
        // What was checked before the change says nothing about the entry as it is now.
        for place in agent.places { agentTests[place.path] = nil }
        busyAgents.remove(agent.id)
    }

    func connectAllAgents() async {
        for agent in agents where agent.installed && agent.standing != .connected {
            await setAgent(agent, connected: true)
        }
    }

    // MARK: Actions

    func copy(_ item: ZoneItem) {
        putOnClipboard(item.copyText)
        markCopied(item.id)
    }

    /// Copies text. A test run writes the sandbox's `clipboard` file instead, so it never
    /// replaces what the person using the Mac has copied.
    private func putOnClipboard(_ text: String) {
        if let file = env.pretendClipboard {
            try? text.write(to: file, atomically: true, encoding: .utf8)
            return
        }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private func markCopied(_ id: String) {
        copiedID = id
        Task {
            try? await Task.sleep(for: .seconds(1.4))
            if copiedID == id { copiedID = nil }
        }
    }

    /// The history page in a Zone window, at a place in it: a meeting, the notes, or the
    /// page as it opens by itself. The window opens at once; while zone starts, it says so.
    func openHistory(_ route: PageRoute = .home) {
        showHistory(route)
    }

    /// The history page's address with its key, pointed at a place in the page.
    func historyLink(_ route: PageRoute) async throws -> URL {
        guard await waitUntilRunning() else { throw ZoneError.failed("zone did not start, so the history page cannot open.") }
        return PageRoutes.url(page: try await client.historyLink(), route: route)
    }

    /// The history page in the web browser, as before the Zone window.
    func openHistoryInBrowser(_ route: PageRoute = .home) {
        act("history") { [self] in
            open(try await historyLink(route))
        }
    }

    /// Flash in a Zone window. It opens at once; while zone starts, the window says so.
    func openFlash() {
        showFlash()
    }

    /// The Flash page's address with its key, the way this zone gives it: in one step, or (an
    /// older zone) the history page first, which hands over the key, then Flash.
    func flashLink() async throws -> (url: URL, port: Int?, first: URL?) {
        guard await waitUntilRunning() else { throw ZoneError.failed("zone did not start. Flash needs it running on this Mac to read the codes.") }
        if canOpenFlash {
            let url = try await client.flashLink()
            return (url, url.port, nil)
        }
        let link = try await client.historyLink()
        guard let flash = URL(string: "/flash", relativeTo: link)?.absoluteURL else { throw ZoneError.unreadable("open --print") }
        return (flash, link.port, link)
    }

    /// Flash in the web browser, as before the Zone window.
    func openFlashInBrowser() {
        act("flash") { [self] in
            guard await waitUntilRunning() else { throw ZoneError.failed("zone did not start, so Flash cannot open.") }
            if canOpenFlash {
                try await client.openFlash()
                return
            }
            // An older zone: unlock the page first, then go to Flash.
            let link = try await client.historyLink()
            open(link)
            try await Task.sleep(for: .seconds(1.5))
            if let flash = URL(string: "/flash", relativeTo: link)?.absoluteURL {
                open(flash)
            }
        }
    }

    func resendEverything() {
        act("resync") { [self] in
            guard status?.phones.isEmpty == false else { throw ZoneError.failed("No phone is paired yet.") }
            try await client.resync()
            say("Asked. Your iPhone sends everything again at its next sync.")
        }
    }

    func setBackupFolder(_ folder: URL?) {
        act("backup") { [self] in
            if let folder {
                try await client.setBackupFolder(folder)
                say("Watching \(Formats.tilde(folder.path, home: env.home.path)).")
            } else {
                try await client.clearBackupFolder()
                say("No longer watching a backup folder.")
            }
            libraryDirty = true
            await refresh()
        }
    }

    func forget(_ phone: StatusReport.Phone) {
        act("unpair") { [self] in
            try await client.unpair(phone.endpointID)
            say("\(phone.name) is forgotten. Its items stay.")
            await refresh()
        }
    }

    /// Runs an action, marks it as working for the views, and says what went wrong.
    private func act(_ name: String, _ work: @escaping () async throws -> Void) {
        guard !working.contains(name) else { return }
        working.insert(name)
        Task {
            do {
                try await work()
            } catch {
                say(error.localizedDescription)
            }
            working.remove(name)
        }
    }

    func say(_ text: String) {
        notice = text
        noticeToken += 1
        let token = noticeToken
        Task {
            try? await Task.sleep(for: .seconds(4))
            if noticeToken == token { notice = nil }
        }
    }

    /// Opens a link in the browser, or a folder in the Finder. A test run notes it in the
    /// sandbox's `opened` file instead, so nothing comes to the front on the Mac.
    func open(_ url: URL) {
        guard env.isSandbox else {
            if url.isFileURL {
                NSWorkspace.shared.activateFileViewerSelecting([url])
            } else {
                NSWorkspace.shared.open(url)
            }
            return
        }
        let file = env.dataDirectory.deletingLastPathComponent().appendingPathComponent("opened")
        let line = Data((url.absoluteString + "\n").utf8)
        if let handle = try? FileHandle(forWritingTo: file) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: line)
        } else {
            try? line.write(to: file)
        }
    }

    // MARK: Start at login

    func refreshLoginItem() {
        if env.isSandbox {
            loginItemOn = state.pretendLoginItem
            return
        }
        let status = SMAppService.mainApp.status
        loginItemOn = status == .enabled
        loginItemNeedsApproval = status == .requiresApproval
    }

    func setLoginItem(_ on: Bool) {
        if env.isSandbox {
            // A test run never adds a real login item.
            state.pretendLoginItem = on
        } else {
            do {
                if on {
                    try SMAppService.mainApp.register()
                } else {
                    try SMAppService.mainApp.unregister()
                }
            } catch {
                // macOS refuses a login item the person switched off in System Settings until
                // they switch it on there.
                if !(on && SMAppService.mainApp.status == .requiresApproval) {
                    say("Could not change the login item: \(error.localizedDescription)")
                }
            }
        }
        refreshLoginItem()
        if on, loginItemNeedsApproval {
            approveLoginItem()
        }
    }

    /// Opens Login Items in System Settings with the card on it.
    func approveLoginItem() {
        showGuide(GuideRequest(
            permission: .loginItems,
            isGranted: { SMAppService.mainApp.status == .enabled },
            onGranted: { [weak self] in self?.refreshLoginItem() }
        ))
    }

    // MARK: The zone command

    func refreshCommandLink() {
        commandLink = CommandLink.state(path: userPath, home: env.home, stableBinary: env.stableBinary)
    }

    var commandLinked: Bool {
        if case .linked = commandLink { return true }
        return false
    }

    /// Puts `zone` on the PATH. `replace` allows taking the place of another zone.
    func installCommand(replace: Bool = false) {
        defer { refreshCommandLink() }
        do {
            switch commandLink {
            case let .absent(suggested, needsAdmin):
                if needsAdmin {
                    try installCommandAsAdministrator(at: suggested)
                } else {
                    try CommandLink.install(at: suggested, stableBinary: env.stableBinary)
                }
                say("zone is now a command in new terminal windows.")
            case let .foreign(existing) where replace:
                try CommandLink.install(at: existing, stableBinary: env.stableBinary)
                say("The zone command now runs this app's zone.")
            default:
                break
            }
        } catch {
            say("Could not add the command: \(error.localizedDescription)")
        }
    }

    func removeCommand() {
        if case let .linked(link) = commandLink {
            try? CommandLink.remove(at: link, stableBinary: env.stableBinary)
        }
        refreshCommandLink()
    }

    private func installCommandAsAdministrator(at link: URL) throws {
        guard !env.isSandbox else {
            throw ZoneError.failed("A test run does not ask for an administrator password.")
        }
        let script = CommandLink.adminScript(link: link, stableBinary: env.stableBinary)
        let escaped = script.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        var failure: NSDictionary?
        NSAppleScript(source: "do shell script \"\(escaped)\" with administrator privileges")?.executeAndReturnError(&failure)
        if let failure {
            let message = failure[NSAppleScript.errorMessage] as? String ?? "macOS said no."
            throw ZoneError.failed(message)
        }
    }

    // MARK: Updates

    /// Asks GitHub which release is the latest. A test run can be pointed at a file or a
    /// local server that stands in for GitHub.
    func checkForUpdates() async {
        guard !checkingUpdate else { return }
        checkingUpdate = true
        lastUpdateCheck = Date()
        let feed = env.isSandbox ? ProcessInfo.processInfo.environment["ZONE_MAC_UPDATE_FEED"].flatMap(URL.init(string:)) : nil
        update = await UpdateCheck.check(current: appVersion, feed: feed)
        checkingUpdate = false
    }

    /// Settings came into view: check, unless that was done a little while ago.
    func checkForUpdatesIfStale() async {
        guard Date().timeIntervalSince(lastUpdateCheck) > 15 * 60 else { return }
        await checkForUpdates()
    }

    /// The one line about updates and the one thing to click.
    var updateLine: UpdateLine {
        UpdateLine(answer: update, checking: checkingUpdate, current: appVersion)
    }

    func followUpdateLine() {
        switch updateLine.action {
        case let .update(page): open(page)
        case .checkAgain: Task { await checkForUpdates() }
        case nil: break
        }
    }

    // MARK: What macOS has to allow

    /// True when this app runs zone, so what macOS allows Zone is what zone may do.
    var runsZone: Bool {
        owner == .app || owner == .adopted
    }

    /// Looks again whether zone may reach the local network, once Zone has asked before (so
    /// this never brings up macOS's question by itself).
    func lookAtLocalNetwork() async {
        guard state.localNetworkAsked, !env.isSandbox || ProcessInfo.processInfo.environment["ZONE_MAC_PROBE_LOCAL_NETWORK"] != nil else { return }
        await probeLocalNetwork()
    }

    /// Right after a phone pairs (the person is at the Mac and has just scanned a code), let
    /// macOS ask whether Zone may reach the phone directly. zone would make it ask at some
    /// moment anyway, the first time it tries the phone's address on this network.
    func askForLocalNetwork() async {
        guard runsZone, !env.isSandbox || ProcessInfo.processInfo.environment["ZONE_MAC_PROBE_LOCAL_NETWORK"] != nil else { return }
        let first = !state.localNetworkAsked
        state.localNetworkAsked = true
        await probeLocalNetwork()
        if first, localNetwork == .denied {
            // macOS may say no while its question is still on the screen. Look again later.
            localNetwork = nil
            lastLocalNetworkLook = Date().addingTimeInterval(-240)
        }
    }

    private func probeLocalNetwork() async {
        guard !lookingAtLocalNetwork else { return }
        lookingAtLocalNetwork = true
        defer { lookingAtLocalNetwork = false }
        lastLocalNetworkLook = Date()
        let answer = await LocalNetworkProbe.check()
        if answer != .unknown { localNetwork = answer }
    }

    func allowLocalNetwork() {
        showGuide(GuideRequest(
            permission: .localNetwork,
            isGranted: { [weak self] in self?.localNetwork == .allowed },
            refresh: { [weak self] in Task { await self?.probeLocalNetwork() } }
        ))
    }

    /// The backup folder is set and macOS has not let zone read it (or is asking).
    var backupFolderBlocked: Bool {
        guard runsZone, let backup = status?.backupState else { return false }
        switch backup.state {
        case "waiting": return (backup.waiting ?? 0) >= 3
        case "error": return isPermissionRefusal(backup.message)
        default: return false
        }
    }

    func allowBackupFolder() {
        guard let folder = status?.backupFolder else { return }
        backupFolderReadable = false
        showGuide(GuideRequest(
            permission: .filesAndFolders,
            folder: ProtectedFolder.containing(folder, home: env.home.path),
            isGranted: { [weak self] in self?.backupFolderReadable ?? false },
            refresh: { [weak self] in self?.tryReadingBackupFolder(folder) },
            onGranted: { [weak self] in
                // zone looks at the folder every half minute; ask it to look now.
                self?.setBackupFolder(URL(fileURLWithPath: folder))
            }
        ))
    }

    /// Reads the backup folder as Zone (which is who zone reads it as). One read at a time:
    /// while macOS is asking, a read waits for the answer.
    private func tryReadingBackupFolder(_ folder: String) {
        guard !readingBackupFolder else { return }
        readingBackupFolder = true
        Task.detached {
            let readable = (try? FileManager.default.contentsOfDirectory(atPath: folder)) != nil
            await MainActor.run {
                self.backupFolderReadable = readable
                self.readingBackupFolder = false
            }
        }
    }

    /// The one thing macOS should allow that is in the way right now, for the main page.
    var permissionIssue: (text: String, permission: Permission)? {
        if backupFolderBlocked {
            return ("macOS has not let Zone read your backup folder yet.", .filesAndFolders)
        }
        if runsZone, localNetwork == .denied {
            return ("Your iPhone cannot reach this Mac directly on this network, so syncs go through the relay.", .localNetwork)
        }
        if loginItemNeedsApproval {
            return ("Zone is waiting for you to allow it in Login Items, so it can start when you log in.", .loginItems)
        }
        return nil
    }

    func allow(_ permission: Permission) {
        switch permission {
        case .filesAndFolders: allowBackupFolder()
        case .localNetwork: allowLocalNetwork()
        case .loginItems: approveLoginItem()
        case .camera: showFlash()
        }
    }

    /// Test runs only: shows what the app does in a state macOS would have to put it in.
    func pretend(_ setting: String) {
        guard env.isSandbox else { return }
        switch setting {
        case "localNetwork=denied": localNetwork = .denied
        case "localNetwork=allowed": localNetwork = .allowed
        case "loginItem=approval": loginItemNeedsApproval = true
        default: break
        }
    }

    // MARK: Welcome

    func finishWelcome() {
        state.welcomeDone = true
        state.welcomeStep = 0
        pairing = .idle
    }
}
