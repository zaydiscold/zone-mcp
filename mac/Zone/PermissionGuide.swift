// The permission guide: opens the right page of System Settings and floats a card beside it
// that says what to switch on, with Zone's own row drawn the way the list shows it.
//
// Ported to Swift, and to Zone's look, from Hex's native/permission_guide.m
// (https://github.com/anomalyco/hex). Hex's notice:
//
//   MIT License
//
//   Copyright (c) 2026 Christopher Langton
//
//   Permission is hereby granted, free of charge, to any person obtaining a copy
//   of this software and associated documentation files (the "Software"), to deal
//   in the Software without restriction, including without limitation the rights
//   to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//   copies of the Software, and to permit persons to whom the Software is
//   furnished to do so, subject to the following conditions:
//
//   The above copyright notice and this permission notice shall be included in all
//   copies or substantial portions of the Software.
//
//   THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//   IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//   FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//   AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//   LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//   OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//   SOFTWARE.
//
// Taken from Hex: following the System Settings window (and waiting out of sight while it is
// on another Space), opening the page again when System Settings is closed with the
// permission still off (twice at most), sending the link again after a cold start of System
// Settings (which drops it while it restores its last page), quitting a System Settings that
// sits on another Space so it opens on this one, the tile that lifts out of the window that
// was clicked and flies to the card, and the drag tile for lists that take a dropped app.
// Changed for Zone: the card is drawn in Zone's colors with SwiftUI, System Settings is found
// by its bundle identifier (its window owner name is translated), the card has a close
// button, says why Zone asks, and shows "Done" for a moment when the switch goes on. The
// thinking that can be tested without windows is in ZoneKit (Permissions.swift).

import AppKit
import AVFoundation
import CoreImage
import QuartzCore
import SwiftUI
import ZoneKit

/// What the guide is asked to help with.
struct GuideRequest {
    let permission: Permission
    var folder: ProtectedFolder?
    /// Read fresh on every look (about eight times a second).
    var isGranted: @MainActor () -> Bool
    /// Called every second or so while the card is up, for answers that take a moment to
    /// find out (the local network probe, reading the backup folder).
    var refresh: (@MainActor () -> Void)?
    var onGranted: @MainActor () -> Void = {}
}

/// What the card shows. The card reads it; the guide changes it.
@MainActor
@Observable
final class GuideCardState {
    var permission: Permission = .camera
    var folder: ProtectedFolder?
    var appName = "Zone"
    var icon = NSImage()
    var bundleURL = Bundle.main.bundleURL
    var done = false
    /// While the tile is in flight its place on the card is empty.
    var tileHidden = false
    var onClose: () -> Void = {}
}

@MainActor
final class PermissionGuide {
    let card = GuideCardState()
    /// A test run keeps the card out of the way and never opens System Settings: the link is
    /// handed to `openLink` (which notes it in the sandbox) instead.
    var parked = false
    var openLink: (URL) -> Void = { NSWorkspace.shared.open($0) }

    private var panel: NSPanel?
    private var timer: Timer?
    private var tracker = GuideTracker(startedAt: 0)
    private var request: GuideRequest?
    private var flight: TileFlight?
    private var lastRefresh = 0.0
    /// Bumped by every page request, so an older request's retries stand down.
    private var settingsRequest = 0

    static let settingsBundleID = "com.apple.systempreferences"
    static let width: CGFloat = 404

    var isShowing: Bool { request != nil }
    var showing: Permission? { request?.permission }
    var window: NSWindow? { panel }

    // MARK: Showing and closing

    /// - Parameter flyFrom: where the tile lifts off. By default, the click that asked for the
    ///   guide, when it was in one of Zone's windows (a test run can name a point).
    func show(_ request: GuideRequest, flyFrom: NSPoint? = nil) {
        let click = flyFrom ?? NSEvent.mouseLocation
        let fly = !parked && (flyFrom != nil || clickInZoneWindow(click)) && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        close()
        self.request = request
        tracker = GuideTracker(startedAt: CACurrentMediaTime())

        let bundleURL = Bundle.main.bundleURL.standardizedFileURL
        card.permission = request.permission
        card.folder = request.folder
        card.appName = Self.appName(bundleURL)
        card.icon = NSWorkspace.shared.icon(forFile: bundleURL.path)
        card.bundleURL = bundleURL
        card.done = false
        card.tileHidden = false
        card.onClose = { [weak self] in self?.close() }

        if parked {
            openLink(request.permission.settingsURL)
        } else {
            openSettingsPane(request.permission.settingsURL, mayQuit: true)
        }

        let panel = makePanel()
        self.panel = panel
        if parked {
            panel.setFrameOrigin(StatusItemController.parkedOrigin(for: panel.frame.size))
            panel.orderFrontRegardless()
            return
        }
        if fly {
            card.tileHidden = true
            panel.alphaValue = 0
            flight = TileFlight(origin: click, image: tileImage(), size: NSSize(width: Self.width - 28, height: 46))
        }
        let timer = Timer(timeInterval: 0.12, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.look() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        look()
    }

    func close() {
        endFlight()
        timer?.invalidate()
        timer = nil
        // Every retry that would open System Settings again checks this number, so a closed
        // guide never brings System Settings back by itself.
        settingsRequest += 1
        panel?.orderOut(nil)
        panel = nil
        request = nil
    }

    private func makePanel() -> NSPanel {
        let host = NSHostingView(rootView: GuideCard(state: card))
        // The card is as tall as what it says.
        host.sizingOptions = [.intrinsicContentSize]
        let size = NSSize(width: Self.width, height: host.fittingSize.height)
        let panel = ParkablePanel(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.parked = parked
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        // The drag tile is dragged straight away, without a click to activate Zone first.
        panel.becomesKeyOnlyIfNeeded = true
        host.frame = NSRect(origin: .zero, size: size)
        panel.contentView = host
        return panel
    }

    // MARK: Following System Settings

    private func look() {
        guard let request, let panel else { return }
        let now = CACurrentMediaTime()
        if let refresh = request.refresh, now - lastRefresh >= 1 {
            lastRefresh = now
            refresh()
        }
        fitToCard(panel)
        if card.done { return }
        let action = tracker.step(
            now: now,
            granted: request.isGranted(),
            visibleFrame: Self.settingsFrame(),
            windowExists: Self.settingsWindowExists()
        )
        switch action {
        case .granted:
            finish(request)
        case let .place(frame):
            let size = panel.frame.size
            let screen = NSScreen.screens.first { $0.frame.intersects(frame) } ?? NSScreen.main
            let visible = screen?.visibleFrame ?? frame
            panel.setFrameOrigin(GuidePlacement.origin(settings: frame, card: size, visible: visible))
            if let flight, !flight.started {
                flight.start(towards: { [weak self] in self?.tileTarget() ?? .zero }, onStep: { [weak self] progress in
                    self?.panel?.alphaValue = TileFlight.easeOutCubic(min(max((progress - 0.45) / 0.55, 0), 1))
                }, onEnd: { [weak self] in self?.endFlight() })
            }
            panel.orderFrontRegardless()
        case .wait:
            break
        case .hide:
            panel.orderOut(nil)
        case .reopen:
            // Out of sight until the page is back.
            panel.orderOut(nil)
            openSettingsPane(request.permission.settingsURL, mayQuit: true)
        case .close:
            close()
        }
    }

    /// Keeps the panel the size of the card (it is shorter once it says "Done"), with its top
    /// edge where it was.
    private func fitToCard(_ panel: NSPanel) {
        guard let host = panel.contentView else { return }
        let height = host.fittingSize.height
        guard height > 0, abs(height - panel.frame.height) > 0.5 else { return }
        var frame = panel.frame
        frame.origin.y += frame.height - height
        frame.size.height = height
        panel.setFrame(frame, display: true)
    }

    /// The switch is on: the card says so for a moment, then goes.
    private func finish(_ request: GuideRequest) {
        endFlight()
        withAnimation(Zone.motion) { card.done = true }
        request.onGranted()
        let shown = self.request
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(1.6))
            guard let self, self.request?.permission == shown?.permission, self.card.done else { return }
            self.close()
        }
    }

    // MARK: The flight

    private func endFlight() {
        flight?.end()
        flight = nil
        card.tileHidden = false
        panel?.alphaValue = 1
    }

    /// Where the tile lands: its place on the card, in screen coordinates.
    private func tileTarget() -> NSPoint {
        guard let panel else { return .zero }
        // The tile row sits at the bottom of the card, 14 points in.
        let frame = panel.frame
        return NSPoint(x: frame.minX + 14 + (Self.width - 28) / 2, y: frame.minY + 14 + 23)
    }

    private func tileImage() -> NSImage {
        let size = NSSize(width: Self.width - 28, height: 46)
        let dark = NSApp.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let icon = card.icon
        let name = card.appName
        let toggle = !card.permission.takesDraggedApp
        return NSImage(size: size, flipped: false) { rect in
            Self.drawTile(rect, icon: icon, name: name, dark: dark, toggle: toggle)
            return true
        }
    }

    /// The tile as a picture: Zone's row as the list will show it.
    static func drawTile(_ rect: NSRect, icon: NSImage, name: String, dark: Bool, toggle: Bool) {
        let card = dark ? NSColor(srgbRed: 0x33 / 255, green: 0x33 / 255, blue: 0x33 / 255, alpha: 1) : NSColor(srgbRed: 0xF7 / 255, green: 0xF2 / 255, blue: 0xE4 / 255, alpha: 1)
        let ink = dark ? NSColor(srgbRed: 0xF1 / 255, green: 0xE9 / 255, blue: 0xD2 / 255, alpha: 1) : NSColor(srgbRed: 0x33 / 255, green: 0x33 / 255, blue: 0x33 / 255, alpha: 1)
        let background = NSBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: 10, yRadius: 10)
        card.setFill()
        background.fill()
        ink.withAlphaComponent(dark ? 0.16 : 0.18).setStroke()
        background.lineWidth = 1
        background.stroke()
        icon.draw(in: NSRect(x: 10, y: (rect.height - 30) / 2, width: 30, height: 30))
        let attributes: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 13, weight: .semibold), .foregroundColor: ink]
        let text = NSAttributedString(string: name, attributes: attributes)
        text.draw(at: NSPoint(x: 50, y: (rect.height - text.size().height) / 2))
        if toggle {
            let accent = dark ? NSColor(srgbRed: 0x9B / 255, green: 0x7D / 255, blue: 0xFF / 255, alpha: 1) : NSColor(srgbRed: 0x7A / 255, green: 0x5A / 255, blue: 0xF0 / 255, alpha: 1)
            let track = NSRect(x: rect.width - 12 - 34, y: (rect.height - 20) / 2, width: 34, height: 20)
            accent.setFill()
            NSBezierPath(roundedRect: track, xRadius: 10, yRadius: 10).fill()
            NSColor.white.setFill()
            NSBezierPath(ovalIn: NSRect(x: track.maxX - 18, y: track.minY + 2, width: 16, height: 16)).fill()
        }
    }

    /// The click that asked for the guide, when it was in one of Zone's own windows.
    private func clickInZoneWindow(_ point: NSPoint) -> Bool {
        NSApp.windows.contains { window in
            window.isVisible && window !== panel && window.frame.contains(point)
        }
    }

    // MARK: System Settings

    static func appName(_ bundleURL: URL) -> String {
        var name = FileManager.default.displayName(atPath: bundleURL.path)
        if name.lowercased().hasSuffix(".app") { name = String(name.dropLast(4)) }
        return name.isEmpty ? "Zone" : name
    }

    private static var settingsPID: pid_t? {
        NSRunningApplication.runningApplications(withBundleIdentifier: settingsBundleID).first { !$0.isTerminated }?.processIdentifier
    }

    private static func windows(_ option: CGWindowListOption) -> [[String: Any]] {
        (CGWindowListCopyWindowInfo([option, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]) ?? []
    }

    /// System Settings' window on this Space, in AppKit coordinates. The biggest one, so a
    /// sheet or a popup is not taken for it.
    static func settingsFrame() -> CGRect? {
        guard let pid = settingsPID else { return nil }
        var best: CGRect?
        for window in windows(.optionOnScreenOnly) {
            guard (window[kCGWindowOwnerPID as String] as? Int).map(pid_t.init) == pid,
                  (window[kCGWindowLayer as String] as? Int) == 0,
                  let bounds = window[kCGWindowBounds as String] as? NSDictionary,
                  let frame = CGRect(dictionaryRepresentation: bounds),
                  GuidePlacement.isSettingsSized(frame) else { continue }
            if best.map({ frame.width * frame.height > $0.width * $0.height }) ?? true { best = frame }
        }
        guard let best else { return nil }
        let primary = NSScreen.screens.first?.frame.height ?? best.maxY
        return GuidePlacement.appKitFrame(quartz: best, primaryScreenHeight: primary)
    }

    /// Whether System Settings still has a real window anywhere, on another Space or
    /// minimized included, rather than having been closed.
    static func settingsWindowExists() -> Bool {
        guard let pid = settingsPID else { return false }
        return windows(.optionAll).contains { window in
            guard (window[kCGWindowOwnerPID as String] as? Int).map(pid_t.init) == pid,
                  (window[kCGWindowLayer as String] as? Int) == 0,
                  let bounds = window[kCGWindowBounds as String] as? NSDictionary,
                  let frame = CGRect(dictionaryRepresentation: bounds) else { return false }
            return GuidePlacement.isSettingsSized(frame)
        }
    }

    private static func settingsHasWindowHere() -> Bool {
        guard let pid = settingsPID else { return false }
        return windows(.optionOnScreenOnly).contains { window in
            (window[kCGWindowOwnerPID as String] as? Int).map(pid_t.init) == pid && (window[kCGWindowLayer as String] as? Int) == 0
        }
    }

    /// Opens System Settings at `url`. A System Settings that is not running yet drops the
    /// page in the link while it restores the page it showed last, so after a cold start the
    /// link goes again once its window is up and settled.
    private func openSettingsPane(_ url: URL, mayQuit: Bool) {
        let running = NSRunningApplication.runningApplications(withBundleIdentifier: Self.settingsBundleID)
        let cold = running.isEmpty
        settingsRequest += 1
        let request = settingsRequest
        // A System Settings window left on another Space would pull the whole screen there,
        // away from Zone. Quit it so it opens fresh on this Space.
        if mayQuit, !cold, !Self.settingsHasWindowHere(), Self.settingsWindowExists(), let settings = running.first {
            settings.terminate()
            var waits = 0
            Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] timer in
                Task { @MainActor in
                    waits += 1
                    guard let self, request == self.settingsRequest else { timer.invalidate(); return }
                    if !settings.isTerminated, waits < 40 { return }
                    timer.invalidate()
                    self.openSettingsPane(url, mayQuit: false)
                }
            }
            return
        }
        NSWorkspace.shared.open(url)
        // A System Settings that is starting, or still quitting (closing its window quits
        // it, and for a moment it still counts as running), loses the link: wait for its
        // window, opening the link again if it finishes quitting first.
        guard cold || !Self.settingsWindowExists() else { return }
        var polls = 0
        var lastOpen = 0
        Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] timer in
            Task { @MainActor in
                polls += 1
                guard let self, request == self.settingsRequest, polls <= 100 else { timer.invalidate(); return }
                if Self.settingsPID == nil {
                    // Gone before it showed the page: once a second, ask again.
                    if polls - lastOpen >= 10 {
                        lastOpen = polls
                        NSWorkspace.shared.open(url)
                    }
                    return
                }
                guard Self.settingsHasWindowHere() else { return }
                timer.invalidate()
                for delay in [0.35, 1.2] {
                    DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                        if request == self?.settingsRequest { NSWorkspace.shared.open(url) }
                    }
                }
            }
        }
    }
}

// MARK: Asking macOS directly

enum TCC {
    /// TCC's own answer for a service, read fresh each time: 0 allowed, 1 refused, 2 not
    /// asked yet, -1 when the answer is not available. AVFoundation keeps the answer it read
    /// first until the app itself asks, so a switch turned on in System Settings shows here
    /// first. (As Hex does for the microphone.)
    static func preflight(_ service: String) -> Int {
        typealias Preflight = @convention(c) (CFString, CFDictionary?) -> Int32
        struct Loaded { static let function: Preflight? = {
            guard let handle = dlopen("/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC", RTLD_NOW),
                  let symbol = dlsym(handle, "TCCAccessPreflight") else { return nil }
            return unsafeBitCast(symbol, to: Preflight.self)
        }() }
        guard let preflight = Loaded.function else { return -1 }
        return Int(preflight(service as CFString, nil))
    }

    static var cameraAllowed: Bool {
        AVCaptureDevice.authorizationStatus(for: .video) == .authorized || preflight("kTCCServiceCamera") == 0
    }
}

// MARK: The card

private struct GuideCard: View {
    let state: GuideCardState
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if state.done {
                HStack(spacing: 10) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 18))
                        .foregroundStyle(Zone.accentGraphic(scheme))
                    Text(doneText)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Zone.ink(scheme))
                    Spacer(minLength: 0)
                }
                .frame(minHeight: 46)
                .transition(.opacity)
            } else {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(Zone.accentText(scheme))
                        .padding(.top, 1)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(state.permission.instruction(appName: state.appName, folder: state.folder))
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Zone.ink(scheme))
                            .fixedSize(horizontal: false, vertical: true)
                        Text(state.permission.reason)
                            .font(.system(size: 11.5))
                            .foregroundStyle(Zone.muted(scheme))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 4)
                    Button {
                        state.onClose()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 9.5, weight: .bold))
                            .foregroundStyle(Zone.muted(scheme))
                            .frame(width: 20, height: 20)
                            .background(Zone.ink(scheme).opacity(0.07), in: Circle())
                            .contentShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .help("Close this guide")
                    .accessibilityLabel("Close")
                }
                tile
                    .opacity(state.tileHidden ? 0 : 1)
            }
        }
        .padding(14)
        .frame(width: PermissionGuide.width, alignment: .leading)
        .background(Zone.paper(scheme), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
        .animation(Zone.motion, value: state.done)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private var tile: some View {
        if state.permission.takesDraggedApp {
            DragTile(bundleURL: state.bundleURL, icon: state.icon, name: state.appName, dark: scheme == .dark)
                .frame(height: 46)
                .help("Drag \(state.appName) into the list")
        } else {
            HStack(spacing: 10) {
                Image(nsImage: state.icon)
                    .resizable()
                    .frame(width: 30, height: 30)
                Text(state.appName)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Zone.ink(scheme))
                Spacer()
                Text("Switch this on")
                    .font(.system(size: 11))
                    .foregroundStyle(Zone.muted(scheme))
                Toggle("", isOn: .constant(true))
                    .toggleStyle(ZoneToggleStyle())
                    .labelsHidden()
                    .fixedSize()
                    .allowsHitTesting(false)
            }
            .padding(.leading, 8)
            .padding(.trailing, 12)
            .frame(height: 46)
            .background(Zone.card(scheme), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(state.appName), switched on")
        }
    }

    private var doneText: String {
        switch state.permission {
        case .camera: return "Done. \(state.appName) can use the camera."
        case .localNetwork: return "Done. Your iPhone can reach this Mac directly."
        case .filesAndFolders: return "Done. \(state.appName) can read the backup folder."
        case .loginItems: return "Done. \(state.appName) starts when you log in."
        }
    }
}

/// The tile for a list that takes a dragged app: Zone's icon and name, dragged as the app
/// itself.
private struct DragTile: NSViewRepresentable {
    let bundleURL: URL
    let icon: NSImage
    let name: String
    let dark: Bool

    func makeNSView(context: Context) -> DragTileView {
        DragTileView(bundleURL: bundleURL, icon: icon, name: name)
    }

    func updateNSView(_ view: DragTileView, context: Context) {
        view.dark = dark
        view.needsDisplay = true
    }
}

final class DragTileView: NSView, NSDraggingSource {
    let bundleURL: URL
    let icon: NSImage
    let name: String
    var dark = true
    private var mouseDownEvent: NSEvent?

    init(bundleURL: URL, icon: NSImage, name: String) {
        self.bundleURL = bundleURL
        self.icon = icon
        self.name = name
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { nil }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func resetCursorRects() {
        addCursorRect(bounds, cursor: .openHand)
    }

    override func draw(_ dirtyRect: NSRect) {
        PermissionGuide.drawTile(bounds, icon: icon, name: name, dark: dark, toggle: false)
    }

    override func mouseDown(with event: NSEvent) {
        mouseDownEvent = event
    }

    override func mouseDragged(with event: NSEvent) {
        guard let start = mouseDownEvent else { return }
        let dx = event.locationInWindow.x - start.locationInWindow.x
        let dy = event.locationInWindow.y - start.locationInWindow.y
        guard hypot(dx, dy) >= 3 else { return }
        let item = NSDraggingItem(pasteboardWriter: bundleURL as NSURL)
        let snapshot = NSImage(size: bounds.size, flipped: false) { [self] rect in
            PermissionGuide.drawTile(rect, icon: icon, name: name, dark: dark, toggle: false)
            return true
        }
        item.setDraggingFrame(bounds, contents: snapshot)
        let session = beginDraggingSession(with: [item], event: start, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = true
        session.draggingFormation = .none
        mouseDownEvent = nil
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        .copy
    }
}

// MARK: The tile's flight

/// A copy of the tile lifts out of the window that was clicked, waits for System Settings to
/// open, then flies along a curve to its place on the card, so the eye can follow it there.
@MainActor
final class TileFlight {
    private let panel: NSPanel
    private let layer = CALayer()
    private let origin: NSPoint
    private let shownAt = CACurrentMediaTime()
    private var startedAt: CFTimeInterval = 0
    private var timer: Timer?
    private var target: () -> NSPoint = { .zero }
    private var onStep: (Double) -> Void = { _ in }
    private var onEnd: () -> Void = {}

    static let padding: CGFloat = 40
    static let lift: CFTimeInterval = 0.18
    static let duration: CFTimeInterval = 0.62
    /// A System Settings that has not opened by then gets no flight.
    static let patience: CFTimeInterval = 4

    var started: Bool { startedAt != 0 }

    init(origin: NSPoint, image: NSImage, size: NSSize) {
        self.origin = origin
        let outer = NSSize(width: size.width + 2 * Self.padding, height: size.height + 2 * Self.padding)
        panel = NSPanel(contentRect: NSRect(origin: .zero, size: outer), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = NSWindow.Level(rawValue: NSWindow.Level.floating.rawValue + 1)
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false

        let host = NSView(frame: NSRect(origin: .zero, size: outer))
        host.layer = CALayer()
        host.wantsLayer = true
        host.layerUsesCoreImageFilters = true
        layer.bounds = CGRect(origin: .zero, size: size)
        layer.position = CGPoint(x: outer.width / 2, y: outer.height / 2)
        layer.contents = image
        layer.contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        // Zone's shadows are hard edged.
        layer.shadowOpacity = 0.35
        layer.shadowRadius = 0
        layer.shadowOffset = CGSize(width: 2, height: -3)
        if let blur = CIFilter(name: "CIGaussianBlur") {
            blur.name = "blur"
            blur.setValue(0, forKey: kCIInputRadiusKey)
            layer.filters = [blur]
        }
        host.layer?.addSublayer(layer)
        panel.contentView = host
        place(origin, scale: 0.7, blur: 4, opacity: 0)
        panel.orderFrontRegardless()
        let timer = Timer(timeInterval: 1.0 / 120, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.step() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    func start(towards target: @escaping () -> NSPoint, onStep: @escaping (Double) -> Void, onEnd: @escaping () -> Void) {
        self.target = target
        self.onStep = onStep
        self.onEnd = onEnd
        startedAt = CACurrentMediaTime()
    }

    func end() {
        timer?.invalidate()
        timer = nil
        panel.orderOut(nil)
    }

    static func easeOutCubic(_ t: Double) -> Double { 1 - pow(1 - t, 3) }
    static func easeInOutCubic(_ t: Double) -> Double { t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2 }

    private func place(_ center: NSPoint, scale: CGFloat, blur: CGFloat, opacity: Float) {
        let size = panel.frame.size
        panel.setFrameOrigin(NSPoint(x: center.x - size.width / 2, y: center.y - size.height / 2))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer.transform = CATransform3DMakeScale(scale, scale, 1)
        layer.opacity = opacity
        layer.setValue(blur, forKeyPath: "filters.blur.inputRadius")
        CATransaction.commit()
    }

    private func step() {
        let now = CACurrentMediaTime()
        guard started else {
            // Lifted out of the window, waiting for System Settings to open.
            if now - shownAt > Self.patience {
                end()
                onEnd()
                return
            }
            let t = Self.easeOutCubic(min(max((now - shownAt) / Self.lift, 0), 1))
            place(NSPoint(x: origin.x, y: origin.y + 12 * t), scale: 0.7 + 0.1 * t, blur: 4 * (1 - t), opacity: Float(t))
            return
        }
        let t = min(max((now - startedAt) / Self.duration, 0), 1)
        let e = Self.easeInOutCubic(t)
        let start = NSPoint(x: origin.x, y: origin.y + 12)
        let end = target()
        let distance = hypot(end.x - start.x, end.y - start.y)
        let control = NSPoint(x: (start.x + end.x) / 2, y: max(start.y, end.y) + min(160, distance * 0.25))
        // A quadratic Bezier curve from where the tile lifted off to its place on the card.
        let u = CGFloat(1 - e), v = CGFloat(e)
        let a = u * u, b = 2 * u * v, c = v * v
        let x: CGFloat = a * start.x + b * control.x + c * end.x
        let y: CGFloat = a * start.y + b * control.y + c * end.y
        let point = NSPoint(x: x, y: y)
        place(point, scale: 0.8 + 0.2 * e, blur: 6 * sin(.pi * e), opacity: 1)
        onStep(t)
        if t >= 1 {
            self.end()
            onEnd()
        }
    }
}
