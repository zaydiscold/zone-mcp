import AppKit
import ApplicationServices
import Carbon.HIToolbox
import SwiftUI
import ZoneKit

// The key that opens the menu from any app, and pasting a row into the app that was in
// front. Both without a package: Carbon's RegisterEventHotKey for the key (it needs no
// permission), and a Command-V posted to the system for pasting (that needs Accessibility).

/// Holds the one key registered with the system, and calls `onPress` when it is pressed.
@MainActor
final class HotKeyCenter {
    var onPress: () -> Void = {}
    private var registered: EventHotKeyRef?
    private var handler: EventHandlerRef?
    private(set) var current: HotKey?

    /// Registers this key in place of the one before. Returns false when the system refuses
    /// it (another app registered the same key), and then no key is registered.
    @discardableResult
    func register(_ key: HotKey?) -> Bool {
        unregister()
        guard let key else { return true }
        installHandler()
        var reference: EventHotKeyRef?
        let id = EventHotKeyID(signature: OSType(0x5A4F_4E45), id: 1) // "ZONE"
        let status = RegisterEventHotKey(key.keyCode, key.modifiers, id, GetEventDispatcherTarget(), 0, &reference)
        guard status == noErr, let reference else { return false }
        registered = reference
        current = key
        return true
    }

    func unregister() {
        if let registered { UnregisterEventHotKey(registered) }
        registered = nil
        current = nil
    }

    private func installHandler() {
        guard handler == nil else { return }
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        let me = Unmanaged.passUnretained(self).toOpaque()
        InstallEventHandler(GetEventDispatcherTarget(), { _, _, data in
            guard let data else { return OSStatus(eventNotHandledErr) }
            let center = Unmanaged<HotKeyCenter>.fromOpaque(data).takeUnretainedValue()
            DispatchQueue.main.async { MainActor.assumeIsolated { center.onPress() } }
            return noErr
        }, 1, &spec, me, &handler)
    }
}

/// Pasting into another app: Command-V, posted as if typed. macOS lets an app do that only
/// once the person has switched it on in Privacy & Security > Accessibility.
enum Paster {
    /// True when Zone may post keys. With `ask`, macOS shows its own question (once) if not.
    static func allowed(ask: Bool) -> Bool {
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: ask] as CFDictionary
        return AXIsProcessTrustedWithOptions(options)
    }

    /// Brings `app` back to the front and presses Command-V in it.
    @MainActor
    static func paste(into app: NSRunningApplication?) {
        app?.activate()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12) {
            let source = CGEventSource(stateID: .combinedSessionState)
            let v = CGKeyCode(kVK_ANSI_V)
            for down in [true, false] {
                let event = CGEvent(keyboardEventSource: source, virtualKey: v, keyDown: down)
                event?.flags = .maskCommand
                event?.post(tap: .cghidEventTap)
            }
        }
    }

    /// The letter of the person's own keyboard layout for a key, for writing a shortcut the
    /// way their keys are labelled. Nil when the layout cannot be read.
    static func keyName(_ keyCode: UInt32) -> String? {
        guard let source = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue(),
              let pointer = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData)
        else { return nil }
        let data = Unmanaged<CFData>.fromOpaque(pointer).takeUnretainedValue() as Data
        var dead: UInt32 = 0
        var length = 0
        var characters = [UniChar](repeating: 0, count: 4)
        let status = data.withUnsafeBytes { bytes -> OSStatus in
            guard let layout = bytes.baseAddress?.assumingMemoryBound(to: UCKeyboardLayout.self) else { return -1 }
            return UCKeyTranslate(layout, UInt16(keyCode), UInt16(kUCKeyActionDisplay), 0, UInt32(LMGetKbdType()),
                                  OptionBits(kUCKeyTranslateNoDeadKeysBit), &dead, 4, &length, &characters)
        }
        guard status == noErr, length > 0 else { return nil }
        let text = String(utf16CodeUnits: characters, count: length).uppercased()
        return text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : text
    }
}

extension HotKey {
    /// The key as the person's keyboard labels it: "⌃⌥Z", or "⌃⌥Y" on a German layout.
    var label: String {
        guard HotKey.isLetterOrDigit(keyCode), let name = Paster.keyName(keyCode) else { return display }
        return String(display.dropLast(display.count - modifierPrefixLength)) + name
    }

    private var modifierPrefixLength: Int {
        [HotKey.control, HotKey.option, HotKey.shift, HotKey.command].filter { modifiers & $0 != 0 }.count
    }

    static func isLetterOrDigit(_ code: UInt32) -> Bool {
        HotKey(keyCode: code, modifiers: 0).display.count == 1
    }
}

/// The key that opens the menu, as a key cap: click it and press the key to use instead.
/// Escape gives up. While it listens, the old key is let go of, so pressing it again is
/// recorded rather than opening the menu.
struct HotKeyRecorder: View {
    let model: AppModel
    @State private var listening = false
    @State private var monitor: Any?
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(listening ? "Type a key" : model.state.hotKeyChoice.label) {
            listening ? stop(keep: true) : start()
        }
        .buttonStyle(ZoneKeyButtonStyle(height: 26))
        .help(listening ? "Press the key to use, with Control, Option or Command. Escape keeps the one you have." : "Click, then press another key")
        .accessibilityLabel(listening ? "Listening for a key" : "Key: \(model.state.hotKeyChoice.label). Change")
        .onDisappear { stop(keep: true) }
    }

    private func start() {
        listening = true
        model.setHotKeyTrouble(nil)
        _ = model.registerHotKey(nil)
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            let code = event.keyCode, flags = event.modifierFlags
            MainActor.assumeIsolated {
                if code == 53, flags.intersection([.command, .option, .control]).isEmpty {
                    stop(keep: true)
                    return
                }
                let key = HotKey(keyCode: code, cocoaFlags: flags.rawValue)
                if let problem = key.problem {
                    model.setHotKeyTrouble(problem)
                    return
                }
                stop(keep: false)
                model.setHotKey(key)
            }
            return nil
        }
    }

    /// Stops listening. `keep` puts the key that was there before back.
    private func stop(keep: Bool) {
        guard listening else { return }
        listening = false
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        if keep, let key = model.state.hotKey { _ = model.registerHotKey(key) }
    }
}
