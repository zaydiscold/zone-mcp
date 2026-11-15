import Foundation

// Clips: text the person sent from the phone to this computer on purpose (what was on the
// phone's clipboard, something shared from another app, or something typed). zone keeps a
// clip like any other item, of kind "clip".

/// One clip, from `zone clip --json` and `zone clip list --json`.
public struct Clip: Decodable, Equatable, Sendable, Identifiable {
    public var id: String
    /// How it was sent: "clipboard", "share", "shortcut" or "typed".
    public var source: String?
    /// When the person sent it. Unix seconds.
    public var createdAt: Double
    /// When it got here. Unix seconds.
    public var receivedAt: Double?
    /// How it got here: "link", "flash" or "backup".
    public var carrier: String?
    /// The phone it came from, as `zone status` names its phones.
    public var deviceID: String?
    /// How many characters.
    public var length: Int
    public var firstLine: String
    /// The whole text. A list of clips leaves it out.
    public var text: String?

    enum CodingKeys: String, CodingKey {
        case id, source, carrier, length, text
        case createdAt = "created_at"
        case receivedAt = "received_at"
        case deviceID = "device_id"
        case firstLine = "first_line"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        source = try c.decodeIfPresent(String.self, forKey: .source)
        createdAt = try c.decodeIfPresent(Double.self, forKey: .createdAt) ?? 0
        receivedAt = try c.decodeIfPresent(Double.self, forKey: .receivedAt)
        carrier = try c.decodeIfPresent(String.self, forKey: .carrier)
        deviceID = try c.decodeIfPresent(String.self, forKey: .deviceID)
        text = try c.decodeIfPresent(String.self, forKey: .text)
        length = try c.decodeIfPresent(Int.self, forKey: .length) ?? text?.count ?? 0
        firstLine = try c.decodeIfPresent(String.self, forKey: .firstLine) ?? ""
    }

    public init(id: String, source: String? = nil, createdAt: Double, receivedAt: Double? = nil, deviceID: String? = nil, text: String) {
        self.id = id
        self.source = source
        self.createdAt = createdAt
        self.receivedAt = receivedAt
        self.deviceID = deviceID
        self.text = text
        self.length = text.count
        self.firstLine = text.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
    }

    /// A clip as the library lists it (`zone list --json`), for when `zone clip` is not there.
    public init(_ item: ZoneItem) {
        self.init(id: item.id, source: item.source, createdAt: item.createdAt, receivedAt: item.receivedAt, text: item.text)
        carrier = item.carrier
    }

    public var created: Date { Date(timeIntervalSince1970: createdAt) }

    /// When it reached this computer (or, when zone does not say, when it was sent).
    public var arrived: Date { Date(timeIntervalSince1970: receivedAt ?? createdAt) }

    /// The text on one line, for a row.
    public var preview: String {
        guard let text else { return firstLine }
        return text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
    }

    /// True while the clip is fresh enough to lead the menu: it got here within the window.
    public func isRecent(now: Date = Date(), window: TimeInterval = Clip.leadsFor) -> Bool {
        let age = now.timeIntervalSince(arrived)
        return age < window && age > -120
    }

    /// How long a clip that just arrived stays at the top of the menu.
    public static let leadsFor: TimeInterval = 15 * 60

    /// "From Demo iPhone, 10:42 AM". Without a name, "From your iPhone".
    public func title(phone: String?, now: Date = Date(), calendar: Calendar = .current) -> String {
        "From \(phone ?? "your iPhone"), \(Formats.when(created, now: now, calendar: calendar))"
    }

    /// How it was sent, in a few words. Nil when zone does not say.
    public var sentHow: String? {
        switch source {
        case "clipboard": return "from its clipboard"
        case "share": return "shared from an app"
        case "shortcut": return "from a shortcut"
        case "typed": return "typed in Zone"
        default: return nil
        }
    }
}

/// `zone clip --json`: the newest clip, or none.
public struct ClipAnswer: Decodable, Equatable, Sendable {
    public var clip: Clip?
}

/// `zone clip list --json`.
public struct ClipList: Decodable, Equatable, Sendable {
    public var total: Int
    public var clips: [Clip]

    enum CodingKeys: String, CodingKey { case total, clips }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        clips = try c.decodeIfPresent([Clip].self, forKey: .clips) ?? []
        total = try c.decodeIfPresent(Int.self, forKey: .total) ?? clips.count
    }
}

/// `zone clipboard status --json`: whether a clip is put on this computer's clipboard as it
/// arrives. Off until the person turns it on.
public struct ClipboardSetting: Decodable, Equatable, Sendable {
    public var enabled: Bool
    /// False when this computer has no tool that can write the clipboard.
    public var available: Bool
    /// What to do about that, in zone's words.
    public var fix: String?

    enum CodingKeys: String, CodingKey { case enabled, available, fix, clipboard }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) {
            self.enabled = enabled
        } else {
            enabled = (try c.decodeIfPresent(String.self, forKey: .clipboard)) == "on"
        }
        available = try c.decodeIfPresent(Bool.self, forKey: .available) ?? true
        fix = try c.decodeIfPresent(String.self, forKey: .fix)
    }

    public init(enabled: Bool, available: Bool = true, fix: String? = nil) {
        self.enabled = enabled
        self.available = available
        self.fix = fix
    }
}
