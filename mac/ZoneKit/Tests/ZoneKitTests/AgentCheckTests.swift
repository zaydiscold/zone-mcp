import XCTest
@testable import ZoneKit

/// The agents as the panel lists them, reading how an agent starts zone out of its own
/// settings, and the check that starts it and speaks MCP to it.
final class AgentCheckTests: XCTestCase {
    private func place(_ client: String, _ label: String, _ profile: String? = nil, state: String, detail: String? = nil, scope: AgentSurvey.Scope? = .init(kinds: ["dictation", "note", "meeting", "clip"], limited: false)) -> AgentSurvey.Place {
        .init(client: client, label: label, profile: profile, path: "/home/\(label)", state: state, detail: detail, scope: state == "registered" || state == "other_copy" ? scope : nil)
    }

    /// The shape of the owner's Mac: many profiles of three apps, one app that starts
    /// another copy, and a dozen the person took zone out of.
    private var survey: AgentSurvey {
        var places: [AgentSurvey.Place] = []
        for profile in ["default", "~/.claude_work", "~/.claude_a", "~/.claude_b", "~/.claude_c", "~/.claude_d", "~/.claude_e"] {
            places.append(place("claude-code", "Claude Code (\(profile))", profile, state: "registered"))
        }
        places.append(place("claude-desktop", "Claude Desktop", state: "removed_by_you"))
        places.append(place("codex", "Codex (default)", "default", state: "registered"))
        places.append(place("codex", "Codex (~/.codex_b)", "~/.codex_b", state: "registered"))
        places.append(place("cursor", "Cursor", state: "registered"))
        for profile in ["default", "profile a", "profile b", "profile c", "profile d"] {
            places.append(place("hermes", "Hermes (\(profile))", profile, state: "registered"))
        }
        places.append(place("gemini", "Gemini CLI", state: "removed_by_you"))
        places.append(place("opencode", "opencode", state: "other_copy", detail: "/Users/me/Library/Application Support/Zone/bin/zone"))
        places.append(place("zed", "Zed", state: "not_registered"))
        places.append(.init(client: "amp", label: "Amp", installed: false, state: "not_registered"))
        return AgentSurvey(zone: "/Users/me/.local/bin/zone", clients: places)
    }

    func testManyProfilesAreOneRowEach() {
        let agents = Agent.list(from: survey, knowsClips: true)
        XCTAssertEqual(agents.map(\.id), ["claude-code", "claude-desktop", "codex", "cursor", "hermes", "gemini", "opencode", "zed"])
        let sections = AgentSections(agents)
        XCTAssertEqual(sections.on.map(\.title), ["Claude Code", "Codex", "Cursor", "Hermes", "opencode"])
        XCTAssertEqual(sections.off.map(\.title), ["Claude Desktop", "Gemini CLI", "Zed"])
        XCTAssertEqual(sections.summary, "5 apps connected, 16 profiles in all")
        XCTAssertEqual(sections.offTitle, "3 not connected (2 removed by you)")

        let byID = Dictionary(uniqueKeysWithValues: agents.map { ($0.id, $0) })
        XCTAssertEqual(byID["claude-code"]?.note, "7 profiles, reads everything")
        XCTAssertEqual(byID["codex"]?.note, "2 profiles, reads everything")
        XCTAssertEqual(byID["cursor"]?.note, "Reads everything")
        XCTAssertEqual(byID["hermes"]?.note, "5 profiles, reads everything")
        XCTAssertEqual(byID["claude-desktop"]?.note, "Off, removed by you")
        XCTAssertEqual(byID["claude-desktop"]?.removedByYou, true)
        XCTAssertEqual(byID["zed"]?.note, "Not connected")
        XCTAssertEqual(byID["zed"]?.removedByYou, false)
    }

    func testEveryPlaceSaysWhatZoneSaysAboutIt() {
        let agents = Agent.list(from: survey, knowsClips: true)
        let claude = agents[0]
        XCTAssertEqual(claude.places.map(\.name), ["default", "~/.claude_work", "~/.claude_a", "~/.claude_b", "~/.claude_c", "~/.claude_d", "~/.claude_e"])
        XCTAssertEqual(claude.places[0].standing, .registered)
        XCTAssertEqual(claude.places[0].words(), "Connected")
        XCTAssertEqual(claude.places[0].limit(knowsClips: true), "reads everything")
        XCTAssertEqual(agents[1].places[0].standing, .removedByYou)
        XCTAssertEqual(agents[1].places[0].words(), "Removed by you")
        XCTAssertNil(agents[1].places[0].limit(knowsClips: true))
        XCTAssertEqual(agents[7].places[0].words(), "Not connected")
        XCTAssertEqual(AgentSurvey.Place(client: "x", label: "X", state: "unreadable", detail: "not valid YAML").words(), "Its settings could not be read: not valid YAML")
        let limited = place("cursor", "Cursor", state: "registered", scope: .init(kinds: ["note", "meeting"], limited: true))
        XCTAssertEqual(limited.limit(knowsClips: true), "reads notes and meetings only")
    }

    func testAnAgentThatStartsAnotherCopyIsExplainedAndFixedInOneStep() throws {
        let opencode = try XCTUnwrap(Agent.list(from: survey, knowsClips: true).first { $0.id == "opencode" })
        XCTAssertEqual(opencode.standing, .otherCopy("/Users/me/Library/Application Support/Zone/bin/zone"))
        XCTAssertTrue(opencode.isOn, "it does have a zone")
        XCTAssertEqual(opencode.note, "Starts another copy of zone")
        XCTAssertEqual(opencode.places[0].words(home: "/Users/me"), "Starts another copy of zone: ~/Library/Application Support/Zone/bin/zone")
        XCTAssertEqual(
            opencode.otherCopyExplanation(home: "/Users/me"),
            "opencode starts another copy of zone (~/Library/Application Support/Zone/bin/zone), not the one that runs on this Mac. It reads the same library, but is updated on its own and can fall behind."
        )
        // The fix is `zone mcp install opencode` with the view it has, so nothing widens.
        XCTAssertTrue(opencode.otherCopyKeepsView)
        XCTAssertEqual(opencode.installArguments(), ["mcp", "install", "opencode", "--kinds", "all", "--exclude-sources", "none"])
        // An older zone cannot say what the other entry allows.
        let old = Agent.list(from: AgentSurvey(clients: [.init(client: "opencode", label: "opencode", state: "other_copy", detail: "/x/zone")]))[0]
        XCTAssertFalse(old.otherCopyKeepsView)
        XCTAssertEqual(old.installArguments(), ["mcp", "install", "opencode"])
    }

    func testProfilesThatDoNotAgree() {
        let mixed = Agent.list(from: AgentSurvey(clients: [
            place("codex", "Codex (default)", "default", state: "registered"),
            place("codex", "Codex (~/.codex_b)", "~/.codex_b", state: "other_copy", detail: "/old/zone"),
            place("codex", "Codex (~/.codex_c)", "~/.codex_c", state: "not_registered"),
        ]), knowsClips: true)[0]
        XCTAssertEqual(mixed.standing, .partly(places: 2, total: 3))
        XCTAssertEqual(mixed.note, "Connected in 2 of 3 profiles, 1 with another copy of zone")
        XCTAssertEqual(mixed.otherCopyExplanation(), "1 of its profiles start another copy of zone (/old/zone), not the one that runs on this Mac. It reads the same library, but is updated on its own and can fall behind.")

        let everywhere = Agent.list(from: AgentSurvey(clients: [
            place("codex", "Codex (default)", "default", state: "registered"),
            place("codex", "Codex (~/.codex_b)", "~/.codex_b", state: "other_copy", detail: "/old/zone"),
        ]), knowsClips: true)[0]
        XCTAssertEqual(everywhere.standing, .otherCopy("/old/zone"))
        XCTAssertEqual(everywhere.note, "1 of 2 profiles start another copy of zone")

        let none = AgentSections(Agent.list(from: AgentSurvey(clients: [place("zed", "Zed", state: "not_registered")])))
        XCTAssertEqual(none.summary, "None connected yet")
        XCTAssertEqual(none.offTitle, "1 not connected")
    }

    // MARK: Reading an agent's settings

    func testReadsTheEntryEachKindOfClientWrites() throws {
        let plain = #"{"mcpServers": {"other": {"command": "x"}, "zone": {"type": "stdio", "command": "/a/zone", "args": ["mcp", "--kinds", "note,meeting"], "env": {"ZONE_HOME": "/lib"}}}}"#
        XCTAssertEqual(try AgentConfig.launch(client: "claude-code", text: plain).get(), AgentLaunch(command: "/a/zone", arguments: ["mcp", "--kinds", "note,meeting"], environment: ["ZONE_HOME": "/lib"]))

        let opencode = #"{"mcp": {"zone": {"type": "local", "command": ["/a b/zone", "mcp", "--read-only"], "enabled": true, "environment": {"ZONE_HOME": "/lib"}}}}"#
        XCTAssertEqual(try AgentConfig.launch(client: "opencode", text: opencode).get(), AgentLaunch(command: "/a b/zone", arguments: ["mcp", "--read-only"], environment: ["ZONE_HOME": "/lib"]))

        // Zed: comments, a trailing comma, and its older shape.
        let zed = """
        // Zed settings
        {
          "theme": "One // Dark", /* not a comment inside the string */
          "context_servers": {
            "zone": {"command": {"path": "/a/zone", "args": ["mcp"], "env": {}}},
          },
        }
        """
        XCTAssertEqual(try AgentConfig.launch(client: "zed", text: zed).get(), AgentLaunch(command: "/a/zone", arguments: ["mcp"]))
        XCTAssertEqual(try AgentConfig.launch(client: "vscode", text: #"{"servers": {"zone": {"type": "stdio", "command": "/a/zone", "args": ["mcp"]}}}"#).get().command, "/a/zone")
        XCTAssertEqual(try AgentConfig.launch(client: "amp", text: #"{"amp.mcpServers": {"zone": {"command": "/a/zone", "args": ["mcp"]}}}"#).get().command, "/a/zone")

        let codex = """
        model = "gpt"

        [mcp_servers.other]
        command = "nope"

        [mcp_servers.zone]
        command = "/a \\"b\\"/zone"
        args = ["mcp", "--exclude-sources", "keyboard"]
        env = { ZONE_HOME = "/lib, here" }

        [profiles.x]
        command = "also nope"
        """
        XCTAssertEqual(try AgentConfig.launch(client: "codex", text: codex).get(), AgentLaunch(command: "/a \"b\"/zone", arguments: ["mcp", "--exclude-sources", "keyboard"], environment: ["ZONE_HOME": "/lib, here"]))

        let hermes = """
        model: demo
        mcp_servers:
          # the library
          other:
            command: "nope"
          zone:
            command: "/a/zone"
            args: ["mcp", "--read-only"]
            env:
              ZONE_HOME: "/lib"
        after: 1
        """
        XCTAssertEqual(try AgentConfig.launch(client: "hermes", text: hermes).get(), AgentLaunch(command: "/a/zone", arguments: ["mcp", "--read-only"], environment: ["ZONE_HOME": "/lib"]))
        // Written by hand, one item to a line.
        let byHand = "mcp_servers:\n  zone:\n    command: /a/zone # mine\n    args:\n      - mcp\n      - '--read-only'\n"
        XCTAssertEqual(try AgentConfig.launch(client: "hermes", text: byHand).get(), AgentLaunch(command: "/a/zone", arguments: ["mcp", "--read-only"]))
        let goose = "extensions:\n  zone:\n    name: \"zone\"\n    cmd: \"/a/zone\"\n    args: [\"mcp\"]\n    envs: {}\n"
        XCTAssertEqual(try AgentConfig.launch(client: "goose", text: goose).get(), AgentLaunch(command: "/a/zone", arguments: ["mcp"]))
    }

    func testSaysWhatIsMissing() {
        XCTAssertEqual(AgentConfig.launch(client: "cursor", text: #"{"mcpServers": {}}"#), .failure(.noEntry))
        XCTAssertEqual(AgentConfig.launch(client: "cursor", text: "not json"), .failure(.unreadable("it is not JSON")))
        XCTAssertEqual(AgentConfig.launch(client: "codex", text: "model = \"x\"\n"), .failure(.noEntry))
        XCTAssertEqual(AgentConfig.launch(client: "hermes", text: "model: demo\n"), .failure(.noEntry))
        XCTAssertEqual(AgentConfig.launch(client: "cursor", path: "/nowhere/mcp.json"), .failure(.noFile("/nowhere/mcp.json")))
        XCTAssertEqual(AgentConfigError.noEntry.localizedDescription, "Its settings have no zone entry")
    }

    // MARK: The check itself

    /// A stand-in server: a shell script that answers the two questions the way an MCP
    /// server does, or misbehaves in the way named.
    private func server(_ behaviour: String) throws -> AgentLaunch {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("zone-probe-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: folder) }
        let script: String
        switch behaviour {
        case "answers":
            script = """
            #!/bin/sh
            [ "$1" = "mcp" ] || exit 9
            while IFS= read -r line; do
              case "$line" in
                *'"method":"initialize"'*) echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"zone","version":"0.1.2"}}}' ;;
                *'"method":"tools/list"'*) echo "{\\"jsonrpc\\":\\"2.0\\",\\"id\\":2,\\"result\\":{\\"tools\\":[{\\"name\\":\\"list_items\\"},{\\"name\\":\\"$ZONE_HOME\\"}]}}" ;;
              esac
            done
            """
        case "refuses":
            script = "#!/bin/sh\necho 'zone: the library at /x cannot be opened' >&2\nexit 1\n"
        case "silent":
            script = "#!/bin/sh\nsleep 5\n"
        default:
            script = "#!/bin/sh\nread line\necho '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\nsleep 2\n"
        }
        let file = folder.appendingPathComponent("zone")
        try script.write(to: file, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: file.path)
        return AgentLaunch(command: file.path, arguments: ["mcp"], environment: ["ZONE_HOME": "from-the-entry"])
    }

    func testAServerThatAnswersIsCounted() throws {
        let outcome = MCPProbe.run(try server("answers"), baseEnvironment: ["PATH": "/usr/bin:/bin", "ZONE_HOME": "from-the-app"], timeout: 5)
        // The entry's own environment wins: the check runs what the agent would run.
        XCTAssertEqual(outcome, .answers(.init(tools: ["list_items", "from-the-entry"], server: "zone", version: "0.1.2")))
        XCTAssertEqual(outcome.words, "Answers: 2 tools")
        XCTAssertTrue(outcome.answered)
    }

    func testFailuresSayWhy() throws {
        XCTAssertEqual(MCPProbe.run(AgentLaunch(command: "/nowhere/zone"), baseEnvironment: [:]), .failed("the program is not there (/nowhere/zone)"))
        XCTAssertEqual(MCPProbe.run(try server("refuses"), baseEnvironment: ["PATH": "/usr/bin:/bin"], timeout: 5), .failed("the library at /x cannot be opened"))
        XCTAssertEqual(MCPProbe.run(try server("silent"), baseEnvironment: ["PATH": "/usr/bin:/bin"], timeout: 1), .failed("nothing came back in 1 seconds"))
        XCTAssertEqual(MCPProbe.run(try server("not mcp"), baseEnvironment: ["PATH": "/usr/bin:/bin"], timeout: 3), .failed("it answered, but not as an MCP server"))
        XCTAssertEqual(MCPProbe.Outcome.failed("x").words, "No answer: x")
        XCTAssertEqual(MCPProbe.Outcome.answers(.init(tools: ["a"])).words, "Answers: 1 tool")
    }

    func testTheWordsSentAreMCP() throws {
        let hello = try XCTUnwrap(JSONSerialization.jsonObject(with: MCPProbe.initialize(clientVersion: "0.1.2")) as? [String: Any])
        XCTAssertEqual(hello["method"] as? String, "initialize")
        XCTAssertEqual(hello["id"] as? Int, 1)
        XCTAssertEqual((hello["params"] as? [String: Any])?["protocolVersion"] as? String, "2025-06-18")
        let note = String(decoding: MCPProbe.request(id: 0, method: "notifications/initialized"), as: UTF8.self)
        XCTAssertEqual(note, "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n", "a notification has no id, and each message is one line")
        if case let .failure(error)? = MCPProbe.response(id: 2, in: ["log line", #"{"id":1,"result":{}}"#, #"{"id":2,"error":{"message":"no"}}"#]) {
            XCTAssertEqual(error.message, "no")
        } else {
            XCTFail("the server's refusal was not read")
        }
        XCTAssertNil(MCPProbe.response(id: 3, in: [#"{"id":1,"result":{}}"#]))
    }
}
