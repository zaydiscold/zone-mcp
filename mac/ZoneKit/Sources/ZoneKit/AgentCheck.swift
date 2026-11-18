import Foundation

// Checking that an agent can really reach the library: read from the agent's own settings
// how it would start zone (which program, which arguments), start exactly that, and speak
// the first words of MCP to it. Nothing here goes by what zone says about itself.

/// How an AI client starts zone, as its settings file has it.
public struct AgentLaunch: Equatable, Sendable {
    public var command: String
    public var arguments: [String]
    public var environment: [String: String]

    public init(command: String, arguments: [String] = ["mcp"], environment: [String: String] = [:]) {
        self.command = command
        self.arguments = arguments
        self.environment = environment
    }
}

public enum AgentConfigError: Error, Equatable, LocalizedError {
    case noFile(String)
    case noEntry
    case unreadable(String)

    public var errorDescription: String? {
        switch self {
        case let .noFile(path): return "Its settings file is not there: \(path)"
        case .noEntry: return "Its settings have no zone entry"
        case let .unreadable(why): return "Its settings could not be read: \(why)"
        }
    }
}

/// Reads zone's entry out of an AI client's settings file. Each client keeps its servers
/// in its own file and shape; this knows the same ones zone writes (`src/clients.rs`).
public enum AgentConfig {
    enum Format: Equatable {
        /// A JSON file, comments allowed. The entry is at these keys, under "zone".
        case json([String])
        /// Codex: the table `[mcp_servers.zone]`.
        case toml
        /// A YAML file: the entry is in the mapping under this top-level key, as "zone".
        case yaml(String)
    }

    static func format(for client: String) -> Format {
        switch client {
        case "codex": return .toml
        case "hermes": return .yaml("mcp_servers")
        case "goose": return .yaml("extensions")
        case "opencode", "crush": return .json(["mcp"])
        case "vscode": return .json(["servers"])
        case "zed": return .json(["context_servers"])
        case "amp": return .json(["amp.mcpServers"])
        default: return .json(["mcpServers"])
        }
    }

    /// How the client whose settings are at `path` starts zone.
    public static func launch(client: String, path: String) -> Result<AgentLaunch, AgentConfigError> {
        guard let data = FileManager.default.contents(atPath: path) else { return .failure(.noFile(path)) }
        return launch(client: client, text: String(decoding: data, as: UTF8.self))
    }

    public static func launch(client: String, text: String) -> Result<AgentLaunch, AgentConfigError> {
        switch format(for: client) {
        case let .json(keys): return fromJSON(text, keys: keys + ["zone"])
        case .toml: return fromTOML(text)
        case let .yaml(key): return fromYAML(text, under: key)
        }
    }

    // MARK: JSON

    static func fromJSON(_ text: String, keys: [String]) -> Result<AgentLaunch, AgentConfigError> {
        // Plain JSON first: a large settings file is read in one go, and only one with
        // comments in it is gone through a character at a time.
        let plain = try? JSONSerialization.jsonObject(with: Data(text.utf8))
        guard let root = plain ?? withoutComments(text).data(using: .utf8).flatMap({ try? JSONSerialization.jsonObject(with: $0) })
        else { return .failure(.unreadable("it is not JSON")) }
        var value: Any? = root
        for key in keys {
            value = (value as? [String: Any])?[key]
        }
        guard let entry = value as? [String: Any] else { return .failure(.noEntry) }
        let strings = { (value: Any?) in (value as? [Any])?.compactMap { $0 as? String } ?? [] }
        let pairs = { (value: Any?) in ((value as? [String: Any]) ?? [:]).compactMapValues { $0 as? String } }
        let environment = pairs(entry["env"]).merging(pairs(entry["environment"])) { _, new in new }
        switch entry["command"] {
        case let command as String:
            return .success(AgentLaunch(command: command, arguments: strings(entry["args"]), environment: environment))
        case let parts as [Any]:
            // opencode: the program and its arguments in one list.
            let all = parts.compactMap { $0 as? String }
            guard let command = all.first else { return .failure(.unreadable("its zone entry names no program")) }
            return .success(AgentLaunch(command: command, arguments: Array(all.dropFirst()), environment: environment))
        case let inner as [String: Any]:
            // Zed's older shape: {"command": {"path", "args", "env"}}.
            guard let command = inner["path"] as? String else { return .failure(.unreadable("its zone entry names no program")) }
            return .success(AgentLaunch(command: command, arguments: strings(inner["args"]), environment: pairs(inner["env"])))
        default:
            return .failure(.unreadable("its zone entry names no program"))
        }
    }

    /// JSON with `//` and `/* */` comments and trailing commas taken out (Zed, VS Code and
    /// opencode allow them). What is inside a string is left alone.
    static func withoutComments(_ text: String) -> String {
        var out = ""
        out.reserveCapacity(text.count)
        var inString = false, escaped = false
        var index = text.startIndex
        while index < text.endIndex {
            let character = text[index]
            let next = text.index(after: index)
            if inString {
                out.append(character)
                if escaped { escaped = false } else if character == "\\" { escaped = true } else if character == "\"" { inString = false }
                index = next
            } else if character == "\"" {
                inString = true
                out.append(character)
                index = next
            } else if character == "/", next < text.endIndex, text[next] == "/" {
                index = text[index...].firstIndex(where: \.isNewline) ?? text.endIndex
            } else if character == "/", next < text.endIndex, text[next] == "*" {
                let rest = text[text.index(after: next)...]
                index = rest.range(of: "*/")?.upperBound ?? text.endIndex
            } else if character == "," {
                // A comma with nothing after it but space (or comments) before } or ].
                let after = firstAfterSpaceAndComments(text[next...])
                if after != "}" && after != "]" { out.append(character) }
                index = next
            } else {
                out.append(character)
                index = next
            }
        }
        return out
    }

    private static func firstAfterSpaceAndComments(_ text: Substring) -> Character? {
        var rest = text
        while true {
            rest = rest.drop(while: \.isWhitespace)
            if rest.hasPrefix("//") {
                rest = rest.drop(while: { !$0.isNewline })
            } else if rest.hasPrefix("/*") {
                guard let end = rest.range(of: "*/") else { return nil }
                rest = rest[end.upperBound...]
            } else {
                return rest.first
            }
        }
    }

    // MARK: TOML

    static func fromTOML(_ text: String) -> Result<AgentLaunch, AgentConfigError> {
        var inTable = false, found = false
        var command: String?
        var arguments: [String] = []
        var environment: [String: String] = [:]
        var inEnvTable = false
        for raw in text.split(whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("[") {
                let name = line.drop(while: { $0 == "[" }).prefix(while: { $0 != "]" })
                    .split(separator: ".").map { $0.trimmingCharacters(in: CharacterSet(charactersIn: " \"'")) }.joined(separator: ".")
                inTable = name == "mcp_servers.zone"
                inEnvTable = name == "mcp_servers.zone.env"
                found = found || inTable
                continue
            }
            guard inTable || inEnvTable, let equals = line.firstIndex(of: "="), !line.hasPrefix("#") else { continue }
            let key = line[..<equals].trimmingCharacters(in: .whitespaces)
            let value = line[line.index(after: equals)...].trimmingCharacters(in: .whitespaces)
            if inEnvTable {
                if let text = quoted(value).first { environment[key] = text }
            } else if key == "command" {
                command = quoted(value).first
            } else if key == "args" {
                arguments = quoted(value)
            } else if key == "env" {
                // env = { NAME = "value", OTHER = "value" }
                let inner = value.trimmingCharacters(in: CharacterSet(charactersIn: "{} "))
                for pair in splitOutsideQuotes(inner, at: ",") {
                    guard let equals = pair.firstIndex(of: "=") else { continue }
                    let name = pair[..<equals].trimmingCharacters(in: CharacterSet(charactersIn: " \"'"))
                    if let text = quoted(String(pair[pair.index(after: equals)...])).first { environment[name] = text }
                }
            }
        }
        guard found else { return .failure(.noEntry) }
        guard let command else { return .failure(.unreadable("its zone entry names no program")) }
        return .success(AgentLaunch(command: command, arguments: arguments, environment: environment))
    }

    // MARK: YAML

    /// Reads `<key>:` / `  zone:` / `    command: ...` the way zone writes it and the way
    /// people write it by hand: a list in brackets or one item to a line.
    static func fromYAML(_ text: String, under key: String) -> Result<AgentLaunch, AgentConfigError> {
        let lines = text.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline).map(String.init)
        func indent(_ line: String) -> Int { line.prefix(while: { $0 == " " }).count }
        func blank(_ line: String) -> Bool {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty || trimmed.hasPrefix("#")
        }
        guard let top = lines.firstIndex(where: { $0.hasPrefix("\(key):") }) else { return .failure(.noEntry) }
        var entry: Int?
        var index = top + 1
        while index < lines.count {
            let line = lines[index]
            if !blank(line) {
                if indent(line) == 0 { break }
                let name = line.trimmingCharacters(in: .whitespaces)
                if name == "zone:" || name == "\"zone\":" || name == "'zone':" { entry = index; break }
            }
            index += 1
        }
        guard let entry else { return .failure(.noEntry) }
        let depth = indent(lines[entry])
        var command: String?
        var arguments: [String] = []
        var environment: [String: String] = [:]
        var list: String?
        index = entry + 1
        while index < lines.count {
            let line = lines[index]
            index += 1
            if blank(line) { continue }
            if indent(line) <= depth { break }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("- ") || trimmed == "-" {
                if list == "args" { arguments.append(scalar(String(trimmed.dropFirst(1)))) }
                continue
            }
            guard let colon = trimmed.firstIndex(of: ":") else { continue }
            let name = String(trimmed[..<colon])
            let value = trimmed[trimmed.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            if list == "env", indent(line) > depth + 2, !value.isEmpty, !["command", "cmd", "args"].contains(name) {
                environment[name] = scalar(value)
                continue
            }
            list = nil
            switch name {
            case "command", "cmd": command = scalar(value)
            case "args":
                if value.hasPrefix("[") { arguments = quotedOrBare(value) } else { list = "args" }
            case "env", "envs":
                if value.isEmpty { list = "env" }
            default: break
            }
        }
        guard let command, !command.isEmpty else { return .failure(.unreadable("its zone entry names no program")) }
        return .success(AgentLaunch(command: command, arguments: arguments, environment: environment))
    }

    // MARK: Small readers

    /// Every double (or single) quoted string in a piece of text, with the usual escapes read.
    static func quoted(_ text: String) -> [String] {
        var found: [String] = []
        var current: String?
        var quote: Character = "\""
        var escaped = false
        for character in text {
            if var open = current {
                if escaped {
                    switch character {
                    case "n": open.append("\n")
                    case "t": open.append("\t")
                    default: open.append(character)
                    }
                    escaped = false
                    current = open
                } else if character == "\\", quote == "\"" {
                    escaped = true
                } else if character == quote {
                    found.append(open)
                    current = nil
                } else {
                    open.append(character)
                    current = open
                }
            } else if character == "\"" || character == "'" {
                quote = character
                current = ""
            }
        }
        return found
    }

    /// The items of `[a, "b", c]`, quoted or not.
    static func quotedOrBare(_ text: String) -> [String] {
        let inner = text.trimmingCharacters(in: CharacterSet(charactersIn: "[] "))
        return splitOutsideQuotes(inner, at: ",").map(scalar).filter { !$0.isEmpty }
    }

    /// One YAML value: quotes taken off, a comment after a bare value dropped.
    static func scalar(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("\"") || trimmed.hasPrefix("'") { return quoted(trimmed).first ?? "" }
        if let hash = trimmed.range(of: " #") { return String(trimmed[..<hash.lowerBound]).trimmingCharacters(in: .whitespaces) }
        return trimmed
    }

    static func splitOutsideQuotes(_ text: String, at separator: Character) -> [String] {
        var parts: [String] = []
        var current = ""
        var quote: Character?
        var escaped = false
        for character in text {
            if let open = quote {
                current.append(character)
                if escaped { escaped = false } else if character == "\\" { escaped = true } else if character == open { quote = nil }
            } else if character == "\"" || character == "'" {
                quote = character
                current.append(character)
            } else if character == separator {
                parts.append(current)
                current = ""
            } else {
                current.append(character)
            }
        }
        if !current.trimmingCharacters(in: .whitespaces).isEmpty { parts.append(current) }
        return parts
    }
}

/// Starts an MCP server the way a client would and asks it the two things every client
/// asks first: who are you (`initialize`), and what can you do (`tools/list`).
public enum MCPProbe {
    public struct Answer: Equatable, Sendable {
        /// The names of the tools the server offers.
        public var tools: [String]
        public var server: String?
        public var version: String?

        public init(tools: [String], server: String? = nil, version: String? = nil) {
            self.tools = tools
            self.server = server
            self.version = version
        }
    }

    public enum Outcome: Equatable, Sendable {
        case answers(Answer)
        case failed(String)

        /// "Answers: 6 tools", or what went wrong.
        public var words: String {
            switch self {
            case let .answers(answer):
                return "Answers: \(answer.tools.count) \(answer.tools.count == 1 ? "tool" : "tools")"
            case let .failed(why): return "No answer: \(why)"
            }
        }

        public var answered: Bool {
            if case .answers = self { return true }
            return false
        }
    }

    static func request(id: Int, method: String, params: [String: Any]? = nil) -> Data {
        var message: [String: Any] = ["jsonrpc": "2.0", "method": method]
        if id > 0 { message["id"] = id }
        if let params { message["params"] = params }
        var data = (try? JSONSerialization.data(withJSONObject: message, options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data()
        data.append(0x0A)
        return data
    }

    static func initialize(clientVersion: String) -> Data {
        request(id: 1, method: "initialize", params: [
            "protocolVersion": "2025-06-18",
            "capabilities": [String: Any](),
            "clientInfo": ["name": "Zone for Mac", "version": clientVersion],
        ])
    }

    /// The answer to request `id` among these lines: its result, or the server's error.
    static func response(id: Int, in lines: [String]) -> Result<[String: Any], ProbeError>? {
        for line in lines {
            guard let data = line.data(using: .utf8),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  (object["id"] as? NSNumber)?.intValue == id
            else { continue }
            if let error = object["error"] as? [String: Any] {
                return .failure(ProbeError(message: (error["message"] as? String) ?? "the server refused"))
            }
            return .success((object["result"] as? [String: Any]) ?? [:])
        }
        return nil
    }

    struct ProbeError: Error, Equatable { var message: String }

    /// The whole check. Blocks until the server has answered both questions, stopped, or
    /// `timeout` has passed, so call it off the main thread.
    public static func run(_ launch: AgentLaunch, baseEnvironment: [String: String], clientVersion: String = "0", timeout: TimeInterval = 10) -> Outcome {
        // A server that stops early must not take this program down with it when written to.
        signal(SIGPIPE, SIG_IGN)
        let fm = FileManager.default
        guard fm.fileExists(atPath: launch.command) else { return .failed("the program is not there (\(launch.command))") }
        guard fm.isExecutableFile(atPath: launch.command) else { return .failed("the program cannot be run (\(launch.command))") }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: launch.command)
        process.arguments = launch.arguments
        process.environment = baseEnvironment.merging(launch.environment) { _, fromConfig in fromConfig }
        let input = Pipe(), output = Pipe(), errors = Pipe()
        process.standardInput = input
        process.standardOutput = output
        process.standardError = errors

        final class Heard: @unchecked Sendable {
            let lock = NSLock()
            var out = Data()
            var err = Data()
            let news = DispatchSemaphore(value: 0)
        }
        let heard = Heard()
        output.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            heard.lock.lock()
            heard.out.append(data)
            heard.lock.unlock()
            heard.news.signal()
        }
        errors.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            heard.lock.lock()
            heard.err.append(data)
            heard.lock.unlock()
        }
        process.terminationHandler = { _ in heard.news.signal() }
        do {
            try process.run()
        } catch {
            return .failed("it could not be started: \(error.localizedDescription)")
        }
        defer {
            output.fileHandleForReading.readabilityHandler = nil
            errors.fileHandleForReading.readabilityHandler = nil
            try? input.fileHandleForWriting.close()
            if process.isRunning { process.terminate() }
        }
        let deadline = Date().addingTimeInterval(timeout)

        func lines() -> [String] {
            heard.lock.lock()
            defer { heard.lock.unlock() }
            return String(decoding: heard.out, as: UTF8.self).split(whereSeparator: \.isNewline).map(String.init)
        }
        func stopped() -> String {
            // Give the last of what it printed a moment to arrive.
            Thread.sleep(forTimeInterval: 0.15)
            heard.lock.lock()
            let text = String(decoding: heard.err, as: UTF8.self)
            heard.lock.unlock()
            let line = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? "it stopped without a word (exit \(process.terminationStatus))"
            return line.hasPrefix("zone: ") ? String(line.dropFirst(6)) : line
        }
        func answer(to id: Int) -> Result<[String: Any], ProbeError> {
            while true {
                if let found = response(id: id, in: lines()) { return found }
                if !process.isRunning {
                    if let found = response(id: id, in: lines()) { return found }
                    return .failure(ProbeError(message: stopped()))
                }
                let left = deadline.timeIntervalSinceNow
                if left <= 0 { return .failure(ProbeError(message: "nothing came back in \(Int(timeout)) seconds")) }
                _ = heard.news.wait(timeout: .now() + min(left, 0.25))
            }
        }
        func send(_ data: Data) -> Bool {
            (try? input.fileHandleForWriting.write(contentsOf: data)) != nil
        }

        guard send(initialize(clientVersion: clientVersion)) else { return .failed(stopped()) }
        let hello: [String: Any]
        switch answer(to: 1) {
        case let .success(result): hello = result
        case let .failure(error): return .failed(error.message)
        }
        guard hello["protocolVersion"] != nil || hello["serverInfo"] != nil else {
            return .failed("it answered, but not as an MCP server")
        }
        _ = send(request(id: 0, method: "notifications/initialized"))
        guard send(request(id: 2, method: "tools/list")) else { return .failed(stopped()) }
        switch answer(to: 2) {
        case let .success(result):
            let tools = (result["tools"] as? [[String: Any]])?.compactMap { $0["name"] as? String } ?? []
            let info = hello["serverInfo"] as? [String: Any]
            return .answers(Answer(tools: tools, server: info?["name"] as? String, version: info?["version"] as? String))
        case let .failure(error):
            return .failed(error.message)
        }
    }
}
