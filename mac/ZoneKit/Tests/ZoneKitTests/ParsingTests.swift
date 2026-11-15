import XCTest
@testable import ZoneKit

/// What `zone` prints, as it really printed it (from a library of made-up items), and
/// what the app reads out of it.
final class ParsingTests: XCTestCase {
    static let statusRunning = """
    {
      "now": {"local": "Sun 2026-11-15 11:45:31 -08:00", "date": "2026-11-15", "weekday": "Sunday",
              "timezone": "America/Los_Angeles", "utc_offset": "-08:00", "rfc3339": "2026-11-15T11:45:31-08:00"},
      "computer": {"name": "Demo Mac", "os": "macos", "zone_version": "0.1.0"},
      "daemon_running": true,
      "phones": [
        {"name": "Demo iPhone", "app": "fake 0.1.0",
         "endpoint_id": "63532d484a3a44d64c001e7575dd9fc495b66d74328c0b00c563fb123d6b4898",
         "paired": "Sun 2026-11-15 11:45:38 -08:00", "last_sync": "Sun 2026-11-15 11:45:38 -08:00",
         "last_sync_age_s": 1.0, "items_received": 40, "last_path": "direct"}
      ],
      "last_sync": {"link": {"time": "Sun 2026-11-15 11:45:38 -08:00", "age_s": 1.0}, "flash": null,
                    "backup_folder": {"time": "Sun 2026-11-15 11:45:27 -08:00", "age_s": 12.0}},
      "backup_folder": null,
      "backup_folder_state": null,
      "items": {"total": 64, "by_kind": {"dictation": 52, "note": 11, "meeting": 1},
                "dictations_by_source": {"keyboard": 52}, "unprocessed": 64, "deleted_on_phone": 0,
                "oldest": "Mon 2026-11-09 08:12:00 -08:00", "newest": "Sun 2026-11-15 09:12:00 -08:00"},
      "data_directory": "/tmp/zone-mac-dev/probe/zh",
      "database": "/tmp/zone-mac-dev/probe/zh/zone.db",
      "daemon": {"running": true, "pid": 15989,
                 "endpoint_id": "13035f4f905e9996b91f0107f0ec6767d75b188b44b09ec247e56a8f619e9af7",
                 "web_page": "http://127.0.0.1:47991/", "relay_connected": true,
                 "relay_url": "https://usw1-1.relay.n0.iroh.link./"}
    }
    """

    static let statusStopped = """
    {"computer": {"name": "Demo Mac", "os": "macos", "zone_version": "0.1.0"}, "daemon_running": false,
     "phones": [], "last_sync": {"link": null, "flash": null, "backup_folder": null},
     "backup_folder": "/Users/zed/Zone Backups",
     "backup_folder_state": {"state": "not_watching"},
     "items": {"total": 0, "by_kind": {}, "dictations_by_source": {}, "unprocessed": 0, "deleted_on_phone": 0,
               "oldest": null, "newest": null},
     "data_directory": "/tmp/zh", "database": "/tmp/zh/zone.db",
     "daemon": {"running": false, "endpoint_id": null}}
    """

    func decode<T: Decodable>(_ type: T.Type, _ text: String) throws -> T {
        try JSONDecoder().decode(T.self, from: Data(text.utf8))
    }

    func testStatusOfARunningZone() throws {
        let status = try decode(StatusReport.self, Self.statusRunning)
        XCTAssertTrue(status.daemon.running)
        XCTAssertEqual(status.daemon.pid, 15989)
        XCTAssertEqual(status.daemon.relayConnected, true)
        XCTAssertEqual(status.daemon.webPage, "http://127.0.0.1:47991/")
        XCTAssertEqual(status.computer?.name, "Demo Mac")
        XCTAssertEqual(status.items.total, 64)
        XCTAssertEqual(status.items.byKind["note"], 11)
        XCTAssertEqual(status.phones.count, 1)
        XCTAssertEqual(status.phones[0].name, "Demo iPhone")
        XCTAssertEqual(status.phones[0].itemsReceived, 40)
        XCTAssertEqual(status.phones[0].lastSyncAge, 1.0)
        XCTAssertEqual(status.phones[0].lastPath, "direct")
        XCTAssertEqual(status.lastSync.backupFolder?.age, 12.0)
        XCTAssertNil(status.lastSync.flash)
        XCTAssertNil(status.backupFolder)
    }

    func testStatusOfAStoppedZone() throws {
        let status = try decode(StatusReport.self, Self.statusStopped)
        XCTAssertFalse(status.daemon.running)
        XCTAssertNil(status.daemon.pid)
        XCTAssertTrue(status.phones.isEmpty)
        XCTAssertEqual(status.backupFolder, "/Users/zed/Zone Backups")
        XCTAssertEqual(status.backupState?.state, "not_watching")
    }

    func testStatusWithFieldsMissingStillReads() throws {
        // A zone that says less (or a later one that renamed something) must not break the app.
        let status = try decode(StatusReport.self, #"{"daemon": {"running": true}, "phones": [{"name": "A"}]}"#)
        XCTAssertTrue(status.daemon.running)
        XCTAssertEqual(status.items.total, 0)
        XCTAssertEqual(status.phones[0].itemsReceived, 0)
        XCTAssertNil(status.phones[0].lastSyncAge)
    }

    func testDevices() throws {
        let devices = try decode([Device].self, """
        [{"name": "Demo iPhone", "phone_id": "85C44583-BE92-4E9E-8ED4-7964BB24A842",
          "endpoint_id": "63532d484a3a44d64c001e7575dd9fc495b66d74328c0b00c563fb123d6b4898", "app": "fake 0.1.0",
          "paired_at": 1794771938.415464, "last_seen": 1794771938.475668, "last_sync": 1794771938.468761,
          "items_received": 40, "last_path": "direct"}]
        """)
        XCTAssertEqual(devices.count, 1)
        XCTAssertEqual(devices[0].name, "Demo iPhone")
        XCTAssertEqual(devices[0].pairedAt ?? 0, 1794771938.415464, accuracy: 0.001)
        XCTAssertEqual(devices[0].itemsReceived, 40)
        XCTAssertEqual(try decode([Device].self, "[]"), [])
    }

    func testPairCode() throws {
        let code = try decode(PairCode.self, """
        {"url": "zone://pair?v=1&n=13035f4f&s=7ec780d5&name=Demo%20Mac", "endpoint_id": "13035f4f",
         "name": "Demo Mac", "expires_at": 1794772531.643459, "expires_local": "2026-11-15T11:55:31-08:00"}
        """)
        XCTAssertTrue(code.url.hasPrefix("zone://pair?"))
        XCTAssertEqual(code.expiry.timeIntervalSince1970, 1794772531.643459, accuracy: 0.001)
    }

    func testItemsAndTheirPreviews() throws {
        let list = try decode(ItemList.self, """
        {"total": 64, "items": [
          {"id": "4CDD2055-930D-4EAF-94F4-733F3E7D1BFB", "kind": "dictation", "source": "keyboard",
           "created_at": 1794762720.0, "updated_at": 1794762720.0, "created_local": "2026-11-15T09:12:00-08:00",
           "text": "Morning! Coffee at the usual place?", "title": null, "duration_s": 2.5, "words": 6,
           "engine": "parakeet", "status": "inserted", "extra": {}, "rev": null, "processed_at": null,
           "carrier": "backup", "device_id": null, "received_at": 1794771927.59641},
          {"id": "M1", "kind": "meeting", "source": null, "created_at": 1794420000.0,
           "text": "Me: Let's start.\\nJordan: Yes.", "title": "Launch planning", "words": 4,
           "carrier": "link", "received_at": 1794771940.0}
        ]}
        """)
        XCTAssertEqual(list.total, 64)
        XCTAssertEqual(list.items[0].preview, "Morning! Coffee at the usual place?")
        XCTAssertEqual(list.items[0].words, 6)
        XCTAssertEqual(list.items[1].preview, "Launch planning: Me: Let's start. Jordan: Yes.")
        XCTAssertEqual(list.items[1].copyText, "Me: Let's start.\nJordan: Yes.")
    }

    func testTodayTotal() throws {
        let stats = try decode(StatsReport.self, #"{"total": {"items": 3, "words": 25, "recorded_s": 9.0}, "groups": []}"#)
        XCTAssertEqual(stats.total.items, 3)
        XCTAssertEqual(stats.total.words, 25)
    }

    // MARK: AI clients

    static let survey = """
    {"zone": "/Users/zed/Library/Application Support/Zone/bin/zone", "automatic": false, "clients": [
      {"client": "claude-code", "label": "Claude Code (default)", "profile": "default", "path": "/Users/zed/.claude.json",
       "installed": true, "state": "registered", "detail": null},
      {"client": "claude-code", "label": "Claude Code (work)", "profile": "work", "path": "/Users/zed/.claude-work/.claude.json",
       "installed": true, "state": "not_registered", "detail": null},
      {"client": "claude-desktop", "label": "Claude Desktop", "profile": null, "path": "/x", "installed": true,
       "state": "registered_read_only", "detail": null},
      {"client": "codex", "label": "Codex", "profile": null, "path": "/x", "installed": true,
       "state": "other_copy", "detail": "/Users/zed/.local/bin/zone"},
      {"client": "cursor", "label": "Cursor", "profile": null, "path": "/x", "installed": true,
       "state": "removed_by_you", "detail": null},
      {"client": "zed", "label": "Zed", "profile": null, "path": "/x", "installed": false,
       "state": "not_registered", "detail": null},
      {"client": "goose", "label": "Goose", "profile": null, "path": "/x", "installed": true,
       "state": "unreadable", "detail": "not valid YAML"}
    ]}
    """

    func testAgentsAreGroupedByClient() throws {
        let agents = Agent.list(from: try decode(AgentSurvey.self, Self.survey))
        XCTAssertEqual(agents.map(\.id), ["claude-code", "claude-desktop", "codex", "cursor", "goose"], "a client that is not on this Mac is left out")

        XCTAssertEqual(agents[0].title, "Claude Code")
        XCTAssertEqual(agents[0].standing, .partly(places: 1, total: 2))
        XCTAssertTrue(agents[0].isOn)
        XCTAssertEqual(agents[0].note, "Connected in 1 of 2 profiles")

        XCTAssertEqual(agents[1].standing, .connected)
        XCTAssertEqual(agents[1].note, "Connected")

        XCTAssertEqual(agents[2].standing, .otherCopy("/Users/zed/.local/bin/zone"))
        XCTAssertTrue(agents[2].isOn)

        XCTAssertEqual(agents[3].standing, .notConnected)
        XCTAssertFalse(agents[3].isOn)

        XCTAssertEqual(agents[4].standing, .unreadable("not valid YAML"))
        XCTAssertFalse(agents[4].isOn)
    }

    func testAClientWithZoneIsShownEvenWhenItLooksUninstalled() throws {
        let survey = AgentSurvey(clients: [
            .init(client: "kiro", label: "Kiro", installed: false, state: "registered"),
            .init(client: "amp", label: "Amp", installed: false, state: "not_registered"),
        ])
        XCTAssertEqual(Agent.list(from: survey).map(\.id), ["kiro"])
    }

    func testAllProfilesConnected() {
        let survey = AgentSurvey(clients: [
            .init(client: "hermes", label: "Hermes (default)", profile: "default", state: "registered"),
            .init(client: "hermes", label: "Hermes (research)", profile: "research", state: "registered"),
        ])
        let agent = Agent.list(from: survey)[0]
        XCTAssertEqual(agent.standing, .connected)
        XCTAssertEqual(agent.title, "Hermes")
        XCTAssertEqual(agent.note, "Connected in all 2 profiles")
    }

    // MARK: Running zone

    struct FakeRunner: ZoneRunning {
        var answers: [String: CommandOutput]
        func run(_ arguments: [String], timeout: TimeInterval) async throws -> CommandOutput {
            answers[arguments.joined(separator: " ")] ?? CommandOutput(status: 2, stderr: Data("error: unrecognized subcommand".utf8))
        }
    }

    func testClientReadsStatusAndVersion() async throws {
        let client = ZoneClient(runner: FakeRunner(answers: [
            "status --json": CommandOutput(text: Self.statusRunning),
            "--version": CommandOutput(text: "zone 0.1.0\n"),
            "open --print": CommandOutput(text: "http://127.0.0.1:47991/?t=abc\n"),
            "resync": CommandOutput(text: "At its next sync, the phone will send its whole library again.\n"),
        ]))
        let status = try await client.status()
        XCTAssertEqual(status.items.total, 64)
        let version = try await client.version()
        XCTAssertEqual(version, "0.1.0")
        let link = try await client.historyLink()
        XCTAssertEqual(link.port, 47991)
        let said = try await client.resync()
        XCTAssertEqual(said, "At its next sync, the phone will send its whole library again.")
        let hasEvents = await client.hasEvents()
        XCTAssertFalse(hasEvents, "this zone has no events command")
    }

    func testClientPassesOnWhatZoneSaidWentWrong() async {
        let client = ZoneClient(runner: FakeRunner(answers: [
            "unpair Nobody": CommandOutput(status: 1, stderr: Data("zone: no paired phone is called Nobody\n".utf8)),
            "status --json": CommandOutput(text: "not json"),
        ]))
        do {
            try await client.unpair("Nobody")
            XCTFail("unpair should have failed")
        } catch {
            XCTAssertEqual(error as? ZoneError, .failed("no paired phone is called Nobody"))
        }
        do {
            _ = try await client.status()
            XCTFail("status should have been unreadable")
        } catch {
            XCTAssertEqual(error as? ZoneError, .unreadable("status --json"))
        }
    }

    func testARealProgramIsRunWithOnlyTheGivenEnvironment() async throws {
        let runner = ZoneProcessRunner(binary: URL(fileURLWithPath: "/usr/bin/env"), environment: ["ZONE_HOME": "/tmp/somewhere", "HOME": "/tmp/home"])
        let output = try await runner.run([], timeout: 10)
        let lines = Set(output.text.split(separator: "\n").map(String.init))
        XCTAssertTrue(lines.contains("ZONE_HOME=/tmp/somewhere"))
        XCTAssertTrue(lines.contains("HOME=/tmp/home"))
        XCTAssertFalse(output.text.contains("CLAUDE_CONFIG_DIR"), "nothing leaks in from the app's own environment")
    }

    func testAMissingProgramIsReported() async {
        let runner = ZoneProcessRunner(binary: URL(fileURLWithPath: "/nonexistent/zone"), environment: [:])
        do {
            _ = try await runner.run(["status"], timeout: 5)
            XCTFail("should not run")
        } catch {
            XCTAssertEqual(error as? ZoneError, .missing("/nonexistent/zone"))
        }
    }

    func testASlowProgramIsStopped() async {
        let runner = ZoneProcessRunner(binary: URL(fileURLWithPath: "/bin/sleep"), environment: [:])
        do {
            _ = try await runner.run(["30"], timeout: 0.3)
            XCTFail("should have timed out")
        } catch {
            XCTAssertEqual(error as? ZoneError, .timedOut("30"))
        }
    }
}
