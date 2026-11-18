import XCTest
@testable import ZoneKit

/// Clips: text sent from the phone on purpose. What zone prints about them, and what the
/// app makes of it.
final class ClipTests: XCTestCase {
    func decode<T: Decodable>(_ type: T.Type, _ text: String) throws -> T {
        try JSONDecoder().decode(T.self, from: Data(text.utf8))
    }

    static let newest = """
    {"clip": {"id": "0B5C1B0E-7C0F-4E2B-9E53-2F5E7C1D9A11", "source": "clipboard", "created_at": 1794786120.5,
              "created_local": "2026-11-15T15:42:00-08:00", "received_at": 1794786121.0, "age_s": 4.0,
              "carrier": "link", "device_id": "63532d48", "length": 34, "bytes": 34, "lines": 2,
              "first_line": "Gate code 4417", "text": "Gate code 4417\\nBack door, not front"}}
    """

    // MARK: zone clip

    func testTheNewestClip() throws {
        let clip = try XCTUnwrap(decode(ClipAnswer.self, Self.newest).clip)
        XCTAssertEqual(clip.id, "0B5C1B0E-7C0F-4E2B-9E53-2F5E7C1D9A11")
        XCTAssertEqual(clip.source, "clipboard")
        XCTAssertEqual(clip.deviceID, "63532d48")
        XCTAssertEqual(clip.length, 34)
        XCTAssertEqual(clip.text, "Gate code 4417\nBack door, not front")
        XCTAssertEqual(clip.preview, "Gate code 4417 Back door, not front", "one line for a row")
        XCTAssertEqual(clip.sentHow, "from its clipboard")
        XCTAssertEqual(clip.arrived.timeIntervalSince1970, 1794786121.0, accuracy: 0.01)
    }

    func testNoClipYet() throws {
        XCTAssertNil(try decode(ClipAnswer.self, #"{"clip": null}"#).clip)
        XCTAssertNil(try decode(ClipAnswer.self, "{}").clip)
    }

    func testAListOfClipsHasNoText() throws {
        let list = try decode(ClipList.self, """
        {"total": 7, "clips": [
          {"id": "A", "source": "share", "created_at": 1794786120.5, "received_at": 1794786121.0, "carrier": "link",
           "device_id": "63532d48", "length": 21, "bytes": 21, "lines": 1, "first_line": "https://zayd.wtf/zone"},
          {"id": "B", "source": "typed", "created_at": 1794780000.0, "length": 5, "first_line": "hello"}
        ]}
        """)
        XCTAssertEqual(list.total, 7)
        XCTAssertEqual(list.clips.map(\.id), ["A", "B"])
        XCTAssertNil(list.clips[0].text)
        XCTAssertEqual(list.clips[0].preview, "https://zayd.wtf/zone", "the first line stands in for the text")
        XCTAssertEqual(list.clips[1].sentHow, "typed in Zone")
        XCTAssertEqual(list.clips[1].arrived, list.clips[1].created, "without a time of arrival, the time it was sent")
    }

    func testAClipOutOfTheLibraryList() throws {
        // `zone list --json` lists a clip like any item. That is enough to show and copy it.
        let item = try decode(ZoneItem.self, """
        {"id": "C", "kind": "clip", "source": "clipboard", "created_at": 1794786120.5, "text": "Gate code 4417",
         "title": null, "words": 3, "carrier": "link", "received_at": 1794786121.0}
        """)
        let clip = Clip(item)
        XCTAssertEqual(clip.text, "Gate code 4417")
        XCTAssertEqual(clip.firstLine, "Gate code 4417")
        XCTAssertEqual(clip.carrier, "link")
        XCTAssertEqual(clip.receivedAt, 1794786121.0)
    }

    // MARK: Leading the menu

    func testAClipLeadsTheMenuWhileItIsFresh() {
        let arrived = Date(timeIntervalSince1970: 1794786121)
        let clip = Clip(id: "A", createdAt: 1794786120, receivedAt: 1794786121, text: "x")
        XCTAssertTrue(clip.isRecent(now: arrived))
        XCTAssertTrue(clip.isRecent(now: arrived.addingTimeInterval(14 * 60)))
        XCTAssertFalse(clip.isRecent(now: arrived.addingTimeInterval(15 * 60 + 1)))
        // A clip sent long ago that only got here now (the phone was offline) is new here.
        let late = Clip(id: "B", createdAt: 1794786121 - 86400, receivedAt: 1794786121, text: "x")
        XCTAssertTrue(late.isRecent(now: arrived.addingTimeInterval(60)))
        // A clock that is far off never makes a clip lead for ever.
        let future = Clip(id: "C", createdAt: 1794786121 + 3600, text: "x")
        XCTAssertFalse(future.isRecent(now: arrived))
    }

    func testTheTitleNamesThePhoneAndTheTime() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        let now = Date(timeIntervalSince1970: 1794786300)
        let clip = Clip(id: "A", createdAt: 1794786120, text: "x")
        XCTAssertEqual(clip.title(phone: "Demo iPhone", now: now, calendar: calendar), "From Demo iPhone, 3:42 PM")
        XCTAssertEqual(clip.title(phone: nil, now: now, calendar: calendar), "From your iPhone, 3:42 PM")
        XCTAssertEqual(clip.label(phone: "Demo iPhone", now: now, calendar: calendar), "Latest clip, from Demo iPhone, 3:42 PM",
                       "the popover's clip has no box or icon, so its label says what it is")
    }

    // MARK: zone clipboard

    func testTheClipboardSetting() throws {
        let on = try decode(ClipboardSetting.self, #"{"clipboard": "on", "enabled": true, "available": true, "tool": "pbcopy", "fix": null, "max_age_s": 120.0, "daemon_running": true}"#)
        XCTAssertTrue(on.enabled)
        XCTAssertTrue(on.available)
        XCTAssertNil(on.fix)
        let off = try decode(ClipboardSetting.self, #"{"clipboard": "off", "enabled": false, "available": false, "tool": null, "fix": "Install xclip."}"#)
        XCTAssertFalse(off.enabled)
        XCTAssertFalse(off.available)
        XCTAssertEqual(off.fix, "Install xclip.")
        // Only the word: still read.
        XCTAssertTrue(try decode(ClipboardSetting.self, #"{"clipboard": "on"}"#).enabled)
        XCTAssertFalse(try decode(ClipboardSetting.self, "{}").enabled, "off until the person turns it on")
    }

    struct FakeRunner: ZoneRunning {
        var answers: [String: CommandOutput]
        func run(_ arguments: [String], timeout: TimeInterval) async throws -> CommandOutput {
            answers[arguments.joined(separator: " ")] ?? CommandOutput(status: 2, stderr: Data("error: unrecognized subcommand 'clipboard'".utf8))
        }
    }

    func testTheClientAsksZoneAboutClips() async throws {
        let client = ZoneClient(runner: FakeRunner(answers: [
            "clipboard --help": CommandOutput(text: "Usage: zone clipboard"),
            "clip --json": CommandOutput(text: Self.newest),
            "clip list --json --limit 5": CommandOutput(text: #"{"total": 0, "clips": []}"#),
            "clipboard status --json": CommandOutput(text: #"{"clipboard": "off", "enabled": false, "available": true}"#),
            "clipboard on --json": CommandOutput(text: #"{"clipboard": "on", "enabled": true, "available": true}"#),
            "clipboard off --json": CommandOutput(text: #"{"clipboard": "off", "enabled": false, "available": true}"#),
        ]))
        let has = await client.hasClips()
        XCTAssertTrue(has)
        let newest = try await client.latestClip()
        XCTAssertEqual(newest?.firstLine, "Gate code 4417")
        let list = try await client.clips(limit: 5)
        XCTAssertTrue(list.clips.isEmpty)
        let before = try await client.clipboardSetting()
        XCTAssertFalse(before.enabled)
        let on = try await client.setClipboard(true)
        XCTAssertTrue(on.enabled)
        let off = try await client.setClipboard(false)
        XCTAssertFalse(off.enabled)
    }

    func testAZoneFromBeforeClips() async {
        // Every clip command is unknown to it. The app then shows nothing about clips.
        let client = ZoneClient(runner: FakeRunner(answers: [:]))
        let has = await client.hasClips()
        XCTAssertFalse(has)
        do {
            _ = try await client.latestClip()
            XCTFail("an older zone has no clip command")
        } catch {
            XCTAssertEqual(error as? ZoneError, .failed("error: unrecognized subcommand 'clipboard'"))
        }
    }

    // MARK: The clip event

    static let copied = #"{"seq":21,"at":1794786121.0,"time":"2026-11-15T15:42:01-08:00","event":"clip","id":"A","device":"Demo iPhone","device_id":"63532d48","carrier":"link","source":"clipboard","length":34,"bytes":34,"copied":true,"not_copied":null,"tool":"pbcopy"}"#
    static let kept = #"{"seq":22,"at":1794786180.0,"time":"2026-11-15T15:43:00-08:00","event":"clip","id":"B","device":"Demo iPhone","device_id":"63532d48","carrier":"link","source":"share","length":21,"bytes":21,"copied":false,"not_copied":"off","tool":null}"#

    func testAClipEvent() throws {
        let event = try XCTUnwrap(EventLineDecoder.decode(Self.copied))
        XCTAssertEqual(event.kind, .clip)
        XCTAssertEqual(event.clipID, "A")
        XCTAssertEqual(event.length, 34)
        XCTAssertEqual(event.copied, true)
        XCTAssertNil(event.notCopied)
        XCTAssertTrue(event.broughtItems, "the menu bar icon shows it arriving, like any item")
        XCTAssertTrue(event.isNews)
        XCTAssertEqual(ActivityWords.headline(event), "Clip from Demo iPhone, copied")

        let kept = try XCTUnwrap(EventLineDecoder.decode(Self.kept))
        XCTAssertEqual(kept.copied, false)
        XCTAssertEqual(kept.notCopied, "off")
        XCTAssertEqual(ActivityWords.headline(kept), "Clip from Demo iPhone")

        var failed = kept
        failed.notCopied = "failed"
        XCTAssertEqual(ActivityWords.headline(failed), "Clip from Demo iPhone, not copied")
    }

    func testASyncThatOnlyBroughtTheClipIsNotSaidTwice() throws {
        var feed = ActivityFeed()
        // The phone connects, sends one clip, and zone writes both down.
        feed.add(try XCTUnwrap(EventLineDecoder.decode(Self.copied)))
        feed.add(try XCTUnwrap(EventLineDecoder.decode(#"{"seq":23,"at":1794786121.4,"event":"link_session","device":"Demo iPhone","device_id":"63532d48","items":1,"new":1,"changed":0,"path":"direct"}"#)))
        XCTAssertEqual(feed.news.map(\.kind), [.clip])
        XCTAssertEqual(feed.events.count, 2, "both are kept, only one is shown")

        // A sync that brought a clip and two dictations is still news.
        feed.add(try XCTUnwrap(EventLineDecoder.decode(Self.kept)))
        feed.add(try XCTUnwrap(EventLineDecoder.decode(#"{"seq":24,"at":1794786180.3,"event":"link_session","device":"Demo iPhone","device_id":"63532d48","items":3,"new":3,"changed":0,"path":"direct"}"#)))
        XCTAssertEqual(feed.news.map(\.kind), [.linkSession, .clip, .clip])

        // Another phone's sync at the same moment is its own news.
        feed.add(try XCTUnwrap(EventLineDecoder.decode(#"{"seq":25,"at":1794786121.9,"event":"link_session","device":"Other","device_id":"ffff","items":1,"new":1,"changed":0,"path":"relay"}"#)))
        XCTAssertTrue(feed.news.contains { $0.device == "Other" })
    }

    func testCountsNameClips() {
        XCTAssertEqual(ActivityWords.count(of: ["clip": 1], total: 1), "1 clip")
        XCTAssertEqual(ActivityWords.count(of: ["dictation": 2, "clip": 2], total: 4), "2 dictations and 2 clips")
    }

    // MARK: What an agent may read, now that there is a fourth kind

    func testAViewFromAZoneWithClips() {
        let everything = AgentSurvey.Scope(kinds: ["dictation", "note", "meeting", "clip"], limited: false)
        XCTAssertEqual(AgentView(everything, knowsClips: true), .everything)
        XCTAssertEqual(AgentView(everything), .everything, "the entry itself names clips")

        // The three kinds there were before clips are a limit now: clips stay out, and
        // connecting again must not round that up to everything.
        let spoken = AgentSurvey.Scope(kinds: ["dictation", "note", "meeting"], limited: true)
        let view = AgentView(spoken, knowsClips: true)
        XCTAssertEqual(view, .custom(kinds: ["dictation", "note", "meeting"], excludeSources: []))
        XCTAssertEqual(view.arguments, ["--kinds", "dictation,note,meeting", "--exclude-sources", "none"])
        XCTAssertEqual(view.words, "dictations, notes and meetings only")
        XCTAssertEqual(AgentView(spoken), view, "zone calling it limited is enough to know")

        XCTAssertEqual(AgentView(.init(kinds: ["note", "meeting"], limited: true), knowsClips: true), .notesAndMeetings)
        XCTAssertEqual(AgentView(.init(kinds: ["dictation", "note", "meeting", "clip"], excludeSources: ["keyboard"], limited: true), knowsClips: true), .noKeyboard)

        // Spoken kinds only, and no keyboard dictations: both limits are kept.
        let both = AgentView(.init(kinds: ["dictation", "note", "meeting"], excludeSources: ["keyboard"], limited: true), knowsClips: true)
        XCTAssertEqual(both.arguments, ["--kinds", "dictation,note,meeting", "--exclude-sources", "keyboard"])

        // Clips only, set from the command line.
        let clips = AgentView(.init(kinds: ["clip"], limited: true), knowsClips: true)
        XCTAssertEqual(clips.arguments, ["--kinds", "clip", "--exclude-sources", "none"])
        XCTAssertEqual(clips.words, "clips only")
    }

    func testAViewFromAZoneBeforeClips() {
        // There, the three kinds are everything.
        XCTAssertEqual(AgentView(.init(kinds: ["dictation", "note", "meeting"], limited: false)), .everything)
        XCTAssertEqual(AgentView(.init(kinds: ["dictation", "note", "meeting"], excludeSources: ["keyboard"], limited: true)), .noKeyboard)
        XCTAssertEqual(AgentView.noKeyboard.arguments, ["--kinds", "all", "--exclude-sources", "keyboard"])
    }

    func testAnAgentKeepsItsViewWhenConnectedAgain() {
        let survey = AgentSurvey(clients: [
            .init(client: "cursor", label: "Cursor", state: "other_copy", detail: "/Users/zed/.local/bin/zone",
                  scope: .init(kinds: ["dictation", "note", "meeting"], limited: true)),
        ])
        let agent = Agent.list(from: survey, knowsClips: true)[0]
        XCTAssertEqual(agent.installArguments(), ["mcp", "install", "cursor", "--kinds", "dictation,note,meeting", "--exclude-sources", "none"])
        XCTAssertEqual(agent.view?.isLimited, true)
    }

    // MARK: Test runs

    func testATestRunNeverWritesTheRealClipboard() {
        let env = ZoneEnvironment.resolve(from: ["ZONE_MAC_SANDBOX": "/tmp/box"], home: URL(fileURLWithPath: "/Users/zed"))
        XCTAssertEqual(env.childEnvironment()["ZONE_CLIPBOARD_COMMAND"], "cat > '/tmp/box/clipboard'")
        XCTAssertEqual(env.pretendClipboard?.path, "/tmp/box/clipboard")
        let real = ZoneEnvironment.resolve(from: [:], home: URL(fileURLWithPath: "/Users/zed"))
        XCTAssertNil(real.childEnvironment()["ZONE_CLIPBOARD_COMMAND"])
        XCTAssertNil(real.pretendClipboard)
    }
}
