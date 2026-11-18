import AppKit
import SwiftUI
import WebKit
import ZoneKit

/// Where the History window stands, for what is drawn over the page.
@MainActor
@Observable
final class HistoryWindowState {
    enum Load: Equatable {
        case loading
        case shown
        case failed(String)
    }

    var load: Load = .loading
}

/// The history page in a window of Zone's own: zone's page (served by zone on this Mac, at
/// 127.0.0.1) in a web view, so the menu can open it at a meeting or at the notes. The page
/// is the same one the browser shows; "Open in browser" is always there.
@MainActor
final class HistoryWindowController: NSObject, NSWindowDelegate, WKUIDelegate, WKNavigationDelegate {
    let window: ParkableWindow
    let state = HistoryWindowState()
    private let model: AppModel
    private var webView: WKWebView?
    private var port: Int?
    /// The place in the page that was asked for last.
    private(set) var route: PageRoute = .home
    var onClose: () -> Void = {}

    static let size = NSSize(width: 1080, height: 760)

    init(model: AppModel, route: PageRoute) {
        self.model = model
        window = ParkableWindow(
            contentRect: NSRect(origin: .zero, size: Self.size),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        super.init()
        window.title = "Zone History"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 560, height: 420)
        window.backgroundColor = NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(srgbRed: 0x2A / 255, green: 0x2A / 255, blue: 0x2A / 255, alpha: 1)
                : NSColor(srgbRed: 0xF1 / 255, green: 0xE9 / 255, blue: 0xD2 / 255, alpha: 1)
        }
        window.delegate = self

        let configuration = WKWebViewConfiguration()
        // Nothing of the page is kept on disk: its key lives in a cookie for this window only.
        configuration.websiteDataStore = .nonPersistent()
        // The page can tell it is in Zone's window (and leave out what only a browser needs).
        configuration.applicationNameForUserAgent = "ZoneMac/\(model.appVersion)"
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.uiDelegate = self
        webView.navigationDelegate = self
        webView.underPageBackgroundColor = window.backgroundColor
        webView.setValue(false, forKey: "drawsBackground")
        self.webView = webView

        let root = HistoryView(model: model, state: state, webView: webView, openInBrowser: { [weak self] in
            self?.openInBrowser()
        }, retry: { [weak self] in
            self?.load()
        })
        let hosting = NSHostingView(rootView: root)
        hosting.sizingOptions = []
        window.contentView = hosting
        window.setFrame(NSRect(origin: .zero, size: Self.size), display: false)
        window.center()
        if !model.env.isSandbox {
            window.setFrameAutosaveName("ZoneHistory")
        }
        self.route = route
        load()
    }

    // MARK: Loading

    /// Goes to a place in the page. A page that is already showing is not loaded again:
    /// only the part of its address after `#` changes, which is what the page listens for.
    func go(to route: PageRoute) {
        self.route = route
        guard state.load == .shown, let webView else {
            load()
            return
        }
        let fragment = PageRoutes.fragment(route).map { "#" + $0 } ?? ""
        guard let data = try? JSONSerialization.data(withJSONObject: [fragment]), let list = String(data: data, encoding: .utf8) else { return }
        webView.evaluateJavaScript("(function (h) { if (h) { location.hash = h; } else { history.replaceState(null, '', location.pathname + location.search); window.dispatchEvent(new HashChangeEvent('hashchange')); } })(\(list)[0])") { _, _ in }
    }

    func load() {
        state.load = .loading
        let route = route
        Task {
            do {
                let url = try await model.historyLink(route)
                port = url.port
                webView?.load(URLRequest(url: url))
            } catch {
                state.load = .failed(error.localizedDescription)
            }
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        state.load = .shown
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
        if isZonePage(url) || url.scheme == "about" || url.scheme == "blob" {
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

    /// The page never gets the camera or the microphone here (Flash has its own window).
    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin, initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType, decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        decisionHandler(.deny)
    }

    private func isZonePage(_ url: URL) -> Bool {
        url.scheme == "http" && url.host == "127.0.0.1" && (port == nil || url.port == port)
    }

    // MARK: Leaving

    private func openInBrowser() {
        model.openHistoryInBrowser(route)
        window.close()
    }

    func windowWillClose(_ notification: Notification) {
        if let webView {
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

/// The window: a slim bar in Zone's look, then the page.
private struct HistoryView: View {
    let model: AppModel
    let state: HistoryWindowState
    let webView: WKWebView
    let openInBrowser: () -> Void
    let retry: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            bar
            ZStack {
                WebViewHost(webView: webView)
                    .opacity(state.load == .shown ? 1 : 0)
                switch state.load {
                case .loading:
                    VStack(spacing: 10) {
                        ProgressView().controlSize(.small)
                        if !model.isRunning {
                            Text("Starting zone")
                                .font(.system(size: 12.5))
                                .foregroundStyle(Zone.muted(scheme))
                        }
                    }
                case let .failed(reason):
                    VStack(spacing: 12) {
                        Text("History could not open here")
                            .font(.zoneSerif(20))
                            .foregroundStyle(Zone.ink(scheme))
                        Text(reason)
                            .font(.system(size: 12.5))
                            .foregroundStyle(Zone.muted(scheme))
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: 420)
                        HStack(spacing: 10) {
                            Button("Try again", action: retry)
                                .buttonStyle(ZoneQuietButtonStyle(height: 32))
                            Button("Open in browser", action: openInBrowser)
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
            Text("History")
                .font(.zoneSerif(17, weight: .regular))
                .foregroundStyle(Zone.muted(scheme))
            Spacer()
            Button(action: openInBrowser) {
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
    }
}
