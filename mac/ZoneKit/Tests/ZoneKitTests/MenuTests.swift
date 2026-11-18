import XCTest
@testable import ZoneKit

/// What the window under the Z shows: the one line about syncing, the list and its kinds,
/// the links into the history page.
final class MenuTests: XCTestCase {
    private func item(_ id: String, _ kind: String, title: String? = nil, text: String = "words", processed: Double? = nil) -> ZoneItem {
        ZoneItem(id: id, kind: kind, createdAt: 1_794_000_000, text: text, title: title, processedAt: processed)
    }

    func testAVoiceNoteIsListedAndCountedOnce() {
        // A voice note is two items in the library: the recording (a dictation whose source
        // is "note") and the note saved when it ended. The page shows the note alone.
        let recording = ZoneItem(id: "r1", kind: "dictation", createdAt: 1000, text: "pick up the saffron", duration: 20, source: "note")
        let note = ZoneItem(id: "n1", kind: "note", createdAt: 1021, text: "pick up the saffron")
        let alone = ZoneItem(id: "r2", kind: "dictation", createdAt: 5000, text: "never saved as a note", duration: 20, source: "note")
        let typed = ZoneItem(id: "k1", kind: "dictation", createdAt: 1010, text: "typed by voice", source: "keyboard")
        let recent = [alone, note, typed, recording]
        XCTAssertTrue(MenuContent.isRecording(recording, behindOneOf: [note]))
        XCTAssertFalse(MenuContent.isRecording(alone, behindOneOf: [note]))
        XCTAssertFalse(MenuContent.isRecording(typed, behindOneOf: [note]))
        XCTAssertFalse(MenuContent.isRecording(recording, behindOneOf: [ZoneItem(id: "n2", kind: "note", createdAt: 1141, text: "saved too late")]))
        XCTAssertEqual(MenuContent.rows(segment: .all, recent: recent, ofKind: nil, shownAbove: []).map(\.id), ["r2", "n1", "k1"])
        XCTAssertEqual(MenuContent.rows(segment: .dictation, recent: recent, ofKind: nil, shownAbove: []).map(\.id), ["r2", "k1"])
        // A list of one kind read by itself is folded against the notes the app knows.
        XCTAssertEqual(MenuContent.rows(segment: .dictation, recent: [], ofKind: [alone, typed, recording], shownAbove: [], notes: [note]).map(\.id), ["r2", "k1"])
        XCTAssertEqual(MenuContent.rows(segment: .dictation, recent: [], ofKind: [alone, typed, recording], shownAbove: []).map(\.id), ["r2", "k1", "r1"], "without its note, a recording stands by itself")
        // And the numbers beside the kinds leave them out, as the page's do.
        let items = StatusReport.Items(total: 12, byKind: ["dictation": 8, "note": 3, "meeting": 1], recordingsBehindNotes: 2)
        XCTAssertEqual(ListSegment.all.count(in: items), 10)
        XCTAssertEqual(ListSegment.dictation.count(in: items), 6)
        XCTAssertEqual(ListSegment.note.count(in: items), 3)
        XCTAssertEqual(ListSegment.all.count(in: StatusReport.Items(total: 12, byKind: ["dictation": 8])), 12, "an older zone does not say, and nothing is taken off")
    }

    func testAllLeavesOutWhatIsShownAboveTheList() {
        let recent = [item("clip", "clip"), item("d1", "dictation"), item("n1", "note"), item("m1", "meeting"), item("d2", "dictation")]
        let rows = MenuContent.rows(segment: .all, recent: recent, ofKind: nil, shownAbove: ["clip", "n1", "m1"])
        XCTAssertEqual(rows.map(\.id), ["d1", "d2"])
    }

    func testAKindIsListedWhole() {
        let recent = [item("d1", "dictation"), item("n1", "note")]
        // Before that kind was read on its own, what the latest items hold of it.
        XCTAssertEqual(MenuContent.rows(segment: .note, recent: recent, ofKind: nil, shownAbove: ["n1"]).map(\.id), ["n1"])
        // Once read, the kind's own list, even what also has a place above.
        let notes = [item("n1", "note"), item("n0", "note")]
        XCTAssertEqual(MenuContent.rows(segment: .note, recent: recent, ofKind: notes, shownAbove: ["n1"]).map(\.id), ["n1", "n0"])
        XCTAssertEqual(MenuContent.rows(segment: .dictation, recent: recent, ofKind: nil, shownAbove: [], limit: 0).count, 0)
    }

    func testSegmentsAndTheirCounts() {
        let items = StatusReport.Items(total: 207, byKind: ["dictation": 162, "note": 43, "meeting": 1, "clip": 1])
        XCTAssertEqual(ListSegment.shown(hasClips: true).map(\.title), ["All", "Notes", "Meetings", "Dictations", "Clips"])
        XCTAssertEqual(ListSegment.shown(hasClips: false).map(\.rawValue), ["all", "note", "meeting", "dictation"])
        XCTAssertEqual(ListSegment.all.count(in: items), 207)
        XCTAssertEqual(ListSegment.note.count(in: items), 43)
        XCTAssertEqual(ListSegment.clip.count(in: StatusReport.Items(total: 3)), 0)
        XCTAssertNil(ListSegment.all.kind)
        XCTAssertEqual(ListSegment.meeting.kind, "meeting")
    }

    func testTheOpenNotesLabelSaysWhenMoreAreOpen() {
        XCTAssertEqual(MenuContent.openNotesLabel(shown: 3, open: 3), "Open notes")
        XCTAssertEqual(MenuContent.openNotesLabel(shown: 2, open: 2), "Open notes")
        XCTAssertEqual(MenuContent.openNotesLabel(shown: 3, open: 1240), "Open notes, newest of 1,240")
    }

    func testAMeetingsFactsAndLength() throws {
        // As `zone list --kind meeting --json` prints one: no duration, the lines in "extra".
        let json = """
        {"id": "M1", "kind": "meeting", "created_at": 1794623095, "text": "Me: Hello", "title": "Launch planning",
         "duration_s": null, "processed_at": null,
         "extra": {"segments": [{"speaker": "Me", "start": 0, "end": 6}, {"speaker": "B", "start": 6, "end": 2520.4}]}}
        """
        let meeting = try JSONDecoder().decode(ZoneItem.self, from: Data(json.utf8))
        XCTAssertEqual(meeting.length, 2520.4)
        XCTAssertTrue(meeting.isOpen)
        XCTAssertEqual(MenuContent.meetingTitle(meeting), "Launch planning")
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        let now = Date(timeIntervalSince1970: 1794623095 + 86400)
        XCTAssertEqual(MenuContent.meetingFacts(meeting, now: now, calendar: calendar), "Yesterday, 42 min")
        XCTAssertEqual(MenuContent.line(meeting), "Launch planning", "a meeting's row is its title, not its transcript")

        XCTAssertEqual(MenuContent.meetingTitle(item("m", "meeting", title: "  ")), "Meeting")
        // After the words "Last meeting" in the popover, a meeting without a title is not
        // "Meeting" a second time.
        XCTAssertEqual(MenuContent.lastMeetingTitle(meeting), "Launch planning")
        XCTAssertEqual(MenuContent.lastMeetingTitle(item("m", "meeting", title: "  ")), "Untitled")
        XCTAssertEqual(MenuContent.lastMeetingTitle(item("m", "meeting")), "Untitled")
        let untimed = item("m", "meeting")
        XCTAssertNil(untimed.length)
        XCTAssertEqual(MenuContent.meetingFacts(untimed, now: untimed.created, calendar: calendar), Formats.when(untimed.created, now: untimed.created, calendar: calendar))
        // zone's own figure wins, and an item with "extra" of another shape still reads.
        let odd = try JSONDecoder().decode(ZoneItem.self, from: Data(#"{"id": "x", "kind": "dictation", "duration_s": 2.7, "extra": [1, 2], "processed_at": 5}"#.utf8))
        XCTAssertEqual(odd.length, 2.7)
        XCTAssertFalse(odd.isOpen)
    }

    func testLengths() {
        XCTAssertEqual(Formats.length(38), "38 sec")
        XCTAssertEqual(Formats.length(60), "1 min")
        XCTAssertEqual(Formats.length(2520), "42 min")
        XCTAssertEqual(Formats.length(3600), "1 hr")
        XCTAssertEqual(Formats.length(3900), "1 hr 5 min")
    }

    func testTheSyncLineNamesThePhoneThatSyncedLast() {
        var status = StatusReport(
            phones: [
                .init(name: "Work iPhone", endpointID: "a", lastSyncAge: 4000, itemsReceived: 10, lastPath: "relay"),
                .init(name: "Demo iPhone", endpointID: "b", lastSyncAge: 130, itemsReceived: 184, lastPath: "direct"),
            ],
            items: .init(total: 207),
            daemon: .init(running: true, relayConnected: true),
            lastSync: .init(link: .init(time: nil, age: 130))
        )
        XCTAssertEqual(MenuContent.syncLine(status), "Demo iPhone, synced 2 min ago")
        XCTAssertEqual(MenuContent.syncDetails(status, today: 12), [
            "Work iPhone: synced 1 hr ago, 10 items, through Zone's relay",
            "Demo iPhone: synced 2 min ago, 184 items, straight over this network",
            "207 in your library, 12 today",
        ])

        // A backup read after the last sync is the news.
        status.lastSync.backupFolder = .init(time: nil, age: 30)
        XCTAssertEqual(MenuContent.syncLine(status), "Backup folder read 30 sec ago")

        // One phone: it is in the line, so the details do not name its sync again.
        let one = StatusReport(phones: [.init(name: "Demo iPhone", endpointID: "b", lastSyncAge: 5, itemsReceived: 1, lastPath: "direct")], items: .init(total: 1), daemon: .init(running: true, relayConnected: false))
        XCTAssertEqual(MenuContent.syncLine(one), "Demo iPhone, synced just now")
        XCTAssertEqual(MenuContent.syncDetails(one, today: 1), [
            "1 item from Demo iPhone, straight over this network",
            "Reachable on this network only, while zone reconnects to the relay",
            "1 in your library, 1 today",
        ])

        XCTAssertEqual(MenuContent.syncLine(StatusReport(phones: [.init(name: "Demo iPhone", endpointID: "b")])), "Demo iPhone, waiting for its first sync")
        XCTAssertEqual(MenuContent.syncLine(StatusReport()), "Ready to pair")
    }

    func testLinksIntoTheHistoryPage() {
        let page = URL(string: "http://127.0.0.1:47821/?t=abc123")!
        XCTAssertEqual(PageRoutes.url(page: page, route: .home), page)
        XCTAssertEqual(PageRoutes.url(page: page, route: .meeting("4CDD2055-930D-4EAF")).absoluteString, "http://127.0.0.1:47821/?t=abc123#/meeting/4CDD2055-930D-4EAF")
        XCTAssertEqual(PageRoutes.url(page: page, route: .notes).absoluteString, "http://127.0.0.1:47821/?t=abc123#/notes")
        XCTAssertEqual(PageRoutes.url(page: page, route: .settings).absoluteString, "http://127.0.0.1:47821/?t=abc123#/settings")
        // An id is never allowed to add to the address.
        XCTAssertEqual(PageRoutes.fragment(.meeting("a/b#c d")), "/meeting/a%2Fb%23c%20d")
        // A link that already points somewhere is pointed again, not added to.
        let there = URL(string: "http://127.0.0.1:47821/?t=abc123#/notes")!
        XCTAssertEqual(PageRoutes.url(page: there, route: .home).absoluteString, "http://127.0.0.1:47821/?t=abc123")
    }
}
