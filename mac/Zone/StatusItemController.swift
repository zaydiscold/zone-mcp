import AppKit
import SwiftUI
import ZoneKit

/// A borderless window that can still take the keyboard, so Escape closes it.
final class PopoverPanel: NSPanel {
    var onCancel: () -> Void = {}

    override var canBecomeKey: Bool { true }

    override func cancelOperation(_ sender: Any?) {
        onCancel()
    }
}

/// The Z in the menu bar and the window that opens under it.
@MainActor
final class StatusItemController: NSObject {
    let navigation = PopoverNavigation()
    private let model: AppModel
    private let item: NSStatusItem
    private var panel: PopoverPanel?
    private var monitors: [Any] = []
    private var animation: Timer?
    private var phase = 0.0
    private var shownState: MenuBarState?
    private var height: CGFloat = 480
    /// The app that was in front when the window opened: Command-Return pastes into it.
    private(set) var appBehind: NSRunningApplication?
    /// A test run keeps the window open while other apps are clicked, and keeps it out of
    /// the way: see `parkedOrigin`.
    var parked = false

    /// Where a test run puts its windows: all but a sliver beyond the right edge of the
    /// screens. Nobody working on the Mac is interrupted, and because a sliver is still on
    /// screen, macOS keeps drawing the window, so it can be photographed as it really looks.
    static func parkedOrigin(for size: NSSize) -> NSPoint {
        let screens = NSScreen.screens
        let right = screens.map(\.frame.maxX).max() ?? 0
        let bottom = (NSScreen.main ?? screens.first)?.visibleFrame.minY ?? 0
        return NSPoint(x: right - 3, y: bottom + 40)
    }

    var isOpen: Bool { panel?.isVisible ?? false }
    var windowNumber: Int? { panel?.windowNumber }
    var window: NSWindow? { panel }
    /// The window macOS keeps for the icon in the menu bar.
    var buttonWindow: NSWindow? { item.button?.window }

    init(model: AppModel) {
        self.model = model
        self.item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        if let button = item.button {
            button.target = self
            button.action = #selector(toggle)
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            button.toolTip = "Zone"
        }
        updateIcon()
        observeIcon()
    }

    // MARK: The icon

    /// Redraws the icon whenever what it should say changes.
    private func observeIcon() {
        withObservationTracking {
            _ = model.menuBarState
        } onChange: { [weak self] in
            Task { @MainActor in
                self?.updateIcon()
                self?.observeIcon()
            }
        }
    }

    private func updateIcon() {
        let state = model.menuBarState
        guard state != shownState else { return }
        shownState = state
        animation?.invalidate()
        animation = nil
        if state == .syncing, !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
            phase = 0
            let timer = Timer(timeInterval: 1.0 / 20, repeats: true) { [weak self] _ in
                Task { @MainActor in
                    guard let self else { return }
                    self.phase = (self.phase + 1.0 / 26).truncatingRemainder(dividingBy: 1)
                    self.item.button?.image = MenuBarIcon.image(.syncing, phase: self.phase)
                }
            }
            RunLoop.main.add(timer, forMode: .common)
            animation = timer
        }
        item.button?.image = MenuBarIcon.image(state)
        item.button?.setAccessibilityLabel(MenuBarIcon.description(state))
    }

    // MARK: The window

    @objc private func toggle() {
        if isOpen {
            close()
        } else {
            show()
        }
    }

    func show(page: PopoverNavigation.Page? = nil, takeKeyboard: Bool = true) {
        if let page { navigation.page = page }
        guard !isOpen else { return }
        let front = NSWorkspace.shared.frontmostApplication
        appBehind = front?.processIdentifier == ProcessInfo.processInfo.processIdentifier ? nil : front
        let panel = self.panel ?? makePanel()
        self.panel = panel
        model.watchers += 1
        Task { await model.refresh() }
        place(panel)
        if takeKeyboard {
            panel.makeKeyAndOrderFront(nil)
        } else {
            panel.orderFrontRegardless()
        }
        item.button?.highlight(!parked)
        navigation.cursor = ListCursor()
        navigation.focusSearch += 1
        // The keys of the search: arrows move, Return copies, Command-Return pastes, Escape
        // closes. Only on the main page, and only while this window has the keyboard.
        if let monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown, handler: { [weak self] event in
            guard let self, event.window === self.panel else { return event }
            let code = event.keyCode, command = event.modifierFlags.contains(.command)
            let used = MainActor.assumeIsolated { self.handle(keyCode: code, command: command) }
            return used ? nil : event
        }) {
            monitors.append(monitor)
        }
        guard !parked else { return }
        // A click anywhere else closes it, the way a menu closes.
        if let monitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown], handler: { [weak self] _ in
            Task { @MainActor in self?.close() }
        }) {
            monitors.append(monitor)
        }
        if let monitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown], handler: { [weak self] event in
            guard let self, let panel = self.panel else { return event }
            if event.window !== panel, event.window !== self.item.button?.window {
                self.close()
            }
            return event
        }) {
            monitors.append(monitor)
        }
    }

    /// Opens the window from the key, with the cursor in the search field; pressed again
    /// while it is open, closes it.
    func toggleFromKey() {
        if isOpen {
            close()
        } else {
            // A test run never takes the keyboard from the person using the Mac.
            show(page: .main, takeKeyboard: !parked)
        }
    }

    /// A key pressed in the window. True when it was used here.
    func handle(keyCode: UInt16, command: Bool) -> Bool {
        guard navigation.page == .main else { return false }
        let rows = navigation.keyRows
        switch Int(keyCode) {
        case 125: // down
            navigation.cursor.move(1, count: rows.count)
        case 126: // up
            navigation.cursor.move(-1, count: rows.count)
        case 36, 76: // Return, Enter
            guard let index = navigation.cursor.index, rows.indices.contains(index) else { return model.search.isActive }
            let item = rows[index]
            if command {
                if model.paste(item) { close() }
            } else {
                model.copy(item)
                close()
            }
        case 53: // Escape
            close()
        default:
            return false
        }
        return true
    }

    func close() {
        guard isOpen else { return }
        panel?.orderOut(nil)
        item.button?.highlight(false)
        monitors.forEach(NSEvent.removeMonitor)
        monitors.removeAll()
        model.watchers = max(0, model.watchers - 1)
        if navigation.page == .pair {
            model.stopPairing()
        }
        navigation.page = .main
        model.search.clear()
        navigation.cursor = ListCursor()
    }

    private func makePanel() -> PopoverPanel {
        let panel = PopoverPanel(
            contentRect: NSRect(x: 0, y: 0, width: PopoverView.width, height: height),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .popUpMenu
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        panel.isMovable = false
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.animationBehavior = .utilityWindow
        panel.onCancel = { [weak self] in self?.close() }
        let root = PopoverView(model: model, navigation: navigation) { [weak self] height in
            self?.setHeight(height)
        }
        .frame(maxHeight: .infinity, alignment: .top)
        let hosting = NSHostingView(rootView: root)
        hosting.sizingOptions = []
        panel.contentView = hosting
        return panel
    }

    private func setHeight(_ new: CGFloat) {
        let rounded = new.rounded(.up)
        guard abs(rounded - height) > 0.5 else { return }
        height = rounded
        if let panel {
            place(panel)
            panel.invalidateShadow()
        }
    }

    /// Hangs the window under the icon, kept on the screen.
    private func place(_ panel: NSPanel) {
        let width = PopoverView.width
        if let screen = item.button?.window?.screen ?? NSScreen.main {
            // The page keeps to this: its list gives way before the buttons under it would.
            let room = screen.visibleFrame.height - 16
            if abs(navigation.maxHeight - room) > 0.5 { navigation.maxHeight = room }
        }
        if parked {
            let size = NSSize(width: width, height: height)
            panel.setFrame(NSRect(origin: Self.parkedOrigin(for: size), size: size), display: true)
            return
        }
        guard let buttonWindow = item.button?.window, let screen = buttonWindow.screen ?? NSScreen.main else {
            panel.setContentSize(NSSize(width: width, height: height))
            panel.center()
            return
        }
        let anchor = buttonWindow.frame
        let visible = screen.visibleFrame
        var x = anchor.midX - width / 2
        x = min(max(x, visible.minX + 8), visible.maxX - width - 8)
        let top = min(anchor.minY, visible.maxY) - 6
        let fitted = min(height, visible.height - 16)
        panel.setFrame(NSRect(x: x, y: top - fitted, width: width, height: fitted), display: true)
    }
}
