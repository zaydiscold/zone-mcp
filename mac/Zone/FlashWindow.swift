import AppKit
import AVFoundation
import SwiftUI
import WebKit
import ZoneKit

/// Where the Flash window stands, for the bar above the page.
@MainActor
@Observable
final class FlashWindowState {
    enum Load: Equatable {
        case loading
        case shown
        case failed(String)
    }

    var load: Load = .loading
    /// macOS said no to the camera for Zone.
    var cameraRefused = false
    /// The page asked for the camera and it is running.
    var cameraOn = false
}

/// Flash in a window of Zone's own: zone's Flash page (served by zone on this Mac, at
/// 127.0.0.1) in a web view, with the camera allowed by Zone instead of a browser. The page is
/// the same one the browser shows; "Open in browser" is always there.
@MainActor
final class FlashWindowController: NSObject, NSWindowDelegate, WKUIDelegate, WKNavigationDelegate {
    let window: ParkableWindow
    let state = FlashWindowState()
    private let model: AppModel
    private var webView: WKWebView?
    private var port: Int?
    /// An older zone without `open --flash`: the history page is opened first (it hands over
    /// the key), then Flash.
    private var thenFlash: URL?
    /// The camera was paused because the window was minimized or Zone was hidden, and comes
    /// back on when the window can be seen again.
    private var pausedWhileAway = false
    private var appObservers: [NSObjectProtocol] = []
    var onClose: () -> Void = {}

    static let size = NSSize(width: 1040, height: 760)

    init(model: AppModel, quiet: Bool) {
        self.model = model
        window = ParkableWindow(
            contentRect: NSRect(origin: .zero, size: Self.size),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        super.init()
        window.title = "Zone Flash"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 720, height: 520)
        // The page follows the Mac's light or dark setting in Zone's paper colors, and so does
        // the window around it (no flash of another color while it loads).
        window.backgroundColor = NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(srgbRed: 0x2A / 255, green: 0x2A / 255, blue: 0x2A / 255, alpha: 1)
                : NSColor(srgbRed: 0xF1 / 255, green: 0xE9 / 255, blue: 0xD2 / 255, alpha: 1)
        }
        window.delegate = self

        let webView = makeWebView(mockCamera: model.env.isSandbox && ProcessInfo.processInfo.environment["ZONE_MAC_MOCK_CAMERA"] != nil)
        self.webView = webView
        let root = FlashView(model: model, state: state, webView: webView, actions: FlashActions(
            openInBrowser: { [weak self] in self?.openInBrowser() },
            retry: { [weak self] in self?.load() },
            allowCamera: { [weak self] in self?.allowCamera() },
            pair: { [weak self] in
                self?.model.showPopoverPage(.pair)
            }
        ))
        let hosting = NSHostingView(rootView: root)
        hosting.sizingOptions = []
        window.contentView = hosting
        window.setFrame(NSRect(origin: .zero, size: Self.size), display: false)
        window.center()
        if !model.env.isSandbox {
            window.setFrameAutosaveName("ZoneFlash")
        }
        let center = NotificationCenter.default
        appObservers = [
            center.addObserver(forName: NSApplication.didHideNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.pauseCameraWhileAway() }
            },
            center.addObserver(forName: NSApplication.didUnhideNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.resumeCameraIfSeen() }
            },
        ]
        load()
    }

    // MARK: The camera while the window is out of sight

    /// Minimized or hidden, nobody is holding a phone up to a window they cannot see: the
    /// camera pauses (its light goes off) instead of running unseen.
    func windowDidMiniaturize(_ notification: Notification) {
        pauseCameraWhileAway()
    }

    func windowDidDeminiaturize(_ notification: Notification) {
        resumeCameraIfSeen()
    }

    private func pauseCameraWhileAway() {
        guard let webView, webView.cameraCaptureState == .active else { return }
        pausedWhileAway = true
        webView.setCameraCaptureState(.muted, completionHandler: nil)
    }

    private func resumeCameraIfSeen() {
        guard pausedWhileAway, let webView, !window.isMiniaturized, !NSApp.isHidden else { return }
        pausedWhileAway = false
        webView.setCameraCaptureState(.active, completionHandler: nil)
    }

    private func makeWebView(mockCamera: Bool) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // Nothing of the page is kept on disk: its key lives in a cookie for this window only.
        configuration.websiteDataStore = .nonPersistent()
        configuration.mediaTypesRequiringUserActionForPlayback = []
        // The page can tell it is in Zone's window (and leave out what only a browser needs).
        configuration.applicationNameForUserAgent = "ZoneMac/\(model.appVersion)"
        if mockCamera {
            // A test run: WebKit's stand-in camera, so the whole path (asking, Zone allowing,
            // pictures reaching zone) runs without a real camera and without macOS asking.
            Self.setPreference(configuration.preferences, "_setMockCaptureDevicesEnabled:", true)
            // Asking stays on, so Zone's own answer below (allow the camera, for this page
            // only) is what lets the stand-in camera run.
            Self.setPreference(configuration.preferences, "_setMockCaptureDevicesPromptEnabled:", true)
        }
        let webView = WKWebView(frame: .zero, configuration: configuration)
        if mockCamera {
            // A test run's window sits off the edge of the screen (or behind the lock screen),
            // and WebKit holds the camera back from a page it thinks nobody can see.
            Self.setPreference(webView, "_setWindowOcclusionDetectionEnabled:", false)
        }
        webView.uiDelegate = self
        webView.navigationDelegate = self
        webView.underPageBackgroundColor = window.backgroundColor
        webView.setValue(false, forKey: "drawsBackground")
        webView.allowsBackForwardNavigationGestures = false
        webView.addObserver(self, forKeyPath: "cameraCaptureState", options: [.new], context: nil)
        return webView
    }

    /// Sets one of WebKit's switches that has no public name, when this WebKit has it.
    private static func setPreference(_ object: NSObject, _ name: String, _ value: Bool) {
        let selector = NSSelectorFromString(name)
        guard object.responds(to: selector) else {
            FileHandle.standardError.write(Data("flash: this WebKit has no \(name)\n".utf8))
            return
        }
        typealias Setter = @convention(c) (AnyObject, Selector, Bool) -> Void
        unsafeBitCast(object.method(for: selector), to: Setter.self)(object, selector, value)
    }

    override nonisolated func observeValue(forKeyPath keyPath: String?, of object: Any?, change: [NSKeyValueChangeKey: Any]?, context: UnsafeMutableRawPointer?) {
        Task { @MainActor in
            guard let webView = self.webView else { return }
            self.state.cameraOn = webView.cameraCaptureState == .active
        }
    }

    // MARK: Loading

    func load() {
        state.load = .loading
        Task {
            do {
                let (url, port, first) = try await model.flashLink()
                self.port = port
                if let first {
                    thenFlash = url
                    webView?.load(URLRequest(url: first))
                } else {
                    thenFlash = nil
                    webView?.load(URLRequest(url: url))
                }
            } catch {
                state.load = .failed(error.localizedDescription)
            }
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if let next = thenFlash, webView.url?.path != "/flash" {
            thenFlash = nil
            webView.load(URLRequest(url: next))
            return
        }
        state.load = .shown
        webView.evaluateJavaScript("document.title") { _, _ in }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        state.load = .failed(error.localizedDescription)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        state.load = .failed(error.localizedDescription)
    }

    /// The page stays on zone's own server. Any other link opens in the browser.
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = action.request.url else { decisionHandler(.cancel); return }
        if isZonePage(url) || url.scheme == "about" {
            decisionHandler(.allow)
        } else {
            decisionHandler(.cancel)
            model.open(url)
        }
    }

    /// A link that asks for a new window opens in the browser.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = action.request.url { model.open(url) }
        return nil
    }

    private func isZonePage(_ url: URL) -> Bool {
        url.scheme == "http" && url.host == "127.0.0.1" && (port == nil || url.port == port)
    }

    // MARK: The camera

    /// The page asks for the camera: Zone allows it for zone's own Flash page, and only the
    /// camera. macOS's own question about the camera is asked for Zone, once.
    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin, initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType, decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        if model.env.isSandbox {
            FileHandle.standardError.write(Data("flash: the page asks for \(type == .camera ? "the camera" : "capture \(type.rawValue)") from \(origin.protocol)://\(origin.host):\(origin.port), camera \(AVCaptureDevice.authorizationStatus(for: .video).rawValue)\n".utf8))
        }
        guard origin.protocol == "http", origin.host == "127.0.0.1", port == nil || origin.port == port, type == .camera else {
            decisionHandler(.deny)
            return
        }
        if model.env.isSandbox, ProcessInfo.processInfo.environment["ZONE_MAC_MOCK_CAMERA"] != nil {
            decisionHandler(.grant)
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            state.cameraRefused = false
            decisionHandler(.grant)
        case .notDetermined:
            if model.env.isSandbox, ProcessInfo.processInfo.environment["ZONE_MAC_REAL_CAMERA"] == nil {
                // A test run never puts macOS's camera question in front of the person.
                decisionHandler(.deny)
                return
            }
            AVCaptureDevice.requestAccess(for: .video) { granted in
                Task { @MainActor in
                    self.state.cameraRefused = !granted
                    decisionHandler(granted ? .grant : .deny)
                }
            }
        default:
            if TCC.cameraAllowed {
                // Switched on in System Settings since AVFoundation last looked.
                state.cameraRefused = false
                decisionHandler(.grant)
            } else {
                state.cameraRefused = true
                decisionHandler(.deny)
            }
        }
    }

    private func allowCamera() {
        model.showGuide(GuideRequest(
            permission: .camera,
            isGranted: { TCC.cameraAllowed },
            onGranted: { [weak self] in
                self?.state.cameraRefused = false
                self?.webView?.reload()
            }
        ))
    }

    // MARK: Leaving

    private func openInBrowser() {
        model.openFlashInBrowser()
        window.close()
    }

    func windowWillClose(_ notification: Notification) {
        appObservers.forEach(NotificationCenter.default.removeObserver)
        appObservers = []
        pausedWhileAway = false
        // The camera goes off with the window, not when the page gets around to it.
        if let webView {
            webView.setCameraCaptureState(.none, completionHandler: nil)
            webView.removeObserver(self, forKeyPath: "cameraCaptureState")
            webView.stopLoading()
            webView.loadHTMLString("", baseURL: nil)
        }
        webView = nil
        onClose()
    }

    /// Runs a script in the page (for test runs) and returns what it gave back.
    func evaluate(_ script: String) async -> String {
        guard let webView else { return "no page" }
        do {
            let value = try await webView.callAsyncJavaScript(script, contentWorld: .page)
            if let value, JSONSerialization.isValidJSONObject(value), let data = try? JSONSerialization.data(withJSONObject: value) {
                return String(decoding: data, as: UTF8.self)
            }
            return value.map { "\($0)" } ?? "null"
        } catch {
            return "error: \(error.localizedDescription)"
        }
    }
}

struct FlashActions {
    var openInBrowser: () -> Void
    var retry: () -> Void
    var allowCamera: () -> Void
    var pair: () -> Void
}

/// The window: a slim bar in Zone's look, then the page.
private struct FlashView: View {
    let model: AppModel
    let state: FlashWindowState
    let webView: WKWebView
    let actions: FlashActions
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            bar
            if state.cameraRefused {
                notice(
                    symbol: "camera",
                    text: "macOS has not let Zone use the camera yet. Flash needs it to read your iPhone's screen.",
                    button: "Allow the camera",
                    action: actions.allowCamera
                )
            } else if model.status?.phones.isEmpty == true {
                notice(
                    symbol: "iphone",
                    text: "No iPhone is paired with this Mac yet. A flash from an unpaired phone can pair it if a pair code is waiting.",
                    button: "Show a pair code",
                    action: actions.pair
                )
            }
            ZStack {
                WebViewHost(webView: webView)
                    .opacity(state.load == .shown ? 1 : 0)
                switch state.load {
                case .loading:
                    ProgressView().controlSize(.small)
                case let .failed(reason):
                    VStack(spacing: 12) {
                        Text("Flash could not open here")
                            .font(.zoneSerif(20))
                            .foregroundStyle(Zone.ink(scheme))
                        Text(reason)
                            .font(.system(size: 12.5))
                            .foregroundStyle(Zone.muted(scheme))
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: 420)
                        HStack(spacing: 10) {
                            Button("Try again", action: actions.retry)
                                .buttonStyle(ZoneQuietButtonStyle(height: 32))
                            Button("Open in browser", action: actions.openInBrowser)
                                .buttonStyle(ZonePrimaryButtonStyle(height: 32))
                        }
                    }
                case .shown:
                    EmptyView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Zone.paper(scheme))
        .tint(Zone.accentText(scheme))
        .ignoresSafeArea()
    }

    private var bar: some View {
        HStack(spacing: 10) {
            // Room for the window's own buttons.
            Color.clear.frame(width: 64, height: 1)
            Wordmark(size: 17)
            Text("Flash")
                .font(.zoneSerif(17, weight: .regular))
                .foregroundStyle(Zone.muted(scheme))
            if state.cameraOn {
                HStack(spacing: 5) {
                    StatusDot(color: Zone.accentGraphic(scheme), pulses: true, size: 6)
                    Text("Camera on")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Zone.muted(scheme))
                }
                .transition(.opacity)
            }
            Spacer()
            Button(action: actions.openInBrowser) {
                HStack(spacing: 4) {
                    Text("Open in browser")
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 9.5, weight: .semibold))
                }
            }
            .buttonStyle(ZoneLinkButtonStyle(size: 12))
            .help("Show this page in your web browser instead, and close this window")
        }
        .padding(.horizontal, 14)
        .frame(height: 40)
        .background(Zone.paper(scheme))
        .overlay(alignment: .bottom) { Hairline() }
        .animation(Zone.motion, value: state.cameraOn)
    }

    private func notice(symbol: String, text: String, button: String, action: @escaping () -> Void) -> some View {
        HStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Zone.accentText(scheme))
            Text(text)
                .font(.system(size: 12))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 8)
            Button(button, action: action)
                .buttonStyle(ZonePillButtonStyle(filled: true))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Zone.accentGraphic(scheme).opacity(0.14))
        .overlay(alignment: .bottom) { Hairline() }
    }
}

private struct WebViewHost: NSViewRepresentable {
    let webView: WKWebView

    func makeNSView(context: Context) -> WKWebView { webView }
    func updateNSView(_ view: WKWebView, context: Context) {}
}
