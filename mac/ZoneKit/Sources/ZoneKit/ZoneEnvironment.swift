import Foundation

/// Where zone's files are and what the `zone` program is told when the app runs it.
///
/// Normally: the person's home folder and zone's usual data folder. For testing, the
/// environment variable `ZONE_MAC_SANDBOX=/some/folder` moves all of it inside that folder:
/// a pretend home (so AI client settings written there are not the real ones), a separate
/// library, another port. In that mode the app also never touches the login item, the old
/// launchd agent, or anything that asks for an administrator password.
public struct ZoneEnvironment: Equatable, Sendable {
    /// The home folder zone is told about.
    public var home: URL
    /// zone's data folder: the library, the log, and the app's own copy of the program.
    public var dataDirectory: URL
    /// Passed to zone as ZONE_HOME when the data folder is not the usual one.
    public var zoneHome: URL?
    /// Passed as ZONE_PORT.
    public var port: Int?
    /// Passed as ZONE_NAME: the name the phone sees for this computer.
    public var computerName: String?
    /// The PATH zone searches for AI client commands.
    public var searchPath: String
    public var isSandbox: Bool
    /// Variables handed through untouched (where AI clients keep their settings).
    public var passthrough: [String: String]

    /// Variables that tell an AI client where its settings are. zone reads them too.
    static let clientVariables = [
        "CLAUDE_CONFIG_DIR", "CODEX_HOME", "HERMES_HOME", "GEMINI_CLI_HOME", "QWEN_HOME",
        "COPILOT_HOME", "GOOSE_PATH_ROOT", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
    ]

    public static let sandboxVariable = "ZONE_MAC_SANDBOX"
    public static let defaultSandboxPort = 47991

    public init(
        home: URL,
        dataDirectory: URL,
        zoneHome: URL? = nil,
        port: Int? = nil,
        computerName: String? = nil,
        searchPath: String = "/usr/bin:/bin:/usr/sbin:/sbin",
        isSandbox: Bool = false,
        passthrough: [String: String] = [:]
    ) {
        self.home = home
        self.dataDirectory = dataDirectory
        self.zoneHome = zoneHome
        self.port = port
        self.computerName = computerName
        self.searchPath = searchPath
        self.isSandbox = isSandbox
        self.passthrough = passthrough
    }

    /// Works out the environment from the app's own environment variables.
    public static func resolve(from variables: [String: String], home realHome: URL) -> ZoneEnvironment {
        func value(_ name: String) -> String? {
            guard let text = variables[name], !text.isEmpty else { return nil }
            return text
        }
        let name = value("ZONE_NAME")
        if let sandbox = value(sandboxVariable) {
            let root = URL(fileURLWithPath: sandbox, isDirectory: true)
            let zoneHome = root.appendingPathComponent("zone-home", isDirectory: true)
            return ZoneEnvironment(
                home: root.appendingPathComponent("home", isDirectory: true),
                dataDirectory: zoneHome,
                zoneHome: zoneHome,
                port: value("ZONE_PORT").flatMap(Int.init) ?? defaultSandboxPort,
                computerName: name,
                // The sandbox's own bin folder comes first, so a test can stand in for a
                // system command (a pretend `open` that notes the link instead of starting
                // the browser).
                searchPath: "\(root.appendingPathComponent("bin").path):/usr/bin:/bin:/usr/sbin:/sbin",
                isSandbox: true,
                // A test run never writes the real clipboard: what zone would put there for
                // an arriving clip goes to the file `clipboard` in the sandbox folder.
                passthrough: ["ZONE_CLIPBOARD_COMMAND": "cat > '\(root.appendingPathComponent("clipboard").path)'"]
            )
        }
        let zoneHome = value("ZONE_HOME").map { URL(fileURLWithPath: $0, isDirectory: true) }
        let data = zoneHome ?? realHome.appendingPathComponent("Library/Application Support/Zone", isDirectory: true)
        var passthrough: [String: String] = [:]
        for key in clientVariables {
            if let text = value(key) { passthrough[key] = text }
        }
        return ZoneEnvironment(
            home: realHome,
            dataDirectory: data,
            zoneHome: zoneHome,
            port: value("ZONE_PORT").flatMap(Int.init),
            computerName: name,
            searchPath: value("PATH") ?? "/usr/bin:/bin:/usr/sbin:/sbin",
            isSandbox: false,
            passthrough: passthrough
        )
    }

    /// The whole environment a `zone` child process gets. Nothing else is inherited, so a
    /// variable in the app's own environment can never point zone at the wrong files.
    public func childEnvironment() -> [String: String] {
        var env = passthrough
        env["HOME"] = home.path
        env["PATH"] = searchPath
        env["LANG"] = "en_US.UTF-8"
        env["USER"] = NSUserName()
        env["LOGNAME"] = NSUserName()
        env["TMPDIR"] = NSTemporaryDirectory()
        if let zoneHome { env["ZONE_HOME"] = zoneHome.path }
        if let port { env["ZONE_PORT"] = String(port) }
        if let computerName { env["ZONE_NAME"] = computerName }
        return env
    }

    /// Sandbox only: the file that stands in for the clipboard.
    public var pretendClipboard: URL? {
        isSandbox ? dataDirectory.deletingLastPathComponent().appendingPathComponent("clipboard") : nil
    }

    public var logFile: URL { dataDirectory.appendingPathComponent("zone.log") }

    /// The app's own copy of the `zone` program. AI clients and the `zone` command point
    /// here, so they keep working when the app is updated, moved or run from a disk image.
    public var stableBinary: URL { dataDirectory.appendingPathComponent("bin/zone") }

    /// What the app remembers between runs (first-run progress, the hide text switch).
    public var appStateFile: URL { dataDirectory.appendingPathComponent("mac-app.json") }

    /// The launchd agent that `zone install` writes: the old way of starting zone at login.
    public var legacyLaunchAgent: URL {
        home.appendingPathComponent("Library/LaunchAgents/\(Self.legacyLabel).plist")
    }

    public static let legacyLabel = "wtf.zayd.zone.link"
}

/// What the app remembers between runs. Kept as a small file in the data folder, so a test
/// run in a sandbox folder starts fresh and never sees the real app's memory.
public struct AppState: Codable, Equatable, Sendable {
    /// True once the person finished or skipped the welcome window.
    public var welcomeDone = false
    /// The welcome step to come back to.
    public var welcomeStep = 0
    /// Hide what was said, for sharing a screen.
    public var hideText = false
    /// The person said no to taking over from the old login item: do not ask again.
    public var declinedTakeover = false
    /// Sandbox only: the pretend "start at login" switch.
    public var pretendLoginItem = false
    /// True once Zone has looked whether it may reach the local network, which the first
    /// time is what makes macOS ask. After that, looking again never asks.
    public var localNetworkAsked = false

    public init() {}

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        welcomeDone = try c.decodeIfPresent(Bool.self, forKey: .welcomeDone) ?? false
        welcomeStep = try c.decodeIfPresent(Int.self, forKey: .welcomeStep) ?? 0
        hideText = try c.decodeIfPresent(Bool.self, forKey: .hideText) ?? false
        declinedTakeover = try c.decodeIfPresent(Bool.self, forKey: .declinedTakeover) ?? false
        pretendLoginItem = try c.decodeIfPresent(Bool.self, forKey: .pretendLoginItem) ?? false
        localNetworkAsked = try c.decodeIfPresent(Bool.self, forKey: .localNetworkAsked) ?? false
    }

    public static func load(from file: URL) -> AppState {
        guard let data = try? Data(contentsOf: file),
              let state = try? JSONDecoder().decode(AppState.self, from: data)
        else { return AppState() }
        return state
    }

    public func save(to file: URL) {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(self) else { return }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: file, options: .atomic)
    }
}
