import XCTest
@testable import ZoneKit

/// The key that opens the menu from anywhere: how it is written, which keys are refused, and
/// that it is off until the person turns it on.
final class HotKeyTests: XCTestCase {
    func testSuggestedKeyAndHowKeysAreWritten() {
        XCTAssertEqual(HotKey.suggested.display, "⌃⌥Z")
        XCTAssertTrue(HotKey.suggested.isUsable)
        XCTAssertNil(HotKey.suggested.problem)
        XCTAssertEqual(HotKey(keyCode: 49, modifiers: HotKey.command | HotKey.shift | HotKey.option | HotKey.control).display, "⌃⌥⇧⌘Space")
        XCTAssertEqual(HotKey(keyCode: 122, modifiers: 0).display, "F1")
        XCTAssertEqual(HotKey(keyCode: 90, modifiers: 0).display, "F20")
    }

    func testFromAKeyPress() {
        // NSEvent.ModifierFlags: shift 1<<17, control 1<<18, option 1<<19, command 1<<20, and
        // bits that are not modifiers (caps lock 1<<16, the function key 1<<23) are dropped.
        let key = HotKey(keyCode: 6, cocoaFlags: (1 << 18) | (1 << 19) | (1 << 16) | (1 << 23))
        XCTAssertEqual(key, .suggested)
        XCTAssertEqual(HotKey(keyCode: 0, cocoaFlags: (1 << 20) | (1 << 17)).modifiers, HotKey.command | HotKey.shift)
        // Bits Carbon does not mean as modifiers are not kept either.
        XCTAssertEqual(HotKey(keyCode: 6, modifiers: HotKey.control | 1).modifiers, HotKey.control)
    }

    func testKeysThatWouldGetInTheWayAreRefused() {
        XCTAssertEqual(HotKey(keyCode: 6, modifiers: 0).problem, "Add Control, Option or Command, so the key still types.")
        XCTAssertFalse(HotKey(keyCode: 6, modifiers: HotKey.shift).isUsable)
        XCTAssertTrue(HotKey(keyCode: 96, modifiers: 0).isUsable, "a function key may stand alone")
        XCTAssertEqual(HotKey(keyCode: 49, modifiers: HotKey.command).problem, "macOS already uses ⌘Space.")
        XCTAssertNotNil(HotKey(keyCode: 999, modifiers: HotKey.command).problem)
    }

    func testOffUntilTurnedOnAndRemembered() throws {
        let fresh = AppState()
        XCTAssertNil(fresh.hotKey)
        XCTAssertEqual(fresh.hotKeyChoice, .suggested)
        // A file from before the hot key reads as off.
        let old = try JSONDecoder().decode(AppState.self, from: Data(#"{"welcomeDone": true}"#.utf8))
        XCTAssertNil(old.hotKey)
        XCTAssertFalse(old.pasteExplained)

        var state = AppState()
        let mine = HotKey(keyCode: 49, modifiers: HotKey.option)
        state.hotKey = mine
        state.recordedHotKey = mine
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("zone-hotkey-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        state.save(to: file)
        var loaded = AppState.load(from: file)
        XCTAssertEqual(loaded.hotKey, mine)
        // Off, and on again: the recorded key comes back.
        loaded.hotKey = nil
        XCTAssertEqual(loaded.hotKeyChoice, mine)
        // A key of another shape in the file is off, not a failure to read the rest.
        let odd = try JSONDecoder().decode(AppState.self, from: Data(#"{"welcomeDone": true, "hotKey": "⌃Z"}"#.utf8))
        XCTAssertTrue(odd.welcomeDone)
        XCTAssertNil(odd.hotKey)
    }
}
