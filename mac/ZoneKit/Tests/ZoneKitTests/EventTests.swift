import XCTest
@testable import ZoneKit

final class EventTests: XCTestCase {
    // MARK: zone events --json

    func testLinkSessionLine() throws {
        let line = #"{"seq":5,"at":1794772842.9,"time":"2026-11-15T12:00:42-08:00","event":"link_session","device":"My iPhone","device_id":"7f7f","items":600,"new":598,"changed":2,"deleted":0,"unchanged":0,"rejected":0,"frames":4,"path":"relay then direct","duration_s":1.2,"paired":true,"end":"bye"}"#
        let event = try XCTUnwrap(EventLineDecoder.decode(line))
        XCTAssertEqual(event.id, "seq:5")
        XCTAssertEqual(event.seq, 5)
        XCTAssertEqual(event.kind, .linkSession)
        XCTAssertEqual(event.device, "My iPhone")
        XCTAssertEqual(event.items, 600)
        XCTAssertEqual(event.arrived, 600)
        XCTAssertEqual(event.path, "relay then direct")
        XCTAssertTrue(event.alsoPaired)
        XCTAssertEqual(event.date.timeIntervalSince1970, 1794772842.9, accuracy: 0.01)
        XCTAssertTrue(event.broughtItems)
        XCTAssertEqual(ActivityWords.headline(event), "600 items from My iPhone")
        XCTAssertEqual(ActivityWords.route(event.path), "relay, then direct")
    }

    func testEveryKindOfLineIsUnderstood() throws {
        let lines = [
            #"{"seq":1,"at":1794772800.12,"time":"2026-11-15T12:00:00-08:00","event":"daemon_started","version":"0.1.0","pid":4242,"port":47813}"#,
            #"{"seq":2,"at":1794772802.4,"time":"2026-11-15T12:00:02-08:00","event":"relay_connected","relay_url":"https://relay.example.com/"}"#,
            #"{"seq":3,"at":1794772830.0,"time":"2026-11-15T12:00:30-08:00","event":"pair_offered","offer":"9c1f0a2b3d4e5f60","expires_at":1794773430.0,"via":"cli"}"#,
            #"{"seq":4,"at":1794772841.7,"time":"2026-11-15T12:00:41-08:00","event":"paired","device":"My iPhone","device_id":"7f7f","via":"link","offer":"9c1f0a2b3d4e5f60"}"#,
            #"{"seq":6,"at":1794773000.0,"time":"2026-11-15T12:03:20-08:00","event":"flash_received","device":"My iPhone","device_id":"7f7f","items":40,"new":3,"changed":1,"deleted":0,"unchanged":36,"rejected":0,"paired":false,"bytes":18432,"color":true}"#,
            #"{"seq":7,"at":1794773100.0,"time":"2026-11-15T12:05:00-08:00","event":"backup_import","trigger":"watch","items":43,"new":2,"changed":0,"unchanged":41,"notes":2,"dictations":40,"meetings":1,"skipped":0}"#,
            #"{"seq":8,"at":1794773200.0,"time":"2026-11-15T12:06:40-08:00","event":"relay_lost"}"#,
            #"{"seq":9,"at":1794773300.0,"time":"2026-11-15T12:08:20-08:00","event":"daemon_stopped","uptime_s":500.0}"#,
            #"{"seq":10,"at":1794773400.0,"time":"2026-11-15T12:10:00-08:00","event":"link_refused","device":null,"device_id":"aa","reason":"not_paired"}"#,
            #"{"seq":11,"at":1794773500.0,"time":"2026-11-15T12:11:40-08:00","event":"resync_requested","device":null,"phones":2}"#,
            #"{"seq":12,"at":1794773600.0,"time":"2026-11-15T12:13:20-08:00","event":"unpaired","device":"My iPhone","device_id":"7f7f"}"#,
            #"{"seq":13,"at":1794773700.0,"time":"2026-11-15T12:15:00-08:00","event":"flash_failed","reason":"The phone is not paired.","bytes":900}"#,
        ]
        let events = lines.compactMap(EventLineDecoder.decode)
        XCTAssertEqual(events.count, lines.count)
        XCTAssertEqual(events.map(\.kind), [
            .daemonStarted, .relayConnected, .pairOffered, .paired, .flashReceived, .backupImport,
            .relayLost, .daemonStopped, .linkRefused, .resyncRequested, .unpaired, .flashFailed,
        ])
        XCTAssertEqual(events.map(ActivityWords.headline), [
            "Zone started",
            "Reachable from other networks",
            "Pair code shown",
            "My iPhone paired",
            "4 items from My iPhone by Flash",
            "2 items from the backup folder",
            "Lost the relay connection",
            "Zone stopped",
            "Turned away a phone that is not paired",
            "Asked every phone to send everything again",
            "My iPhone forgotten",
            "A flash could not be stored",
        ])
        XCTAssertFalse(events[2].isNews, "showing a pair code is not news")
        XCTAssertTrue(events[3].isNews)
        XCTAssertEqual(events.filter(\.isNews).map(\.kind), [
            .paired, .flashReceived, .backupImport, .linkRefused, .resyncRequested, .unpaired, .flashFailed,
        ], "the service and the relay coming and going are state, not news")
        XCTAssertEqual(events[12 - 1].reason, "The phone is not paired.")
    }

    func testAFirstBackupReadNamesItsKinds() throws {
        let line = #"{"seq":7,"at":1794773100.0,"event":"backup_import","trigger":"import","items":24,"new":24,"changed":0,"unchanged":0,"notes":11,"dictations":12,"meetings":1,"skipped":0}"#
        let event = try XCTUnwrap(EventLineDecoder.decode(line))
        XCTAssertEqual(event.kinds, ["note": 11, "dictation": 12, "meeting": 1])
        XCTAssertEqual(ActivityWords.headline(event), "12 dictations, 11 notes and 1 meeting from the backup folder")
    }

    func testAnEventFromALaterZoneIsKeptButNotShown() throws {
        let event = try XCTUnwrap(EventLineDecoder.decode(#"{"seq":40,"at":1794773100.0,"event":"library_compacted","freed":12}"#))
        XCTAssertEqual(event.kind, .unknown)
        XCTAssertEqual(event.name, "library_compacted")
        XCTAssertFalse(event.isNews)
        XCTAssertEqual(ActivityWords.headline(event), "library compacted")
    }

    func testLinesThatAreNotEventsAreSkipped() {
        XCTAssertNil(EventLineDecoder.decode(""))
        XCTAssertNil(EventLineDecoder.decode("not json"))
        XCTAssertNil(EventLineDecoder.decode(#"{"seq": 1}"#))
        XCTAssertNil(EventLineDecoder.decode("[1, 2]"))
    }

    func testAnEmptySessionIsNotNews() throws {
        let event = try XCTUnwrap(EventLineDecoder.decode(#"{"seq":14,"at":1794773800.0,"event":"link_session","device":"My iPhone","items":0,"new":0,"changed":0,"path":"direct"}"#))
        XCTAssertFalse(event.isNews)
        XCTAssertFalse(event.broughtItems)
        XCTAssertEqual(ActivityWords.headline(event), "My iPhone checked in, nothing new")
    }

    // MARK: zone.log

    func testLogLines() throws {
        let log = """
        2026-11-15T11:45:27-08:00 info  zone 0.1.0 started for /tmp/zone-mac-dev/probe/zh (library 22862E2F-4B07-4063-827F-FFE137697199)
        2026-11-15T11:45:27-08:00 info  link: endpoint id 13035f4f905e9996b91f0107f0ec6767d75b188b44b09ec247e56a8f619e9af7
        2026-11-15T11:45:27-08:00 info  web: http://127.0.0.1:47991/ (open it with `zone open`)
        2026-11-15T11:45:31-08:00 info  link: reachable through the relay https://usw1-1.relay.n0.iroh.link./
        2026-11-15T11:45:38-08:00 info  link: paired Demo iPhone (63532d484a)
        2026-11-15T11:45:38-08:00 info  link: Demo iPhone (63532d484a) sent 40 items in 1 frames, 40 new or changed, path: relay then direct
        2026-11-15T11:46:02-08:00 info  link: refused 0a1b2c3d4e: not paired
        2026-11-15T11:47:10-08:00 info  flash: Demo iPhone sent 12 items, 3 new or changed, and is now paired
        2026-11-15T11:47:40-08:00 warn  flash: a complete flash of 900 bytes was not stored: the phone is not paired
        2026-11-15T11:48:00-08:00 info  backup folder: read 11 notes, 12 dictations, 1 meetings: 24 new, 0 changed, 0 already here
        2026-11-15T11:49:00-08:00 warn  link: no relay connection. Phones on other networks cannot reach this computer until it returns.
        2026-11-15T11:50:00-08:00 info  zone is stopping
        """
        let events = log.split(separator: "\n").compactMap { LogLineParser.parse(String($0)) }
        XCTAssertEqual(events.map(\.kind), [
            .daemonStarted, .relayConnected, .paired, .linkSession, .linkRefused, .flashReceived,
            .flashFailed, .backupImport, .relayLost, .daemonStopped,
        ])
        let session = events[3]
        XCTAssertEqual(session.device, "Demo iPhone")
        XCTAssertEqual(session.items, 40)
        XCTAssertEqual(session.arrived, 40)
        XCTAssertEqual(session.path, "relay then direct")
        XCTAssertNil(session.seq)
        let expected = try XCTUnwrap(LogLineParser.date("2026-11-15T11:45:38-08:00"))
        XCTAssertEqual(session.date, expected)

        XCTAssertEqual(events[2].device, "Demo iPhone")
        XCTAssertEqual(events[4].reason, "not paired")
        XCTAssertEqual(ActivityWords.headline(events[4]), "Turned away a phone that is not paired")
        XCTAssertEqual(events[5].arrived, 3)
        XCTAssertTrue(events[5].alsoPaired)
        XCTAssertEqual(events[6].reason, "the phone is not paired")
        XCTAssertEqual(events[7].arrived, 24)
        XCTAssertEqual(events[7].kinds, ["note": 11, "dictation": 12, "meeting": 1])
    }

    func testAPhoneNameWithBracketsInTheLog() throws {
        let event = try XCTUnwrap(LogLineParser.parse("2026-11-15T11:45:38-08:00 info  link: Sam's iPhone (work) (63532d484a) sent 3 items in 1 frames, 2 new or changed, path: direct"))
        XCTAssertEqual(event.device, "Sam's iPhone (work)")
        XCTAssertEqual(event.arrived, 2)
    }

    func testTheSameLogLineHasTheSameID() throws {
        let line = "2026-11-15T11:45:38-08:00 info  link: paired Demo iPhone (63532d484a)"
        XCTAssertEqual(LogLineParser.parse(line)?.id, LogLineParser.parse(line)?.id)
        XCTAssertNil(LogLineParser.parse("garbage"))
        XCTAssertNil(LogLineParser.parse("2026-11-15T11:45:38-08:00 info  mcp: registered with Cursor"))
    }

    // MARK: Words

    func testCounts() {
        XCTAssertEqual(ActivityWords.count(of: ["dictation": 3], total: 3), "3 dictations")
        XCTAssertEqual(ActivityWords.count(of: ["dictation": 1], total: 1), "1 dictation")
        XCTAssertEqual(ActivityWords.count(of: ["dictation": 2, "note": 1], total: 3), "2 dictations and 1 note")
        XCTAssertEqual(ActivityWords.count(of: [:], total: 1764), "1,764 items")
        XCTAssertEqual(ActivityWords.count(of: [:], total: 1), "1 item")
        XCTAssertEqual(ActivityWords.count(of: ["dictation": 2], total: 40), "40 items", "kinds that do not add up are not trusted")
    }

    func testKindsOfWhatJustArrived() {
        let moment = Date(timeIntervalSince1970: 1_000_000)
        let items = [
            ZoneItem(id: "a", kind: "dictation", createdAt: 10, text: "", receivedAt: 1_000_001),
            ZoneItem(id: "b", kind: "dictation", createdAt: 11, text: "", receivedAt: 1_000_000.5),
            ZoneItem(id: "c", kind: "note", createdAt: 12, text: "", receivedAt: 999_998),
            ZoneItem(id: "d", kind: "meeting", createdAt: 13, text: "", receivedAt: 900_000),
            ZoneItem(id: "e", kind: "note", createdAt: 14, text: ""),
        ]
        XCTAssertEqual(ActivityWords.kinds(of: items, receivedSince: moment), ["dictation": 2, "note": 1])
    }

    // MARK: The feed

    func testFeedKeepsNewestFirstWithoutRepeats() {
        var feed = ActivityFeed(limit: 3)
        let at = { (seconds: Double) in Date(timeIntervalSince1970: seconds) }
        XCTAssertTrue(feed.add(ActivityEvent(id: "seq:1", seq: 1, date: at(10), kind: .daemonStarted)))
        XCTAssertTrue(feed.add(ActivityEvent(id: "seq:3", seq: 3, date: at(30), kind: .paired, device: "A")))
        XCTAssertTrue(feed.add(ActivityEvent(id: "seq:2", seq: 2, date: at(20), kind: .pairOffered)))
        XCTAssertFalse(feed.add(ActivityEvent(id: "seq:3", seq: 3, date: at(30), kind: .paired, device: "A")), "a repeat is dropped")
        XCTAssertEqual(feed.events.map(\.id), ["seq:3", "seq:2", "seq:1"])
        XCTAssertEqual(feed.news.map(\.id), ["seq:3"])
        XCTAssertEqual(feed.lastSeq, 3)

        feed.add(ActivityEvent(id: "seq:4", seq: 4, date: at(40), kind: .linkSession, device: "A", items: 2, arrived: 2))
        XCTAssertEqual(feed.events.map(\.id), ["seq:4", "seq:3", "seq:2"], "the oldest falls off")

        feed.update(id: "seq:4") { $0.kinds = ["dictation": 2] }
        XCTAssertEqual(ActivityWords.headline(feed.events[0]), "2 dictations from A")
    }

    // MARK: Following

    func testFollowerArguments() {
        XCTAssertEqual(EventsCommandSource.arguments(history: 50, after: nil), ["events", "--json", "--follow", "--limit", "50"])
        XCTAssertEqual(EventsCommandSource.arguments(history: 50, after: 12), ["events", "--json", "--follow", "--after", "12"])
    }

    func testLineBufferHoldsAnUnfinishedLine() {
        var buffer = LineBuffer()
        XCTAssertEqual(buffer.add(Data("{\"a\":1}\n{\"b\"".utf8)), ["{\"a\":1}"])
        XCTAssertEqual(buffer.add(Data(":2}\n\n".utf8)), ["{\"b\":2}"])
        XCTAssertEqual(buffer.add(Data()), [])
    }

    func testLogTailReadsOnlyWhatIsNew() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("zonekit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("zone.log")
        var tail = LogTail()

        XCTAssertEqual(tail.newLines(in: file), [], "no file yet")

        func append(_ text: String) throws {
            if !FileManager.default.fileExists(atPath: file.path) {
                FileManager.default.createFile(atPath: file.path, contents: nil)
            }
            let handle = try FileHandle(forWritingTo: file)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: Data(text.utf8))
        }

        try append("one\ntwo\nthr")
        XCTAssertEqual(tail.newLines(in: file), ["one", "two"])
        XCTAssertEqual(tail.newLines(in: file), [])
        try append("ee\nfour\n")
        XCTAssertEqual(tail.newLines(in: file), ["three", "four"])

        // The log was rotated: a new, shorter file under the same name.
        try Data("fresh\n".utf8).write(to: file)
        XCTAssertEqual(tail.newLines(in: file), ["fresh"])
    }

    func testLogTailStartsNearTheEndOfALongFile() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("zonekit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("zone.log")
        let lines = (0..<200).map { "line number \($0)" }
        try Data((lines.joined(separator: "\n") + "\n").utf8).write(to: file)

        var tail = LogTail()
        let read = tail.newLines(in: file, history: 100)
        XCTAssertFalse(read.isEmpty)
        XCTAssertEqual(read.last, "line number 199")
        XCTAssertTrue(read.allSatisfy { $0.hasPrefix("line number ") }, "a line cut in half is dropped: \(read)")
        XCTAssertLessThan(read.count, 10)
    }

    func testLogTailSourceDeliversEvents() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("zonekit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("zone.log")
        try Data("2026-11-15T11:45:38-08:00 info  link: paired Demo iPhone (63532d484a)\n".utf8).write(to: file)

        let source = LogTailSource(file: file, interval: 0.05)
        let got = expectation(description: "the event arrives")
        source.start { event in
            if event.kind == .paired, event.device == "Demo iPhone" { got.fulfill() }
        }
        wait(for: [got], timeout: 5)
        source.stop()
    }
}
