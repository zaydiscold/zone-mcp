import XCTest
@testable import ZoneKit

/// The search in the menu: what zone's snippet says matched, waiting for the typing to stop,
/// giving up a search the person typed past, and the row the arrow keys are on.
final class SearchTests: XCTestCase {
    func testSnippetPartsAndMatchedWords() {
        let parts = SearchSnippet.parts(" ... signed the [lease] on [Wednesday], the [Lease] ...")
        XCTAssertEqual(parts, [
            .init(" ... signed the ", matched: false), .init("lease", matched: true), .init(" on ", matched: false),
            .init("Wednesday", matched: true), .init(", the ", matched: false), .init("Lease", matched: true), .init(" ...", matched: false),
        ])
        XCTAssertEqual(SearchSnippet.matchedWords(" ... signed the [lease] on [Wednesday], the [Lease] ..."), ["lease", "Wednesday"])
        // A bracket the person said, never closed, stays text. Lines become one.
        XCTAssertEqual(SearchSnippet.parts("a [b\nc"), [.init("a [b c", matched: false)])
        XCTAssertEqual(SearchSnippet.parts("[x][y]"), [.init("xy", matched: true)])
        XCTAssertEqual(SearchSnippet.parts(""), [])
    }

    func testSearchOutputReadsSnippetAndHowItMatched() throws {
        let json = """
        {"total": 2, "matched": "prefix", "next_cursor": null, "items": [
          {"id": "A", "kind": "note", "created_at": 1763200000, "text": "the lease on Wednesday", "snippet": "the [lease] on Wednesday"},
          {"id": "B", "kind": "dictation", "source": "keyboard", "created_at": 1763100000, "text": "leasing"}
        ]}
        """
        let list = try JSONDecoder().decode(ItemList.self, from: Data(json.utf8))
        XCTAssertEqual(list.matched, "prefix")
        XCTAssertEqual(list.items.map(\.snippet), ["the [lease] on Wednesday", nil])
        // `zone list` has neither, and still reads.
        let plain = try JSONDecoder().decode(ItemList.self, from: Data(#"{"total": 0, "items": []}"#.utf8))
        XCTAssertNil(plain.matched)
    }

    /// Records every search it is asked for, and answers after `wait`.
    final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private var _asked: [String] = []
        var asked: [String] { lock.withLock { _asked } }

        func search(_ words: String, wait: Duration = .zero) async throws -> ItemList {
            lock.withLock { _asked.append(words) }
            try await Task.sleep(for: wait)
            return ItemList(total: 1, items: [ZoneItem(id: words, kind: "note", createdAt: 0, text: words)])
        }
    }

    @MainActor
    func testTypingQuicklyAsksOnceForTheLastWords() async {
        let recorder = Recorder()
        let search = LiveSearch(delay: .milliseconds(80)) { try await recorder.search($0) }
        for text in ["l", "le", "lea", "leas", "lease"] {
            search.update(text)
            try? await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertEqual(search.phase, .searching)
        await search.settle()
        XCTAssertEqual(recorder.asked, ["lease"])
        XCTAssertEqual(search.items.map(\.id), ["lease"])
    }

    @MainActor
    func testAnAnswerForOlderWordsIsThrownAway() async {
        let recorder = Recorder()
        // The first search is slow, the second fast: the slow answer must not win.
        let search = LiveSearch(delay: .milliseconds(5)) { words in
            try await recorder.search(words, wait: words == "slow" ? .milliseconds(300) : .zero)
        }
        search.update("slow")
        try? await Task.sleep(for: .milliseconds(60))
        search.update("fast")
        await search.settle()
        try? await Task.sleep(for: .milliseconds(350))
        XCTAssertEqual(recorder.asked, ["slow", "fast"])
        XCTAssertEqual(search.items.map(\.id), ["fast"])
    }

    @MainActor
    func testEmptyingTheFieldStopsTheSearch() async {
        let recorder = Recorder()
        let search = LiveSearch(delay: .milliseconds(40)) { try await recorder.search($0) }
        search.update("lease")
        search.update("   ")
        XCTAssertEqual(search.phase, .idle)
        XCTAssertFalse(search.isActive)
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(recorder.asked, [])
        // The same words again do not search again.
        search.update("x")
        await search.settle()
        search.update("x")
        await search.settle()
        XCTAssertEqual(recorder.asked, ["x"])
    }

    @MainActor
    func testAFailureSaysWhy() async {
        let search = LiveSearch(delay: .zero) { _ in throw ZoneError.failed("no library") }
        search.update("x")
        await search.settle()
        XCTAssertEqual(search.phase, .failed("no library"))
    }

    func testCursor() {
        var cursor = ListCursor()
        cursor.move(1, count: 3)
        XCTAssertEqual(cursor.index, 0)
        cursor.move(1, count: 3); cursor.move(1, count: 3); cursor.move(1, count: 3)
        XCTAssertEqual(cursor.index, 2)
        cursor.move(-5, count: 3)
        XCTAssertEqual(cursor.index, 0)
        var fromNothing = ListCursor()
        fromNothing.move(-1, count: 4)
        XCTAssertEqual(fromNothing.index, 3)
        cursor.move(1, count: 0)
        XCTAssertNil(cursor.index)
        cursor.reset(count: 5)
        XCTAssertEqual(cursor.index, 0)
        cursor.move(4, count: 5)
        cursor.clamp(count: 2)
        XCTAssertEqual(cursor.index, 1)
        cursor.clamp(count: 0)
        XCTAssertNil(cursor.index)
    }
}
