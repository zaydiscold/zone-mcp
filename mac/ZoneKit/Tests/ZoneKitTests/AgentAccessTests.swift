import XCTest
@testable import ZoneKit

/// The Agents page's newer commands as zone prints them (shapes from the commits that added
/// `zone agents`, `zone prompts` and `zone mcp config`), the five choices of what agents can
/// read and the flags each one is, and the view most apps share.
final class AgentAccessTests: XCTestCase {
    func testAgentsStatus() throws {
        let on = try JSONDecoder().decode(AgentAccess.self, from: Data("""
        {"agents": "on", "enabled": true, "changed_at": null, "changed_local": null,
         "message": "AI agents with zone registered can use the library, each within the view it was registered with."}
        """.utf8))
        XCTAssertEqual(on.enabled, true)
        XCTAssertNil(on.changedAt)
        let off = try JSONDecoder().decode(AgentAccess.self, from: Data("""
        {"agents": "off", "enabled": false, "changed_at": 1795111570.5, "changed_local": "2026-11-19T10:06:10-08:00", "message": "AI agents cannot read the library"}
        """.utf8))
        XCTAssertEqual(off.enabled, false)
        XCTAssertEqual(off.changedAt, 1795111570.5)
        // Only the word: still read.
        XCTAssertFalse(try JSONDecoder().decode(AgentAccess.self, from: Data(#"{"agents": "off"}"#.utf8)).enabled)
    }

    func testPrompts() throws {
        let list = try JSONDecoder().decode(PromptList.self, from: Data("""
        {"prompts": [
          {"id": "notes-to-plan", "title": "Turn my voice notes into a plan", "description": "Turns your open voice notes into one checklist.",
           "kinds": ["note"], "writes": true, "arguments": [], "text": "Turn my open voice notes into a plan."},
          {"id": "find", "title": "Find where I talked about something", "description": "Searches.", "kinds": ["dictation", "note", "meeting"],
           "writes": false, "arguments": [{"name": "topic", "description": "What", "required": true, "default": null}], "text": "Find where I talked about [topic]."},
          {"id": "empty", "title": "Nothing to copy", "text": ""}
        ]}
        """.utf8))
        XCTAssertEqual(list.prompts.map(\.id), ["notes-to-plan", "find"])
        XCTAssertEqual(list.prompts[0].writes, true)
        XCTAssertEqual(list.prompts[1].text, "Find where I talked about [topic].")
    }

    func testOtherAppSnippet() throws {
        let config = try JSONDecoder().decode(McpConfig.self, from: Data("""
        {"name": "zone", "command": "/Users/zed/Library/Application Support/Zone/bin/zone", "args": ["mcp", "--kinds", "note,meeting"], "env": {},
         "generic": {"client": null, "label": "Any other app", "format": "json", "path": null, "path_shown": null,
                     "key_path": ["mcpServers", "zone"], "snippet": "{\\n  \\"mcpServers\\": {\\n    \\"zone\\": {}\\n  }\\n}", "command_line": null},
         "clients": []}
        """.utf8))
        // zone's own text, as it wrote it, ending in a newline.
        XCTAssertEqual(config.snippet, "{\n  \"mcpServers\": {\n    \"zone\": {}\n  }\n}\n")

        // Without zone's text, the same shape is made here.
        let made = McpConfig(command: "/Applications/Zone.app/zone", args: ["mcp", "--kinds", "note"], env: ["ZONE_HOME": "/tmp/z"])
        let parsed = try JSONSerialization.jsonObject(with: Data(made.snippet.utf8)) as? [String: [String: [String: Any]]]
        let entry = try XCTUnwrap(parsed?["mcpServers"]?["zone"])
        XCTAssertEqual(entry["command"] as? String, "/Applications/Zone.app/zone")
        XCTAssertEqual(entry["args"] as? [String], ["mcp", "--kinds", "note"])
        XCTAssertEqual(entry["env"] as? [String: String], ["ZONE_HOME": "/tmp/z"])
        XCTAssertFalse(made.snippet.contains("\\/"), "slashes are written plainly")
        XCTAssertNil(McpConfig(command: "zone", args: ["mcp"]).snippet.range(of: "env"), "no env when there is none")
    }

    func testStatusSaysWhatEachPlaceLaunches() throws {
        let survey = try JSONDecoder().decode(AgentSurvey.self, from: Data("""
        {"zone": "/z/bin/zone", "automatic": true, "clients": [
          {"client": "cursor", "label": "Cursor", "profile": null, "path": "/h/.cursor/mcp.json", "installed": true, "state": "registered",
           "detail": null, "read_only": false, "scope": {"limited": false, "kinds": ["dictation","note","meeting","clip"], "exclude_sources": []},
           "launch": {"command": "/z/bin/zone", "args": ["mcp"], "env": {}}},
          {"client": "zed", "label": "Zed", "profile": null, "path": "/h/.config/zed/settings.json", "installed": true, "state": "not_registered",
           "detail": null, "read_only": null, "scope": null, "launch": null}
        ]}
        """.utf8))
        XCTAssertEqual(survey.clients[0].launch, .init(command: "/z/bin/zone", args: ["mcp"]))
        XCTAssertNil(survey.clients[1].launch)
    }

    func testFiveChoicesAreExactlyTheseFlags() {
        func flags(_ choices: ReadChoices, clips: Bool = true) -> [String] { choices.view(knowsClips: clips).arguments }
        XCTAssertEqual(ReadChoices().view(knowsClips: true), .everything)
        XCTAssertEqual(ReadChoices().view(knowsClips: false), .everything)
        XCTAssertEqual(flags(ReadChoices(keyboard: false)), ["--kinds", "all", "--exclude-sources", "keyboard"])
        XCTAssertEqual(ReadChoices(keyboard: false).view(knowsClips: true), .noKeyboard)
        XCTAssertEqual(flags(ReadChoices(inZone: false)), ["--kinds", "all", "--exclude-sources", "app,note"])
        XCTAssertEqual(flags(ReadChoices(keyboard: false, inZone: false, clips: false)), ["--kinds", "note,meeting", "--exclude-sources", "none"])
        XCTAssertEqual(ReadChoices(keyboard: false, inZone: false, clips: false).view(knowsClips: true), .notesAndMeetings)
        XCTAssertEqual(flags(ReadChoices(clips: false)), ["--kinds", "dictation,note,meeting", "--exclude-sources", "none"])
        XCTAssertEqual(flags(ReadChoices(keyboard: false, meetings: false)), ["--kinds", "dictation,note,clip", "--exclude-sources", "keyboard"])
        XCTAssertEqual(flags(ReadChoices(keyboard: true, inZone: false, notes: false, meetings: false, clips: false)), ["--kinds", "dictation", "--exclude-sources", "app,note"])
        // A zone without clips: the clip choice says nothing, three kinds are everything.
        XCTAssertEqual(flags(ReadChoices(clips: false), clips: false), ["--kinds", "all", "--exclude-sources", "none"])
    }

    func testEveryViewTheChoicesCanSayReadsBack() {
        for keyboard in [true, false] { for inZone in [true, false] { for notes in [true, false] { for meetings in [true, false] { for clips in [true, false] {
            let choices = ReadChoices(keyboard: keyboard, inZone: inZone, notes: notes, meetings: meetings, clips: clips)
            guard !choices.isEmpty else { continue }
            let view = choices.view(knowsClips: true)
            XCTAssertEqual(ReadChoices(view, knowsClips: true), choices, "\(view)")
        } } } } }
        // Views set from the command line that the five cannot say are left alone.
        XCTAssertNil(ReadChoices(.custom(kinds: [], excludeSources: ["app"]), knowsClips: true))
        XCTAssertNil(ReadChoices(.custom(kinds: ["note"], excludeSources: ["keyboard"]), knowsClips: true))
        XCTAssertNil(ReadChoices(.custom(kinds: [], excludeSources: ["share"]), knowsClips: true))
        XCTAssertEqual(ReadChoices(.notesAndMeetings, knowsClips: false), ReadChoices(keyboard: false, inZone: false, clips: true))
    }

    func testTheLastChoiceCannotGo() {
        let one = ReadChoices(keyboard: false, inZone: false, notes: true, meetings: false, clips: false)
        XCTAssertTrue(one.isLast(\.notes, knowsClips: true))
        XCTAssertFalse(ReadChoices().isLast(\.notes, knowsClips: true))
        // Without clips, the hidden clip choice does not count as one left on.
        XCTAssertTrue(ReadChoices(keyboard: false, inZone: false, notes: true, meetings: false, clips: true).isLast(\.notes, knowsClips: false))
        XCTAssertFalse(one.isLast(\.meetings, knowsClips: true))
    }

    private func agent(_ id: String, _ scope: AgentSurvey.Scope?, profiles: Int = 1) -> Agent {
        let places = (0..<profiles).map { AgentSurvey.Place(client: id, label: id, profile: profiles > 1 ? "p\($0)" : nil, path: "/\(id)/\($0)", state: "registered", scope: scope) }
        return Agent.list(from: AgentSurvey(clients: places), knowsClips: true)[0]
    }

    func testTheSharedViewAndTheExceptions() {
        let notes = AgentSurvey.Scope(kinds: ["note", "meeting"], limited: true)
        let all = AgentSurvey.Scope(kinds: ["dictation", "note", "meeting", "clip"], limited: false)
        let claude = agent("claude-code", notes, profiles: 3)
        let cursor = agent("cursor", all)
        let codex = agent("codex", all)
        // Three profiles read notes and meetings, two read everything.
        XCTAssertEqual(SharedView.of([claude, cursor, codex]), .notesAndMeetings)
        XCTAssertEqual(SharedView.exceptions([claude, cursor, codex], shared: .notesAndMeetings).map(\.id), ["cursor", "codex"])
        XCTAssertEqual(SharedView.followers([claude, cursor, codex], shared: .notesAndMeetings).map(\.id), ["claude-code"])
        // A tie goes to the wider view; nothing connected is everything.
        XCTAssertEqual(SharedView.of([agent("zed", notes), cursor]), .everything)
        XCTAssertEqual(SharedView.of([]), .everything)
    }
}
