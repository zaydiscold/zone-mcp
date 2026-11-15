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
    private var commandTimer: Timer?
    /// A test run can ask the app to stay out of the way of whoever is using the Mac: its
    /// windows never take the keyboard and sit almost entirely off the edge of the screen
    /// (see `parkedOrigin`), where they can still be photographed.
    private var quiet = false
    private var signals: [DispatchSourceSignal] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        let variables = ProcessInfo.processInfo.environment
        let env = ZoneEnvironment.resolve(from: variables, home: FileManager.default.homeDirectoryForCurrentUser)
        quiet = env.isSandbox && variables["ZONE_MAC_QUIET"] != nil

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
        model.closePopover = { [weak self] in self?.statusItem.close() }

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
        panel.minSize = NSSize(width: 300, height: 280)
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
        case "hide":
            model.state.hideText = argument != "off"
        case "appearance":
            NSApp.appearance = NSAppearance(named: argument == "light" ? .aqua : .darkAqua)
        case "windows":
            writeWindowNumbers()
        case "snap":
            // snap:popover=/path/picture.png
            if let (window, path) = target(argument) {
                _ = WindowSnapshot.capture(window, to: URL(fileURLWithPath: path))
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
        case "record":
            // record:welcome=/path/folder,6,12 takes 12 pictures a second for 6 seconds.
            if let (window, rest) = target(argument) {
                let fields = rest.split(separator: ",").map(String.init)
                record(window, into: URL(fileURLWithPath: fields[0]), seconds: fields.count > 1 ? Double(fields[1]) ?? 5 : 5, rate: fields.count > 2 ? Double(fields[2]) ?? 12 : 12)
            }
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
        case "welcome": window = welcomeWindow
        case "floating": window = floatingWindow
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

    private func record(_ window: NSWindow, into folder: URL, seconds: Double, rate: Double) {
        var frame = 0
        let total = Int(seconds * rate)
        let timer = Timer(timeInterval: 1 / rate, repeats: true) { timer in
            Task { @MainActor in
                guard frame < total, window.isVisible else {
                    timer.invalidate()
                    try? "done\n".write(to: folder.appendingPathComponent("done"), atomically: true, encoding: .utf8)
                    return
                }
                _ = WindowSnapshot.capture(window, to: folder.appendingPathComponent(String(format: "frame-%04d.png", frame)))
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
        let file = model.env.dataDirectory.deletingLastPathComponent().appendingPathComponent("windows")
        try? (lines.joined(separator: "\n") + "\n").write(to: file, atomically: true, encoding: .utf8)
    }
}
