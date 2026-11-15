import Foundation

/// Something that happened in zone: a phone synced, a phone paired, the service started.
/// Names and counts only. An event never carries what the person said.
public struct ActivityEvent: Identifiable, Equatable, Sendable {
    public enum Kind: String, Sendable {
        case daemonStarted = "daemon_started"
        case daemonStopped = "daemon_stopped"
        case relayConnected = "relay_connected"
        case relayLost = "relay_lost"
        case pairOffered = "pair_offered"
        case paired
        case unpaired
        case linkSession = "link_session"
        case linkRefused = "link_refused"
        case flashReceived = "flash_received"
        case flashFailed = "flash_failed"
        case backupImport = "backup_import"
        case resyncRequested = "resync_requested"
        /// A clip arrived: text the person sent from the phone on purpose.
        case clip
        case unknown
    }

    /// "seq:12" for an event from `zone events`, "log:..." for one read out of zone.log.
    public var id: String
    /// The event's number in zone's own list. Nil for events read out of the log.
    public var seq: Int?
    public var date: Date
    public var kind: Kind
    /// The phone's name.
    public var device: String?
    public var deviceID: String?
    /// How many items the phone sent.
    public var items: Int?
    /// How many of them were new or changed here.
    public var arrived: Int?
    /// How the connection travelled: "direct", "relay", "relay then direct".
    public var path: String?
    /// New or changed items by kind ("dictation", "note", "meeting"), when known.
    public var kinds: [String: Int]
    /// Why something was refused or failed.
    public var reason: String?
    /// True when this session or flash also paired the phone.
    public var alsoPaired: Bool
    /// For `paired` and `pair_offered`: the name of the pair code that was shown or used.
    public var offer: String?
    /// For `daemon_started`: the service's process number.
    public var pid: Int?
    /// For `clip`: the clip's id, how many characters it has, and whether zone put it on
    /// this computer's clipboard. Never its text.
    public var clipID: String?
    public var length: Int?
    public var copied: Bool?
    /// For `clip` that was not copied: "off", "old" or "failed".
    public var notCopied: String?
    /// The event's name as zone wrote it, for kinds this app does not know yet.
    public var name: String

    public init(
        id: String,
        seq: Int? = nil,
        date: Date,
        kind: Kind,
        device: String? = nil,
        deviceID: String? = nil,
        items: Int? = nil,
        arrived: Int? = nil,
        path: String? = nil,
        kinds: [String: Int] = [:],
        reason: String? = nil,
        alsoPaired: Bool = false,
        name: String? = nil
    ) {
        self.id = id
        self.seq = seq
        self.date = date
        self.kind = kind
        self.device = device
        self.deviceID = deviceID
        self.items = items
        self.arrived = arrived
        self.path = path
        self.kinds = kinds
        self.reason = reason
        self.alsoPaired = alsoPaired
        self.name = name ?? kind.rawValue
    }

    /// True when this event put something new in the library.
    public var broughtItems: Bool {
        switch kind {
        case .linkSession, .flashReceived, .backupImport: return (arrived ?? 0) > 0
        case .clip: return true
        default: return false
        }
    }

    /// Whether the event belongs in the list a person reads: things arriving, phones coming
    /// and going, and anything that went wrong. The service starting, the relay connecting,
    /// a pair code being shown or a phone checking in with nothing new are not news (the
    /// state they lead to is shown elsewhere).
    public var isNews: Bool {
        switch kind {
        case .pairOffered, .unknown, .daemonStarted, .daemonStopped, .relayConnected, .relayLost: return false
        case .linkSession: return (arrived ?? 0) > 0 || (items ?? 0) > 0
        default: return true
        }
    }
}

// MARK: Reading `zone events --json`

/// Reads one line of `zone events --json --follow`.
///
/// Every line has `seq`, `at` (Unix seconds), `time` and `event`. The rest depends on the
/// event. Unknown events and unknown fields are accepted: zone may add more.
public enum EventLineDecoder {
    public static func decode(_ line: String) -> ActivityEvent? {
        guard let data = line.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let name = object["event"] as? String
        else { return nil }

        let seq = int(object["seq"])
        let date: Date
        if let at = double(object["at"]) {
            date = Date(timeIntervalSince1970: at)
        } else if let time = object["time"] as? String, let parsed = LogLineParser.date(time) {
            date = parsed
        } else {
            date = Date()
        }
        let kind = ActivityEvent.Kind(rawValue: name) ?? .unknown
        var event = ActivityEvent(
            id: seq.map { "seq:\($0)" } ?? "event:\(name):\(date.timeIntervalSince1970)",
            seq: seq,
            date: date,
            kind: kind,
            device: object["device"] as? String,
            deviceID: object["device_id"] as? String,
            items: int(object["items"]),
            path: object["path"] as? String,
            reason: object["reason"] as? String,
            alsoPaired: (object["paired"] as? Bool) ?? false,
            name: name
        )
        event.offer = object["offer"] as? String
        event.pid = int(object["pid"])
        if kind == .clip {
            event.clipID = object["id"] as? String
            event.length = int(object["length"])
            event.copied = object["copied"] as? Bool
            event.notCopied = object["not_copied"] as? String
            // For a clip, "paired" is not said and "path" is the carrier's business.
            event.alsoPaired = false
        }
        let new = int(object["new"])
        let changed = int(object["changed"])
        if new != nil || changed != nil {
            event.arrived = (new ?? 0) + (changed ?? 0)
        }
        if kind == .backupImport, let arrived = event.arrived, arrived > 0, arrived == event.items {
            // A first read of a backup: everything in it is new, so the kinds are known.
            for (key, kindName) in [("dictations", "dictation"), ("notes", "note"), ("meetings", "meeting")] {
                if let count = int(object[key]), count > 0 { event.kinds[kindName] = count }
            }
        }
        return event
    }

    /// True when these lines include the start of the service with this process number.
    public static func daemonStarted(pid: Int, in lines: String) -> Bool {
        lines.split(whereSeparator: \.isNewline).contains { line in
            guard line.contains("daemon_started"), let event = decode(String(line)) else { return false }
            return event.kind == .daemonStarted && event.pid == pid
        }
    }

    private static func int(_ value: Any?) -> Int? {
        if let number = value as? NSNumber { return number.intValue }
        return nil
    }

    private static func double(_ value: Any?) -> Double? {
        if let number = value as? NSNumber { return number.doubleValue }
        return nil
    }
}

// MARK: Reading zone.log

/// Reads one line of `zone.log`, for a zone that does not have `zone events` yet.
///
/// A line is a time, a level and a sentence:
/// `2026-11-15T11:45:38-08:00 info  link: paired Demo iPhone (63532d484a)`
public enum LogLineParser {
    public static func date(_ text: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: text)
    }

    public static func parse(_ line: String) -> ActivityEvent? {
        let parts = line.split(separator: " ", maxSplits: 2, omittingEmptySubsequences: true)
        guard parts.count == 3, let date = date(String(parts[0])) else { return nil }
        let message = parts[2].trimmingCharacters(in: .whitespaces)
        let id = "log:\(parts[0]):\(message)"

        func event(_ kind: ActivityEvent.Kind) -> ActivityEvent {
            ActivityEvent(id: id, date: date, kind: kind)
        }

        if message.hasPrefix("zone "), message.contains(" started for ") {
            return event(.daemonStarted)
        }
        if message == "zone is stopping" {
            return event(.daemonStopped)
        }
        if message.hasPrefix("link: reachable through the relay") {
            return event(.relayConnected)
        }
        if message.hasPrefix("link: no relay connection") {
            return event(.relayLost)
        }
        if let m = match(message, #"^link: (.+) \(([0-9a-f]+)\) sent (\d+) items in (\d+) frames, (\d+) new or changed, path: (.+)$"#) {
            var e = event(.linkSession)
            e.device = m[0]
            e.items = Int(m[2])
            e.arrived = Int(m[4])
            e.path = m[5]
            return e
        }
        if let m = match(message, #"^link: paired (.+) \(([0-9a-f]+)\)$"#) {
            var e = event(.paired)
            e.device = m[0]
            return e
        }
        if let m = match(message, #"^link: refused ([0-9a-f]+): (.+)$"#) {
            var e = event(.linkRefused)
            e.reason = m[1]
            return e
        }
        if let m = match(message, #"^flash: (.+) sent (\d+) items, (\d+) new or changed(, and is now paired)?$"#) {
            var e = event(.flashReceived)
            e.device = m[0] == "a phone" ? nil : m[0]
            e.items = Int(m[1])
            e.arrived = Int(m[2])
            e.alsoPaired = !m[3].isEmpty
            return e
        }
        if let m = match(message, #"^flash: a complete flash of \d+ bytes was not stored: (.+)$"#) {
            var e = event(.flashFailed)
            e.reason = m[0]
            return e
        }
        if let m = match(message, #"^backup folder: read (\d+) notes, (\d+) dictations, (\d+) meetings: (\d+) new, (\d+) changed, (\d+) already here$"#) {
            var e = event(.backupImport)
            let (notes, dictations, meetings) = (Int(m[0]) ?? 0, Int(m[1]) ?? 0, Int(m[2]) ?? 0)
            let (new, changed) = (Int(m[3]) ?? 0, Int(m[4]) ?? 0)
            e.items = notes + dictations + meetings
            e.arrived = new + changed
            if e.arrived == e.items, new > 0 {
                e.kinds = ["note": notes, "dictation": dictations, "meeting": meetings].filter { $0.value > 0 }
            }
            return e
        }
        return nil
    }

    /// The capture groups of a whole-line match. A group that did not take part is "".
    private static func match(_ text: String, _ pattern: String) -> [String]? {
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return nil }
        let range = NSRange(text.startIndex..., in: text)
        guard let found = regex.firstMatch(in: text, range: range) else { return nil }
        return (1..<found.numberOfRanges).map { index in
            Range(found.range(at: index), in: text).map { String(text[$0]) } ?? ""
        }
    }
}

// MARK: Words for a person

public enum ActivityWords {
    /// "3 dictations", "2 notes and 1 meeting", "40 items".
    public static func count(of kinds: [String: Int], total: Int) -> String {
        let order = ["dictation", "note", "meeting", "clip"]
        let parts = order.compactMap { kind -> String? in
            guard let n = kinds[kind], n > 0 else { return nil }
            return "\(Formats.number(n)) \(n == 1 ? kind : kind + "s")"
        }
        let known = kinds.values.reduce(0, +)
        if parts.isEmpty || known != total {
            return "\(Formats.number(total)) \(total == 1 ? "item" : "items")"
        }
        switch parts.count {
        case 1: return parts[0]
        case 2: return "\(parts[0]) and \(parts[1])"
        default: return "\(parts[0]), \(parts[1]) and \(parts[2])"
        }
    }

    /// The one line that says what happened: "3 dictations from Demo iPhone".
    public static func headline(_ event: ActivityEvent) -> String {
        let phone = event.device ?? "your iPhone"
        switch event.kind {
        case .linkSession:
            let arrived = event.arrived ?? 0
            if arrived > 0 {
                return "\(count(of: event.kinds, total: arrived)) from \(phone)"
            }
            return "\(phone) checked in, nothing new"
        case .flashReceived:
            let arrived = event.arrived ?? 0
            if arrived > 0 {
                return "\(count(of: event.kinds, total: arrived)) from \(phone) by Flash"
            }
            return "Flash from \(phone), nothing new"
        case .backupImport:
            let arrived = event.arrived ?? 0
            if arrived > 0 {
                return "\(count(of: event.kinds, total: arrived)) from the backup folder"
            }
            return "Backup folder read, nothing new"
        case .clip:
            if event.copied == true { return "Clip from \(phone), copied" }
            if event.notCopied == "failed" { return "Clip from \(phone), not copied" }
            return "Clip from \(phone)"
        case .paired: return "\(phone) paired"
        case .unpaired: return "\(phone) forgotten"
        case .daemonStarted: return "Zone started"
        case .daemonStopped: return "Zone stopped"
        case .relayConnected: return "Reachable from other networks"
        case .relayLost: return "Lost the relay connection"
        case .linkRefused:
            switch event.reason {
            case "not_paired", "not paired": return "Turned away a phone that is not paired"
            case "busy": return "A phone tried while another was syncing"
            case "version": return "Turned away a phone on another version"
            default: return "Turned away a pair code that was wrong or used"
            }
        case .flashFailed: return "A flash could not be stored"
        case .resyncRequested:
            return event.device.map { "Asked \($0) to resend" } ?? "Asked every phone to resend"
        case .pairOffered: return "Pair code shown"
        case .unknown: return event.name.replacingOccurrences(of: "_", with: " ")
        }
    }

    /// How the connection travelled, in words. Nil when there is nothing worth saying.
    public static func route(_ path: String?) -> String? {
        switch path {
        case "direct": return "direct"
        case "relay": return "through the relay"
        case "relay then direct": return "relay, then direct"
        case "direct then relay": return "direct, then relay"
        default: return nil
        }
    }

    /// Counts the kinds of the items that arrived at or after a moment. Used to turn
    /// "3 items" into "3 dictations" when zone itself only gives a number.
    public static func kinds(of items: [ZoneItem], receivedSince moment: Date, slack: TimeInterval = 5) -> [String: Int] {
        let floor = moment.timeIntervalSince1970 - slack
        var counts: [String: Int] = [:]
        for item in items {
            guard let received = item.receivedAt, received >= floor else { continue }
            counts[item.kind, default: 0] += 1
        }
        return counts
    }
}

/// The short list of recent events shown to the person: newest first, no repeats.
public struct ActivityFeed: Equatable, Sendable {
    public private(set) var events: [ActivityEvent] = []
    public let limit: Int

    public init(limit: Int = 40) {
        self.limit = limit
    }

    /// Adds an event. Returns false when it was already there: the same event again, or
    /// the same happening heard a second way (once from zone.log and once from
    /// `zone events`, when the app changes from one to the other).
    @discardableResult
    public mutating func add(_ event: ActivityEvent) -> Bool {
        guard !events.contains(where: { $0.id == event.id || Self.sameHappening($0, event) }) else { return false }
        let index = events.firstIndex(where: { $0.date < event.date }) ?? events.endIndex
        events.insert(event, at: index)
        if events.count > limit {
            events.removeLast(events.count - limit)
        }
        return true
    }

    /// Two reports of one happening: one read from the log (whole seconds, no number) and
    /// one from `zone events`, of the same kind, about the same phone, at the same moment.
    static func sameHappening(_ a: ActivityEvent, _ b: ActivityEvent) -> Bool {
        guard (a.seq == nil) != (b.seq == nil), a.kind == b.kind else { return false }
        guard abs(a.date.timeIntervalSince(b.date)) < 1.5 else { return false }
        if let x = a.device, let y = b.device, x != y { return false }
        if let x = a.items, let y = b.items, x != y { return false }
        return true
    }

    public mutating func update(id: String, _ change: (inout ActivityEvent) -> Void) {
        guard let index = events.firstIndex(where: { $0.id == id }) else { return }
        change(&events[index])
    }

    /// The events a person would call news. A sync that brought nothing but clips is left
    /// out: each of those clips has its own line, which says more.
    public var news: [ActivityEvent] { events.filter { $0.isNews && !broughtOnlyClips($0) } }

    /// True when every item a sync brought is a clip that has its own event.
    func broughtOnlyClips(_ event: ActivityEvent) -> Bool {
        guard event.kind == .linkSession || event.kind == .flashReceived, let arrived = event.arrived, arrived > 0 else { return false }
        let clips = events.filter { clip in
            guard clip.kind == .clip, abs(clip.date.timeIntervalSince(event.date)) < 5 else { return false }
            if let a = clip.deviceID, let b = event.deviceID { return a == b }
            if let a = clip.device, let b = event.device { return a == b }
            return true
        }
        return clips.count == arrived
    }

    /// The highest event number seen, to ask zone for only what came after.
    public var lastSeq: Int? { events.compactMap(\.seq).max() }
}
