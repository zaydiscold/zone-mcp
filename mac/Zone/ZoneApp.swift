import AppKit
import SwiftUI
import ZoneKit

@main
enum ZoneMain {
    @MainActor
    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        // A menu bar app: no Dock icon, no main menu.
        app.setActivationPolicy(.accessory)
        app.run()
    }
}

/// A window that stays where a test run parks it. macOS otherwise pulls a window with a
/// title bar back on to the screen.
final class ParkableWindow: NSWindow {
    var parked = false

    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
        parked ? frameRect : super.constrainFrameRect(frameRect, to: screen)
    }
}

final class ParkablePanel: NSPanel {
    var parked = false

    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
        parked ? frameRect : super.constrainFrameRect(frameRect, to: screen)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private var model: AppModel!
    private var statusItem: StatusItemController!
    private var welcomeWindow: NSWindow?
    private var floatingWindow: NSPanel?
    private var flash: FlashWindowController?
    private var history: HistoryWindowController?
    private let guide = PermissionGuide()
    private var commandTimer: Timer?
    /// A test run can ask the app to stay out of the way of whoever is using the Mac: its
    /// windows never take the keyboard and sit almost entirely off the edge of the screen
    /// (see `parkedOrigin`), where they can still be photographed.
    private var quiet = false
    private var signals: [DispatchSourceSignal] = []
    private var testActivity: NSObjectProtocol?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let variables = ProcessInfo.processInfo.environment
        let env = ZoneEnvironment.resolve(from: variables, home: FileManager.default.homeDirectoryForCurrentUser)
        quiet = env.isSandbox && variables["ZONE_MAC_QUIET"] != nil
        if env.isSandbox {
            // A test run is steered by a file it looks at five times a second. With its
            // windows off the screen (or the screen locked) macOS would slow that to a crawl.
            testActivity = ProcessInfo.processInfo.beginActivity(options: .userInitiatedAllowingIdleSystemSleep, reason: "A test run is being steered")
        }

        // One Zone at a time (a test run in its own folder may sit beside the real one).
        if !env.isSandbox, let id = Bundle.main.bundleIdentifier {
            let others = NSRunningApplication.runningApplications(withBundleIdentifier: id).filter { $0 != .current }
            if !others.isEmpty {
                NSApp.terminate(nil)
                return
            }
        }

        switch variables["ZONE_MAC_APPEARANCE"] {
        case "dark": NSApp.appearance = NSAppearance(named: .darkAqua)
        case "light": NSApp.appearance = NSAppearance(named: .aqua)
        default: break
        }

        // The zone program is inside the app, in Contents/Helpers (not beside the app's own
        // program: "zone" and "Zone" are one name on a disk that ignores case). A build
        // without one can be pointed at a zone with ZONE_MAC_BINARY.
        let helper = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/zone")
        let bundled = variables["ZONE_MAC_BINARY"].map { URL(fileURLWithPath: $0) }
            ?? (FileManager.default.isExecutableFile(atPath: helper.path) ? helper : nil)

        model = AppModel(env: env, bundledBinary: bundled)
        statusItem = StatusItemController(model: model)
        statusItem.parked = quiet
        quitCleanlyOnSignals()
        model.showWelcome = { [weak self] in self?.showWelcome() }
        model.showFloating = { [weak self] in self?.showFloating() }
        model.showPopover = { [weak self] in self?.statusItem.show(takeKeyboard: !(self?.quiet ?? false)) }
        model.showPopoverPage = { [weak self] page in
            guard let self else { return }
            self.statusItem.close()
            self.statusItem.show(page: page, takeKeyboard: !self.quiet)
        }
        model.closePopover = { [weak self] in self?.statusItem.close() }
        model.showFlash = { [weak self] in self?.showFlash() }
        model.showHistory = { [weak self] route in self?.showHistory(route) }
        // A test run that stays out of the way never opens System Settings: the link is
        // noted in the sandbox's `opened` file and the card is parked like the windows.
        guide.parked = quiet
        guide.openLink = { [weak self] url in self?.model.open(url) }
        model.showGuide = { [weak self] request in self?.guide.show(request) }

        Task {
            await model.start()
            if !model.state.welcomeDone {
                showWelcome()
            }
            if env.isSandbox {
                for command in (variables["ZONE_MAC_SHOW"] ?? "").split(separator: ",") {
                    run(command: String(command))
                }
                watchCommands(in: env)
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        flash?.window.close()
        history?.window.close()
        guide.close()
        model?.shutdown()
    }

    /// `kill`, a logout or a shutdown ends the app with a signal, which by default skips
    /// applicationWillTerminate. Quit the ordinary way instead, so the zone this app started
    /// is stopped and nothing is left running behind it.
    private func quitCleanlyOnSignals() {
        for number in [SIGTERM, SIGINT, SIGHUP] {
            signal(number, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: number, queue: .main)
            source.setEventHandler { NSApp.terminate(nil) }
            source.resume()
            signals.append(source)
        }
    }

    /// Opening the app again while it runs (from the Finder or Spotlight) shows something,
    /// since there is no Dock icon to click.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if model.state.welcomeDone {
            statusItem.show()
        } else {
            showWelcome()
        }
        return false
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    // MARK: Windows

    private func present(_ window: NSWindow) {
        if quiet {
            (window as? ParkableWindow)?.parked = true
            (window as? ParkablePanel)?.parked = true
            window.level = .floating
            window.setFrameOrigin(StatusItemController.parkedOrigin(for: window.frame.size))
            window.orderFrontRegardless()
        } else {
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
        }
    }

    func showWelcome() {
        if let welcomeWindow {
            present(welcomeWindow)
            return
        }
        let window = ParkableWindow(
            contentRect: NSRect(origin: .zero, size: WelcomeView.size),
            styleMask: [.titled, .closable, .miniaturizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Welcome to Zone"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.isMovableByWindowBackground = true
        window.isReleasedWhenClosed = false
        window.backgroundColor = NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(srgbRed: 0x2A / 255, green: 0x2A / 255, blue: 0x2A / 255, alpha: 1)
                : NSColor(srgbRed: 0xF1 / 255, green: 0xE9 / 255, blue: 0xD2 / 255, alpha: 1)
        }
        window.delegate = self
        let hosting = NSHostingView(rootView: WelcomeView(model: model) { [weak self] finished in
            self?.welcomeWindow?.close()
            if finished {
                // Show where Zone lives now that the window is gone.
                self?.model.showPopover()
            }
        })
        // The window keeps the size it is given: the view fills it, title bar included.
        hosting.sizingOptions = []
        window.contentView = hosting
        // With the content under the title bar, the window itself is the size of the view.
        window.setFrame(NSRect(origin: .zero, size: WelcomeView.size), display: false)
        window.center()
        welcomeWindow = window
        model.watchers += 1
        present(window)
    }

    func showFloating() {
        if let floatingWindow {
            present(floatingWindow)
            return
        }
        let panel = ParkablePanel(
            contentRect: NSRect(x: 0, y: 0, width: 360, height: 520),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.title = "Zone"
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isFloatingPanel = true
        // Clicking a row to copy it leaves the keyboard with the app the person is working
        // in. Only the search field takes it.
        panel.becomesKeyOnlyIfNeeded = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.isMovableByWindowBackground = true
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.standardWindowButton(.miniaturizeButton)?.isHidden = true
        panel.standardWindowButton(.zoomButton)?.isHidden = true
        panel.minSize = NSSize(width: 344, height: 280)
        panel.delegate = self
        panel.contentView = NSHostingView(rootView: FloatingView(model: model))
        panel.setContentSize(NSSize(width: 360, height: 520))
        if let screen = NSScreen.main {
            let visible = screen.visibleFrame
            panel.setFrameOrigin(NSPoint(x: visible.maxX - 360 - 24, y: visible.maxY - 520 - 24))
        }
        if !model.env.isSandbox {
            // Comes back where the person left it.
            panel.setFrameAutosaveName("ZoneFloating")
        }
        floatingWindow = panel
        model.watchers += 1
        present(panel)
    }

    /// Flash in a window of Zone's own (see FlashWindow.swift).
    func showFlash() {
        if let flash {
            present(flash.window)
            return
        }
        let controller = FlashWindowController(model: model, quiet: quiet)
        controller.onClose = { [weak self] in
            self?.flash = nil
            self?.model.watchers = max(0, (self?.model.watchers ?? 1) - 1)
        }
        flash = controller
        model.watchers += 1
        present(controller.window)
    }

    /// The history page in a window of Zone's own (see HistoryWindow.swift), at a place in it.
    func showHistory(_ route: PageRoute) {
        if let history {
            history.go(to: route)
            present(history.window)
            return
        }
        let controller = HistoryWindowController(model: model, route: route)
        controller.onClose = { [weak self] in
            self?.history = nil
            self?.model.watchers = max(0, (self?.model.watchers ?? 1) - 1)
        }
        history = controller
        model.watchers += 1
        present(controller.window)
    }

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow else { return }
        model.watchers = max(0, model.watchers - 1)
        if window === welcomeWindow {
            welcomeWindow = nil
            // Closing the window with a pair code on it gives the code up.
            if case .paired = model.pairing {} else { model.stopPairing() }
            if !model.state.welcomeDone {
                // Closed half way: the guide is done for now and can be opened again from the menu.
                model.state.welcomeDone = true
            }
        } else if window === floatingWindow {
            floatingWindow = nil
        }
    }

    // MARK: Test runs

    // A test run (ZONE_MAC_SANDBOX) can be steered without the mouse: a line written to the
    // file `command` in the sandbox folder is carried out and the file is removed. That is how
    // the screenshots in the README are taken without interrupting whoever is using the Mac.

    private func watchCommands(in env: ZoneEnvironment) {
        let file = env.dataDirectory.deletingLastPathComponent().appendingPathComponent("command")
        let timer = Timer(timeInterval: 0.2, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let text = try? String(contentsOf: file, encoding: .utf8) else { return }
                try? FileManager.default.removeItem(at: file)
                for line in text.split(whereSeparator: \.isNewline) {
                    self?.run(command: String(line))
                }
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        commandTimer = timer
    }

    private func run(command: String) {
        let parts = command.trimmingCharacters(in: .whitespaces).split(separator: ":", maxSplits: 1).map(String.init)
        guard let name = parts.first else { return }
        let argument = parts.count > 1 ? parts[1] : nil
        switch name {
        case "popover":
            if argument == "close" {
                statusItem.close()
            } else {
                let page = argument.flatMap(PopoverNavigation.Page.init(rawValue:)) ?? .main
                statusItem.navigation.page = page
                statusItem.show(page: page, takeKeyboard: !quiet)
            }
        case "welcome":
            if argument == "close" {
                welcomeWindow?.close()
            } else {
                if let step = argument.flatMap(Int.init) {
                    model.state.welcomeStep = step
                    welcomeWindow?.close()
                }
                showWelcome()
            }
        case "floating":
            if argument == "close" {
                floatingWindow?.close()
            } else {
                showFloating()
            }
        case "flash":
            switch argument {
            case "close": flash?.window.close()
            case "minimize": flash?.window.miniaturize(nil)
            case "restore": flash?.window.deminiaturize(nil)
            default: showFlash()
            }
        case "history":
            // history opens the page, history:notes, history:settings and
            // history:meeting=ID open it at that place.
            switch argument {
            case "close": history?.window.close()
            case "notes": showHistory(.notes)
            case "settings": showHistory(.settings)
            case let place? where place.hasPrefix("meeting="): showHistory(.meeting(String(place.dropFirst(8))))
            default: showHistory(.home)
            }
        case "historyjs":
            // historyjs:/path/answer.txt=return location.hash runs a script in the history page.
            if let argument, let equals = argument.firstIndex(of: "="), let history {
                let file = URL(fileURLWithPath: String(argument[..<equals]))
                let script = String(argument[argument.index(after: equals)...])
                Task {
                    let answer = await history.evaluate(script)
                    try? (answer + "\n").write(to: file, atomically: true, encoding: .utf8)
                }
            }
        case "segment":
            // segment:note narrows the menu's list to a kind (all, note, meeting, dictation, clip).
            if let segment = argument.flatMap(ListSegment.init(rawValue:)) {
                statusItem.navigation.segment = segment
                model.showKind(segment.kind)
            }
        case "details":
            // details:on opens what the line at the top of the menu leaves out.
            statusItem.navigation.detailsOpen = argument != "off"
        case "agent":
            // agent:claude-code opens that app's profiles on the Agents page, agent:off
            // opens the apps that are not connected, agent:close folds everything.
            switch argument {
            case "close": statusItem.navigation.openAgent = nil; statusItem.navigation.offAgentsOpen = false
            case "off": statusItem.navigation.offAgentsOpen = true
            default: statusItem.navigation.openAgent = argument
            }
        case "test":
            // test:zone checks the zone that runs here, test:cursor each place of that agent.
            if argument == "zone" {
                model.testZone()
            } else if let agent = model.agents.first(where: { $0.id == argument }) {
                model.test(agent)
            }
        case "connect":
            // connect:opencode is the agent's switch (or "Use this one"), connect:opencode=off
            // turns it off.
            if let argument {
                let parts = argument.split(separator: "=").map(String.init)
                if let agent = model.agents.first(where: { $0.id == parts[0] }) {
                    Task { await model.setAgent(agent, connected: parts.last != "off") }
                }
            }
        case "agents":
            // agents reads the agents' settings again (after a test changed them from outside).
            Task { await model.loadAgents() }
        case "update":
            Task { await model.checkForUpdates() }
        case "flashjs":
            // flashjs:/path/answer.txt=return document.title runs a script in the Flash page
            // and writes what it returned to that file.
            if let argument, let equals = argument.firstIndex(of: "="), let flash {
                let file = URL(fileURLWithPath: String(argument[..<equals]))
                let script = String(argument[argument.index(after: equals)...])
                Task {
                    let answer = await flash.evaluate(script)
                    try? (answer + "\n").write(to: file, atomically: true, encoding: .utf8)
                }
            }
        case "guide":
            // guide:camera (or localNetwork, filesAndFolders, loginItems) shows the card for
            // that list; guide:close closes it.
            // guide:camera,fly lifts the tile out of the middle of the screen, as a click there
            // in a Zone window would.
            let parts = (argument ?? "").split(separator: ",").map(String.init)
            if argument == "close" {
                guide.close()
            } else if let permission = parts.first.flatMap(Permission.init(rawValue:)) {
                // The switch counts as on once the file `granted` appears in the sandbox folder.
                let granted = model.env.dataDirectory.deletingLastPathComponent().appendingPathComponent("granted")
                try? FileManager.default.removeItem(at: granted)
                let request = GuideRequest(
                    permission: permission,
                    folder: permission == .filesAndFolders ? .iCloudDrive : nil,
                    isGranted: { FileManager.default.fileExists(atPath: granted.path) }
                )
                if parts.contains("fly"), let screen = NSScreen.main {
                    guide.show(request, flyFrom: NSPoint(x: screen.frame.midX, y: screen.frame.midY))
                } else {
                    model.showGuide(request)
                }
            }
        case "pretend":
            // pretend:localNetwork=denied shows what the app does when macOS says no.
            model.pretend(argument ?? "")
        case "hide":
            model.state.hideText = argument != "off"
        case "backup":
            // backup:/path/to/folder watches a backup folder, backup:off stops. (The folder
            // chooser is a system window, which a test run does not put on the screen.)
            model.setBackupFolder(argument == "off" ? nil : argument.map { URL(fileURLWithPath: $0) })
        case "view":
            // view:cursor=notes sets what an agent may read: all, notes or nokeyboard.
            if let argument, let equals = argument.firstIndex(of: "="),
               let agent = model.agents.first(where: { $0.id == argument[..<equals] }) {
                let view: AgentView
                switch argument[argument.index(after: equals)...] {
                case "notes": view = .notesAndMeetings
                case "nokeyboard": view = .noKeyboard
                default: view = .everything
                }
                Task { await model.setAgentView(agent, view) }
            }
        case "clipboard":
            // clipboard:on puts arriving clips on the clipboard, clipboard:off stops.
            model.setClipboardCopy(argument != "off")
        case "appearance":
            NSApp.appearance = NSAppearance(named: argument == "light" ? .aqua : .darkAqua)
        case "windows":
            writeWindowNumbers()
        case "snap":
            // snap:popover=/path/picture.png
            if let (window, path) = target(argument) {
                _ = WindowSnapshot.capture(window, to: URL(fileURLWithPath: path))
            }
        case "snapdrawn":
            // The same, drawn by the app itself: for a run behind the lock screen, where the
            // window server has nothing to give. No shadow, and no rounded corners cut out.
            if let (window, path) = target(argument) {
                _ = WindowSnapshot.capture(window, to: URL(fileURLWithPath: path), drawn: true)
            }
        case "snapflat":
            // The same without the shadow, so a point in the picture is a point in the window.
            if let (window, path) = target(argument) {
                _ = WindowSnapshot.capture(window, to: URL(fileURLWithPath: path), shadow: false)
            }
        case "click":
            // click:popover=120,300 presses the mouse at that point, measured in points from
            // the window's top left corner. The press goes through the app's ordinary event
            // queue, so it does exactly what a person's click there would do.
            if let (window, rest) = target(argument) {
                let numbers = rest.split(separator: ",").compactMap { Double($0) }
                if numbers.count == 2 { click(window, x: numbers[0], y: numbers[1]) }
            }
        case "type":
            // type:floating=lease types into whatever has the keyboard in that window.
            if let (window, text) = target(argument) { type(text, into: window) }
        case "record", "recordflat":
            // record:welcome=/path/folder,6,12 takes 12 pictures a second for 6 seconds.
            if let (window, rest) = target(argument) {
                let fields = rest.split(separator: ",").map(String.init)
                record(
                    window,
                    into: URL(fileURLWithPath: fields[0]),
                    seconds: fields.count > 1 ? Double(fields[1]) ?? 5 : 5,
                    rate: fields.count > 2 ? Double(fields[2]) ?? 12 : 12,
                    shadow: name == "record"
                )
            }
        case "icons":
            // icons:/path/picture.png draws the four faces of the menu bar icon.
            if let argument { drawIconSheet(to: URL(fileURLWithPath: argument)) }
        case "quit":
            NSApp.terminate(nil)
        default:
            break
        }
        writeWindowNumbers()
    }

    /// Splits "popover=/some/path" into the window meant and the rest.
    private func target(_ argument: String?) -> (NSWindow, String)? {
        guard let argument, let equals = argument.firstIndex(of: "=") else { return nil }
        let rest = String(argument[argument.index(after: equals)...])
        let window: NSWindow?
        switch argument[..<equals] {
        case "popover": window = statusItem.window
        case "status": window = statusItem.buttonWindow
        case "welcome": window = welcomeWindow
        case "floating": window = floatingWindow
        case "flash": window = flash?.window
        case "history": window = history?.window
        case "guide": window = guide.window
        default: window = nil
        }
        guard let window, window.isVisible else { return nil }
        return (window, rest)
    }

    private func click(_ window: NSWindow, x: Double, y: Double) {
        let point = NSPoint(x: x, y: Double(window.frame.height) - y)
        for (index, type) in [NSEvent.EventType.leftMouseDown, .leftMouseUp].enumerated() {
            guard let event = NSEvent.mouseEvent(
                with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime + Double(index) * 0.05,
                windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: type == .leftMouseDown ? 1 : 0
            ) else { continue }
            NSApp.postEvent(event, atStart: false)
        }
    }

    private func type(_ text: String, into window: NSWindow) {
        if !window.isKeyWindow { window.makeKey() }
        for character in text {
            for type in [NSEvent.EventType.keyDown, .keyUp] {
                guard let event = NSEvent.keyEvent(
                    with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: window.windowNumber, context: nil, characters: String(character),
                    charactersIgnoringModifiers: String(character), isARepeat: false, keyCode: 0
                ) else { continue }
                NSApp.postEvent(event, atStart: false)
            }
        }
    }

    /// The menu bar icon's four faces, large, on a dark and a light menu bar. The real one
    /// sits in the real menu bar, which a test run does not photograph.
    private func drawIconSheet(to file: URL) {
        let states: [(MenuBarState, String, Double)] = [(.idle, "Running", 0), (.syncing, "Receiving", 0.45), (.stopped, "Not running", 0), (.problem, "Needs you", 0)]
        let scale: CGFloat = 4
        let cell = NSSize(width: 150, height: 86)
        let size = NSSize(width: cell.width * CGFloat(states.count), height: cell.height * 2)
        let image = NSImage(size: size, flipped: true) { _ in
            for (row, dark) in [true, false].enumerated() {
                let bar = NSRect(x: 0, y: CGFloat(row) * cell.height, width: size.width, height: cell.height)
                (dark ? NSColor(white: 0.13, alpha: 1) : NSColor(white: 0.93, alpha: 1)).setFill()
                bar.fill()
                let ink = dark ? NSColor.white : NSColor.black
                for (index, (state, label, phase)) in states.enumerated() {
                    let icon = MenuBarIcon.image(state, phase: phase)
                    let tinted = NSImage(size: icon.size, flipped: false) { rect in
                        icon.draw(in: rect)
                        ink.set()
                        rect.fill(using: .sourceIn)
                        return true
                    }
                    let iconSize = NSSize(width: icon.size.width * 2, height: icon.size.height * 2)
                    let x = CGFloat(index) * cell.width + (cell.width - iconSize.width) / 2
                    tinted.draw(in: NSRect(x: x, y: bar.minY + 14, width: iconSize.width, height: iconSize.height), from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
                    let text = NSAttributedString(string: label, attributes: [.font: NSFont.systemFont(ofSize: 11, weight: .medium), .foregroundColor: ink.withAlphaComponent(0.7)])
                    text.draw(at: NSPoint(x: CGFloat(index) * cell.width + (cell.width - text.size().width) / 2, y: bar.minY + 60))
                }
            }
            return true
        }
        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale), bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        ) else { return }
        rep.size = size
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        image.draw(in: NSRect(origin: .zero, size: size))
        NSGraphicsContext.restoreGraphicsState()
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? rep.representation(using: .png, properties: [:])?.write(to: file)
    }

    private func record(_ window: NSWindow, into folder: URL, seconds: Double, rate: Double, shadow: Bool) {
        var frame = 0
        let total = Int(seconds * rate)
        let timer = Timer(timeInterval: 1 / rate, repeats: true) { timer in
            Task { @MainActor in
                guard frame < total, window.isVisible else {
                    timer.invalidate()
                    try? "done\n".write(to: folder.appendingPathComponent("done"), atomically: true, encoding: .utf8)
                    return
                }
                _ = WindowSnapshot.capture(window, to: folder.appendingPathComponent(String(format: "frame-%04d.png", frame)), shadow: shadow)
                frame += 1
            }
        }
        RunLoop.main.add(timer, forMode: .common)
    }

    /// Writes the window numbers a screenshot tool needs to `windows` in the sandbox folder.
    private func writeWindowNumbers() {
        guard model.env.isSandbox else { return }
        var lines: [String] = []
        if statusItem.isOpen, let number = statusItem.windowNumber { lines.append("popover \(number)") }
        if let welcomeWindow, welcomeWindow.isVisible { lines.append("welcome \(welcomeWindow.windowNumber)") }
        if let floatingWindow, floatingWindow.isVisible { lines.append("floating \(floatingWindow.windowNumber)") }
        if let window = flash?.window, window.isVisible { lines.append("flash \(window.windowNumber)") }
        if let window = history?.window, window.isVisible { lines.append("history \(window.windowNumber)") }
        if let window = guide.window, window.isVisible { lines.append("guide \(window.windowNumber)") }
        let file = model.env.dataDirectory.deletingLastPathComponent().appendingPathComponent("windows")
        try? (lines.joined(separator: "\n") + "\n").write(to: file, atomically: true, encoding: .utf8)
    }
}
