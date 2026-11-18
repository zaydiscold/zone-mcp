import Foundation

/// The `zone` command in the terminal: a link, in a folder on the person's PATH, to the
/// app's own copy of the program. Offered, never forced.
public enum CommandLink {
    public enum State: Equatable, Sendable {
        /// `zone` in the terminal is this app's zone.
        case linked(URL)
        /// There is no `zone` command. `suggested` is where the link would go.
        case absent(suggested: URL, needsAdmin: Bool)
        /// There is a `zone` command already and it is a different copy (for example one
        /// installed with cargo). The app leaves it alone unless asked.
        case foreign(URL)
    }

    /// Folders worth putting a command in, best first.
    static func preferred(home: URL) -> [String] {
        [home.appendingPathComponent(".local/bin").path, home.appendingPathComponent("bin").path, "/opt/homebrew/bin", "/usr/local/bin"]
    }

    public static func folders(inPath path: String) -> [String] {
        path.split(separator: ":").map(String.init).filter { $0.hasPrefix("/") }
    }

    /// Looks at every folder on the PATH for a `zone`.
    public static func state(path: String, home: URL, stableBinary: URL) -> State {
        let fm = FileManager.default
        let folders = folders(inPath: path)
        let target = stableBinary.resolvingSymlinksInPath().path
        for folder in folders {
            let candidate = URL(fileURLWithPath: folder).appendingPathComponent("zone")
            // attributesOfItem does not follow a link, so a link whose target is gone counts.
            guard (try? fm.attributesOfItem(atPath: candidate.path)) != nil else { continue }
            if candidate.resolvingSymlinksInPath().path == target {
                return .linked(candidate)
            }
            return .foreign(candidate)
        }
        let onPath = Set(folders)
        for folder in preferred(home: home) where onPath.contains(folder) {
            var isDirectory: ObjCBool = false
            if fm.fileExists(atPath: folder, isDirectory: &isDirectory), isDirectory.boolValue, fm.isWritableFile(atPath: folder) {
                return .absent(suggested: URL(fileURLWithPath: folder).appendingPathComponent("zone"), needsAdmin: false)
            }
        }
        // /usr/local/bin is on every Mac's PATH, but it belongs to the administrator.
        return .absent(suggested: URL(fileURLWithPath: "/usr/local/bin/zone"), needsAdmin: true)
    }

    /// Makes the link, replacing whatever is at `link`.
    public static func install(at link: URL, stableBinary: URL) throws {
        let fm = FileManager.default
        try fm.createDirectory(at: link.deletingLastPathComponent(), withIntermediateDirectories: true)
        if (try? fm.attributesOfItem(atPath: link.path)) != nil {
            try fm.removeItem(at: link)
        }
        try fm.createSymbolicLink(at: link, withDestinationURL: stableBinary)
    }

    /// Removes the link, only if it is a link to this app's zone.
    public static func remove(at link: URL, stableBinary: URL) throws {
        let fm = FileManager.default
        guard let type = try? fm.attributesOfItem(atPath: link.path)[.type] as? FileAttributeType, type == .typeSymbolicLink,
              link.resolvingSymlinksInPath().path == stableBinary.resolvingSymlinksInPath().path
        else { return }
        try fm.removeItem(at: link)
    }

    /// The two shell commands an administrator would run to make the link.
    public static func adminScript(link: URL, stableBinary: URL) -> String {
        let quote = { (text: String) in "'" + text.replacingOccurrences(of: "'", with: "'\\''") + "'" }
        return "/bin/mkdir -p \(quote(link.deletingLastPathComponent().path)) && /bin/ln -sf \(quote(stableBinary.path)) \(quote(link.path))"
    }
}

public enum LoginShell {
    /// The PATH the person's terminal has, which is longer than the one an app gets from
    /// the Finder. Nil when the shell would not say.
    public static func path(shell: String?, home: URL) -> String? {
        let shell = shell.flatMap { $0.isEmpty ? nil : $0 } ?? "/bin/zsh"
        guard let output = try? ZoneProcessRunner.runAndWait(
            binary: URL(fileURLWithPath: shell),
            arguments: ["-l", "-c", "printf '\\n__PATH__%s\\n' \"$PATH\""],
            environment: ["HOME": home.path, "USER": NSUserName(), "LOGNAME": NSUserName(), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "TERM": "dumb"],
            timeout: 6
        ) else { return nil }
        return parse(output.text)
    }

    /// Picks the PATH out of what the shell printed (a login shell may print a greeting too).
    public static func parse(_ text: String) -> String? {
        for line in text.split(whereSeparator: \.isNewline).reversed() where line.hasPrefix("__PATH__") {
            let path = String(line.dropFirst(8))
            return path.contains("/") ? path : nil
        }
        return nil
    }
}

public enum UpdateCheck {
    public enum Answer: Equatable, Sendable {
        /// A later version is out. `page` is where to get it.
        case newer(version: String, page: URL)
        /// This is the latest release, or later than it (a build made before its release
        /// was published). `latest` is the newest published one, when there is one.
        case upToDate(latest: String?)
        /// GitHub could not be asked. The words say why, to follow "Could not check: ".
        case unknown(String)
    }

    public static let releasesPage = URL(string: "https://github.com/zaydiscold/zone-mcp/releases")!
    static let latestRelease = URL(string: "https://api.github.com/repos/zaydiscold/zone-mcp/releases/latest")!
    /// The same question asked of the website: it answers with a redirect to the latest
    /// release's page, and is not counted against the API's hourly limit.
    static let latestPage = URL(string: "https://github.com/zaydiscold/zone-mcp/releases/latest")!

    /// Reads GitHub's answer about the latest release.
    public static func answer(status: Int, body: Data, current: String) -> Answer {
        if status == 404 {
            // Nothing has been released yet, so nothing is newer.
            return .upToDate(latest: nil)
        }
        if status == 403 || status == 429 {
            return .unknown("GitHub is limiting requests from this network for now")
        }
        guard status == 200,
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let tag = object["tag_name"] as? String
        else {
            return .unknown("GitHub did not answer (status \(status))")
        }
        let page = (object["html_url"] as? String).flatMap(URL.init(string:)) ?? releasesPage
        return compare(tag: tag, page: page, current: current)
    }

    /// Reads where the website's "latest release" link led: `.../releases/tag/v0.1.3`.
    public static func answer(redirectedTo url: URL?, current: String) -> Answer {
        guard let url, let tagIndex = url.pathComponents.firstIndex(of: "tag"), tagIndex + 1 < url.pathComponents.count else {
            // No release at all sends the visitor to the plain list of releases.
            if url?.path.hasSuffix("/releases") == true { return .upToDate(latest: nil) }
            return .unknown("GitHub did not say which release is the latest")
        }
        return compare(tag: url.pathComponents[tagIndex + 1], page: url, current: current)
    }

    static func compare(tag: String, page: URL, current: String) -> Answer {
        Versions.isNewer(tag, than: current) ? .newer(version: Versions.plain(tag), page: page) : .upToDate(latest: Versions.plain(tag))
    }

    /// Why a request did not get through, in a few words.
    public static func words(for error: Error) -> String {
        switch (error as? URLError)?.code {
        case .notConnectedToInternet, .networkConnectionLost, .dataNotAllowed, .internationalRoamingOff:
            return "this Mac is offline"
        case .timedOut, .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed:
            return "GitHub could not be reached"
        default:
            return "GitHub could not be reached"
        }
    }

    /// Asks which release is the latest. `feed` stands in for GitHub's API in a test run.
    public static func check(current: String, feed: URL? = nil) async -> Answer {
        var request = URLRequest(url: feed ?? latestRelease, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 10)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        let first: Answer
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            first = answer(status: (response as? HTTPURLResponse)?.statusCode ?? 0, body: data, current: current)
        } catch {
            return .unknown(words(for: error))
        }
        guard case .unknown = first, feed == nil else { return first }
        // The API said no (most often its hourly limit for this network): ask the website.
        var page = URLRequest(url: latestPage, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 10)
        page.httpMethod = "HEAD"
        guard let (_, response) = try? await URLSession.shared.data(for: page), (response as? HTTPURLResponse)?.statusCode == 200 else { return first }
        let second = answer(redirectedTo: response.url, current: current)
        if case .unknown = second { return first }
        return second
    }
}

/// The one line about updates in Settings, and the one thing to click, if any.
public struct UpdateLine: Equatable, Sendable {
    public enum Action: Equatable, Sendable {
        /// Opens the page the newer version is downloaded from.
        case update(URL)
        case checkAgain
    }

    public var text: String
    public var action: Action?
    /// The longer story, for whoever points at the line.
    public var detail: String?

    public var button: String? {
        switch action {
        case .update: return "Update"
        case .checkAgain: return "Try again"
        case nil: return nil
        }
    }

    public init(answer: UpdateCheck.Answer?, checking: Bool, current: String) {
        if checking {
            self.text = "Checking for updates"
            return
        }
        switch answer {
        case let .newer(version, page):
            text = "\(version) is available"
            action = .update(page)
            detail = "You have \(Versions.plain(current)). Update opens the page to download \(version) from."
        case let .upToDate(latest):
            text = "Up to date"
            if let latest, Versions.isNewer(current, than: latest) {
                detail = "This is \(Versions.plain(current)), newer than the latest published release (\(latest))."
            } else {
                detail = "\(Versions.plain(current)) is the latest release."
            }
        case let .unknown(why):
            text = "Could not check for updates: \(why)"
            action = .checkAgain
        case nil:
            text = "Not checked for updates yet"
            action = .checkAgain
        }
    }
}
