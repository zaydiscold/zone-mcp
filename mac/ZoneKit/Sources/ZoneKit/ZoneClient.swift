import Foundation

/// What a finished command left behind.
public struct CommandOutput: Sendable, Equatable {
    public var status: Int32
    public var stdout: Data
    public var stderr: Data

    public init(status: Int32 = 0, stdout: Data = Data(), stderr: Data = Data()) {
        self.status = status
        self.stdout = stdout
        self.stderr = stderr
    }

    public init(status: Int32 = 0, text: String) {
        self.init(status: status, stdout: Data(text.utf8))
    }

    public var text: String { String(decoding: stdout, as: UTF8.self) }
    public var errorText: String { String(decoding: stderr, as: UTF8.self) }
}

public enum ZoneError: Error, LocalizedError, Equatable {
    /// The `zone` program is not where it should be.
    case missing(String)
    /// zone ran and said no. The message is zone's own sentence.
    case failed(String)
    case timedOut(String)
    /// zone printed something this app could not read.
    case unreadable(String)

    public var errorDescription: String? {
        switch self {
        case let .missing(path): return "The zone program is missing at \(path)."
        case let .failed(message): return message
        case let .timedOut(command): return "zone \(command) took too long and was stopped."
        case let .unreadable(command): return "Could not read what zone \(command) printed."
        }
    }
}

/// Runs `zone` with some arguments. A protocol so tests can answer without a real program.
public protocol ZoneRunning: Sendable {
    func run(_ arguments: [String], timeout: TimeInterval) async throws -> CommandOutput
}

/// Runs the real program, with exactly the environment it is given.
public struct ZoneProcessRunner: ZoneRunning {
    public let binary: URL
    public let environment: [String: String]

    public init(binary: URL, environment: [String: String]) {
        self.binary = binary
        self.environment = environment
    }

    public func run(_ arguments: [String], timeout: TimeInterval) async throws -> CommandOutput {
        let binary = binary, environment = environment
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    continuation.resume(returning: try Self.runAndWait(binary: binary, arguments: arguments, environment: environment, timeout: timeout))
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    /// Runs a program to its end and collects what it printed.
    public static func runAndWait(binary: URL, arguments: [String], environment: [String: String], timeout: TimeInterval) throws -> CommandOutput {
        guard FileManager.default.isExecutableFile(atPath: binary.path) else {
            throw ZoneError.missing(binary.path)
        }
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        process.environment = environment
        process.standardInput = FileHandle.nullDevice
        let out = Pipe(), err = Pipe()
        process.standardOutput = out
        process.standardError = err
        try process.run()

        // Both pipes are drained while the program runs, so a long answer never blocks it.
        final class Collected: @unchecked Sendable {
            var out = Data()
            var err = Data()
            var timedOut = false
        }
        let collected = Collected()
        let readers = DispatchGroup()
        readers.enter()
        DispatchQueue.global(qos: .utility).async {
            collected.out = out.fileHandleForReading.readDataToEndOfFile()
            readers.leave()
        }
        readers.enter()
        DispatchQueue.global(qos: .utility).async {
            collected.err = err.fileHandleForReading.readDataToEndOfFile()
            readers.leave()
        }
        let deadline = DispatchWorkItem {
            if process.isRunning {
                collected.timedOut = true
                process.terminate()
            }
        }
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + timeout, execute: deadline)
        process.waitUntilExit()
        deadline.cancel()
        // A grandchild that inherited the pipes (the browser opened by `zone open`) can hold
        // them open after zone itself is done. Do not wait for it for ever.
        _ = readers.wait(timeout: .now() + 2)
        if collected.timedOut {
            throw ZoneError.timedOut(arguments.first ?? "")
        }
        return CommandOutput(status: process.terminationStatus, stdout: collected.out, stderr: collected.err)
    }
}

/// The `zone` commands the app uses, as Swift calls.
public struct ZoneClient: Sendable {
    public let runner: any ZoneRunning

    public init(runner: any ZoneRunning) {
        self.runner = runner
    }

    // MARK: Plumbing

    @discardableResult
    public func run(_ arguments: [String], timeout: TimeInterval = 20) async throws -> CommandOutput {
        let output = try await runner.run(arguments, timeout: timeout)
        guard output.status == 0 else {
            throw ZoneError.failed(Self.message(from: output))
        }
        return output
    }

    func json<T: Decodable>(_ type: T.Type, _ arguments: [String], timeout: TimeInterval = 20) async throws -> T {
        let output = try await run(arguments, timeout: timeout)
        do {
            return try JSONDecoder().decode(T.self, from: output.stdout)
        } catch {
            throw ZoneError.unreadable(arguments.joined(separator: " "))
        }
    }

    /// zone's own sentence about what went wrong, without the "zone: " in front.
    static func message(from output: CommandOutput) -> String {
        let text = (output.errorText.isEmpty ? output.text : output.errorText).trimmingCharacters(in: .whitespacesAndNewlines)
        let line = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? "zone stopped with an error."
        return line.hasPrefix("zone: ") ? String(line.dropFirst(6)) : line
    }

    // MARK: Reading

    public func status() async throws -> StatusReport {
        try await json(StatusReport.self, ["status", "--json"])
    }

    public func devices() async throws -> [Device] {
        try await json([Device].self, ["devices", "--json"])
    }

    public func recent(limit: Int) async throws -> ItemList {
        try await json(ItemList.self, ["list", "--json", "--limit", String(limit)])
    }

    public func search(_ words: String, limit: Int) async throws -> ItemList {
        // "--" so a search that begins with a dash is still read as words.
        try await json(ItemList.self, ["search", "--json", "--limit", String(limit), "--"] + words.split(separator: " ").map(String.init))
    }

    /// How many items were said today.
    public func today() async throws -> StatsReport.Total {
        try await json(StatsReport.self, ["stats", "--json", "--since", "today"]).total
    }

    public func agents() async throws -> AgentSurvey {
        try await json(AgentSurvey.self, ["mcp", "status", "--json"])
    }

    public func version() async throws -> String {
        let text = try await run(["--version"]).text.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.hasPrefix("zone ") ? String(text.dropFirst(5)) : text
    }

    /// True when this zone has the `events` command.
    public func hasEvents() async -> Bool {
        guard let output = try? await runner.run(["events", "--help"], timeout: 10) else { return false }
        return output.status == 0
    }

    /// True when this zone can open the Flash page in one step (`zone open --flash`).
    public func hasFlashOpen() async -> Bool {
        guard let output = try? await runner.run(["open", "--help"], timeout: 10), output.status == 0 else { return false }
        return output.text.contains("--flash")
    }

    /// True when this zone has clips (`zone clip`, `zone clipboard`).
    public func hasClips() async -> Bool {
        guard let output = try? await runner.run(["clipboard", "--help"], timeout: 10) else { return false }
        return output.status == 0
    }

    /// The newest clip, with its text. Nil when there is none.
    public func latestClip() async throws -> Clip? {
        try await json(ClipAnswer.self, ["clip", "--json"]).clip
    }

    /// The recent clips, newest first, as short cards without their text.
    public func clips(limit: Int = 20) async throws -> ClipList {
        try await json(ClipList.self, ["clip", "list", "--json", "--limit", String(limit)])
    }

    /// Whether a clip goes on this computer's clipboard as it arrives.
    public func clipboardSetting() async throws -> ClipboardSetting {
        try await json(ClipboardSetting.self, ["clipboard", "status", "--json"])
    }

    /// Turns that on or off, and returns how it stands afterwards.
    @discardableResult
    public func setClipboard(_ on: Bool) async throws -> ClipboardSetting {
        try await json(ClipboardSetting.self, ["clipboard", on ? "on" : "off", "--json"])
    }

    /// The link to the history page, with the key that unlocks it.
    public func historyLink() async throws -> URL {
        let text = try await run(["open", "--print"]).text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: text) else { throw ZoneError.unreadable("open --print") }
        return url
    }

    /// The link to the Flash page, with the key. Needs a zone that has `open --flash`.
    public func flashLink() async throws -> URL {
        let text = try await run(["open", "--print", "--flash"]).text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: text), url.scheme == "http" else { throw ZoneError.unreadable("open --print --flash") }
        return url
    }

    // MARK: Doing

    public func pair() async throws -> PairCode {
        // --no-wait: print the code and return. An older zone has no such flag (and never waits).
        do {
            return try await json(PairCode.self, ["pair", "--json", "--no-wait"])
        } catch ZoneError.failed(let message) where message.contains("--no-wait") {
            return try await json(PairCode.self, ["pair", "--json"])
        }
    }

    /// Opens the history page in the browser.
    public func openHistory() async throws {
        try await run(["open"])
    }

    /// Opens the Flash page in the browser. Needs a zone that has `open --flash`.
    public func openFlash() async throws {
        try await run(["open", "--flash"])
    }

    /// Asks one phone, or every phone, to send its whole library again at its next sync.
    @discardableResult
    public func resync(device: String? = nil) async throws -> String {
        try await run(["resync"] + (device.map { [$0] } ?? [])).text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public func unpair(_ device: String) async throws {
        try await run(["unpair", device])
    }

    public func setBackupFolder(_ folder: URL) async throws {
        // Reading a first backup can take a while (and macOS may ask for permission).
        try await run(["backup-folder", folder.path], timeout: 120)
    }

    public func clearBackupFolder() async throws {
        try await run(["backup-folder", "--clear"])
    }

    /// Adds zone to an AI client (every profile it has), or changes what it may read.
    /// With no view given, the one it has is kept.
    public func connect(_ agent: Agent, view: AgentView? = nil) async throws {
        try await run(agent.installArguments(view: view), timeout: 90)
    }

    /// True when the zone that is running writes down what happens (so `zone events` has
    /// something to say about it). An older zone that the app did not start may not.
    public func daemonRecordsEvents(pid: Int) async -> Bool {
        guard let output = try? await runner.run(["events", "--json", "--limit", "2000"], timeout: 15), output.status == 0 else { return false }
        return EventLineDecoder.daemonStarted(pid: pid, in: output.text)
    }

    /// Takes zone out of an AI client.
    public func disconnect(agent: String) async throws {
        try await run(["mcp", "uninstall", agent], timeout: 90)
    }

    /// Removes the login item that `zone install` wrote and stops the zone it started.
    public func removeLoginItem() async throws {
        try await run(["uninstall"], timeout: 30)
    }
}
