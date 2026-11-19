import Foundation

/// A key and its modifiers that open the menu from anywhere, as Carbon's
/// `RegisterEventHotKey` takes them: a virtual key code and Carbon's modifier bits.
///
/// It is off until the person turns it on. A key that other apps use cannot be found out
/// beforehand (an app's own menu shortcut is not registered anywhere zone could look), so
/// Zone never takes one by itself.
public struct HotKey: Codable, Equatable, Sendable {
    public var keyCode: UInt32
    public var modifiers: UInt32

    // Carbon's modifier bits (Events.h).
    public static let command: UInt32 = 1 << 8
    public static let shift: UInt32 = 1 << 9
    public static let option: UInt32 = 1 << 11
    public static let control: UInt32 = 1 << 12

    // The same in NSEvent.ModifierFlags, which the app hands over from a key press.
    static let cocoaShift: UInt = 1 << 17
    static let cocoaControl: UInt = 1 << 18
    static let cocoaOption: UInt = 1 << 19
    static let cocoaCommand: UInt = 1 << 20

    public init(keyCode: UInt32, modifiers: UInt32) {
        self.keyCode = keyCode
        self.modifiers = modifiers & (Self.command | Self.shift | Self.option | Self.control)
    }

    /// From a key press: its key code and `NSEvent.modifierFlags.rawValue`.
    public init(keyCode: UInt16, cocoaFlags: UInt) {
        var bits: UInt32 = 0
        if cocoaFlags & Self.cocoaCommand != 0 { bits |= Self.command }
        if cocoaFlags & Self.cocoaShift != 0 { bits |= Self.shift }
        if cocoaFlags & Self.cocoaOption != 0 { bits |= Self.option }
        if cocoaFlags & Self.cocoaControl != 0 { bits |= Self.control }
        self.init(keyCode: UInt32(keyCode), modifiers: bits)
    }

    /// Control-Option-Z: what the switch offers until another key is recorded. Z for Zone,
    /// and two modifiers that few apps put together on a letter.
    public static let suggested = HotKey(keyCode: 6, modifiers: control | option)

    /// A key alone, or with Shift only, would take a letter away from typing everywhere. A
    /// function key may stand alone.
    public var isUsable: Bool {
        guard Self.names[keyCode] != nil else { return false }
        if Self.functionKeys.contains(keyCode) { return true }
        return modifiers & (Self.command | Self.option | Self.control) != 0
    }

    /// Why a key that was pressed cannot be used, in one sentence. Nil when it can.
    public var problem: String? {
        if Self.names[keyCode] == nil { return "That key cannot open Zone. Try a letter, a number or Space." }
        if !isUsable { return "Add Control, Option or Command, so the key still types." }
        if Self.taken.contains(self) { return "macOS already uses \(display)." }
        return nil
    }

    /// "⌃⌥Z", in the order macOS writes a shortcut.
    public var display: String {
        var text = ""
        if modifiers & Self.control != 0 { text += "⌃" }
        if modifiers & Self.option != 0 { text += "⌥" }
        if modifiers & Self.shift != 0 { text += "⇧" }
        if modifiers & Self.command != 0 { text += "⌘" }
        return text + (Self.names[keyCode] ?? "?")
    }

    /// Shortcuts macOS keeps for itself on every Mac.
    static let taken: [HotKey] = [
        HotKey(keyCode: 49, modifiers: command), // Spotlight
        HotKey(keyCode: 49, modifiers: command | option), // Finder search
        HotKey(keyCode: 49, modifiers: control), // input sources
        HotKey(keyCode: 48, modifiers: command), // app switcher
        HotKey(keyCode: 12, modifiers: command), // quit
        HotKey(keyCode: 4, modifiers: command), // hide
        HotKey(keyCode: 13, modifiers: command), // close
        HotKey(keyCode: 9, modifiers: command), HotKey(keyCode: 8, modifiers: command), HotKey(keyCode: 7, modifiers: command), // paste, copy, cut
        HotKey(keyCode: 6, modifiers: command), // undo
        HotKey(keyCode: 20, modifiers: command | shift), HotKey(keyCode: 21, modifiers: command | shift), HotKey(keyCode: 23, modifiers: command | shift), // screenshots
        HotKey(keyCode: 12, modifiers: command | control), // lock screen
    ]

    /// F1 to F20, in order.
    static let functionKeyCodes: [UInt32] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90]
    static let functionKeys = Set(functionKeyCodes)

    /// The keys of a US keyboard by virtual key code (Events.h, kVK_*). The app writes the
    /// letter of the person's own layout where it differs; this is what is known without it.
    static let names: [UInt32: String] = {
        var names: [UInt32: String] = [
            0: "A", 11: "B", 8: "C", 2: "D", 14: "E", 3: "F", 5: "G", 4: "H", 34: "I", 38: "J", 40: "K",
            37: "L", 46: "M", 45: "N", 31: "O", 35: "P", 12: "Q", 15: "R", 1: "S", 17: "T", 32: "U",
            9: "V", 13: "W", 7: "X", 16: "Y", 6: "Z",
            29: "0", 18: "1", 19: "2", 20: "3", 21: "4", 23: "5", 22: "6", 26: "7", 28: "8", 25: "9",
            49: "Space", 36: "Return", 48: "Tab", 51: "Delete", 53: "Esc",
            27: "-", 24: "=", 33: "[", 30: "]", 42: "\\", 41: ";", 39: "'", 43: ",", 47: ".", 44: "/", 50: "`",
            123: "←", 124: "→", 125: "↓", 126: "↑",
        ]
        for (index, code) in functionKeyCodes.enumerated() { names[code] = "F\(index + 1)" }
        return names
    }()
}
