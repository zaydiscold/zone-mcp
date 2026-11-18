import Foundation

// What the window under the Z shows, worked out apart from the views so it can be tested:
// the one line about syncing, the list and its kinds, and the links into the history page.

/// The kinds the menu's list can be narrowed to.
public enum ListSegment: String, CaseIterable, Sendable, Identifiable {
    case all, note, meeting, dictation, clip

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .all: return "All"
        case .note: return "Notes"
        case .meeting: return "Meetings"
        case .dictation: return "Dictations"
        case .clip: return "Clips"
        }
    }

    /// The kind as zone names it. Nil for everything.
    public var kind: String? { self == .all ? nil : rawValue }

    /// The segments on offer. Clips get one when this zone has them.
    public static func shown(hasClips: Bool) -> [ListSegment] {
        hasClips ? allCases : allCases.filter { $0 != .clip }
    }

    /// How many items of this kind the library holds. A voice note is counted once: the
    /// recording behind it is not a dictation of its own here, as on the page.
    public func count(in items: StatusReport.Items) -> Int {
        guard let kind else { return max(0, items.total - items.recordingsBehindNotes) }
        let count = items.byKind[kind] ?? 0
        return kind == "dictation" ? max(0, count - items.recordingsBehindNotes) : count
    }
}

public enum MenuContent {
    /// The rows of the list. "All" leaves out what already has a place above the list (the
    /// clip that just arrived, the last meeting, the open notes), so nothing is shown twice
    /// at once. A list of one kind is that kind, whole.
    ///
    /// The recording behind a voice note is left out when the note is among `notes` (or the
    /// recent items): the note is there with the same words, as on the page.
    public static func rows(segment: ListSegment, recent: [ZoneItem], ofKind: [ZoneItem]?, shownAbove: Set<String>, notes: [ZoneItem] = [], limit: Int = 60) -> [ZoneItem] {
        let known = notes + recent.filter { $0.kind == "note" }
        guard let kind = segment.kind else {
            return Array(folded(recent, notes: known).lazy.filter { !shownAbove.contains($0.id) }.prefix(limit))
        }
        return Array(folded(ofKind ?? recent.filter { $0.kind == kind }, notes: known).prefix(limit))
    }

    /// True when `item` is the recording behind one of `notes`: a dictation whose source is
    /// `note`, with a note saved while it was recorded or in the two minutes after. The
    /// same rule as the page's (zone's `FOLDED_RECORDING`).
    public static func isRecording(_ item: ZoneItem, behindOneOf notes: [ZoneItem]) -> Bool {
        guard item.kind == "dictation", item.source == "note" else { return false }
        let from = item.createdAt - 5, to = item.createdAt + (item.duration ?? 0) + 120
        return notes.contains { $0.kind == "note" && $0.createdAt >= from && $0.createdAt <= to }
    }

    /// `items` without the recordings behind `notes`.
    public static func folded(_ items: [ZoneItem], notes: [ZoneItem]) -> [ZoneItem] {
        guard !notes.isEmpty, items.contains(where: { $0.source == "note" }) else { return items }
        return items.filter { !isRecording($0, behindOneOf: notes) }
    }

    /// A meeting's name. One recorded without a title is "Meeting".
    public static func meetingTitle(_ item: ZoneItem) -> String {
        let title = item.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return title.isEmpty ? "Meeting" : title
    }

    /// The words "Last meeting" stand before this in the popover, so a meeting without a
    /// title is "Untitled" there, not "Meeting" a second time.
    public static func lastMeetingTitle(_ item: ZoneItem) -> String {
        let title = item.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return title.isEmpty ? "Untitled" : title
    }

    /// The label over the open notes: "Open notes", or "Open notes, newest of 37" when more
    /// are open than are shown.
    public static func openNotesLabel(shown: Int, open: Int) -> String {
        open > shown ? "Open notes, newest of \(Formats.number(open))" : "Open notes"
    }

    /// When a meeting was and how long it ran: "Yesterday, 42 min".
    public static func meetingFacts(_ item: ZoneItem, now: Date = Date(), calendar: Calendar = .current) -> String {
        var parts = [Formats.when(item.created, now: now, calendar: calendar)]
        if let length = item.length { parts.append(Formats.length(length)) }
        return parts.joined(separator: ", ")
    }

    /// One line for a row: a meeting by its title, anything else by its words.
    public static func line(_ item: ZoneItem) -> String {
        if item.kind == "meeting" { return meetingTitle(item) }
        return item.preview
    }

    /// Which phone synced last and when, in one line: "Demo iPhone, synced 2 min ago".
    /// A sync that came another way says so: "Backup folder read 5 min ago".
    public static func syncLine(_ status: StatusReport) -> String {
        let phone = status.phones.filter { $0.lastSyncAge != nil }.min { ($0.lastSyncAge ?? 0) < ($1.lastSyncAge ?? 0) }
        var newest: (age: Double, text: String)?
        func offer(_ age: Double?, _ text: (Double) -> String) {
            guard let age, newest == nil || age < newest!.age - 1 else { return }
            newest = (age, text(age))
        }
        offer(phone?.lastSyncAge) { "\(phone?.name ?? "iPhone"), synced \(Formats.ago($0))" }
        offer(status.lastSync.flash?.age) { "Synced by Flash \(Formats.ago($0))" }
        offer(status.lastSync.backupFolder?.age) { "Backup folder read \(Formats.ago($0))" }
        // zone can know of a sync without naming the phone (an older zone).
        offer(status.lastSync.link?.age) { "Synced \(Formats.ago($0))" }
        if let newest { return newest.text }
        if let first = status.phones.first {
            return status.phones.count == 1 ? "\(first.name), waiting for its first sync" : "Waiting for the first sync"
        }
        return "Ready to pair"
    }

    /// What the one line leaves out, for whoever asks: each phone (the one in the line is
    /// not named again when it is the only one), how the last sync travelled, and what the
    /// library holds.
    public static func syncDetails(_ status: StatusReport, today: Int) -> [String] {
        var lines: [String] = []
        let single = status.phones.count == 1
        for phone in status.phones {
            let items = "\(Formats.number(phone.itemsReceived)) \(phone.itemsReceived == 1 ? "item" : "items")"
            var parts: [String] = []
            if !single {
                parts.append(phone.lastSyncAge.map { "\(phone.name): synced \(Formats.ago($0))" } ?? "\(phone.name): waiting for its first sync")
                parts.append(items)
            } else {
                parts.append("\(items) from \(phone.name)")
            }
            if let route = routeWords(phone.lastPath) { parts.append(route) }
            lines.append(parts.joined(separator: ", "))
        }
        if status.daemon.relayConnected == false {
            lines.append("Reachable on this network only, while zone reconnects to the relay")
        }
        lines.append("\(Formats.number(status.items.total)) in your library, \(Formats.number(today)) today")
        return lines
    }

    /// How a phone's last sync travelled, for someone who has never heard of a relay.
    public static func routeWords(_ path: String?) -> String? {
        switch path {
        case "direct": return "straight over this network"
        case "relay": return "through Zone's relay"
        case "relay then direct": return "through the relay, then straight"
        case "direct then relay": return "straight, then through the relay"
        default: return nil
        }
    }
}

/// A place in zone's history page that the app can open directly.
public enum PageRoute: Equatable, Sendable {
    /// The page as it opens by itself.
    case home
    case meeting(String)
    case notes
    case settings
}

/// The links into the history page. The page reads the part after `#`; the key that
/// unlocks it stays in the query, as `zone open --print` gives it. The routes are the ones
/// the page documents in `docs/page-routes.md`.
public enum PageRoutes {
    /// The part after `#`. Nil for the page as it opens by itself.
    public static func fragment(_ route: PageRoute) -> String? {
        switch route {
        case .home: return nil
        case let .meeting(id):
            var allowed = CharacterSet.alphanumerics
            allowed.insert(charactersIn: "-._~")
            return "/meeting/" + (id.addingPercentEncoding(withAllowedCharacters: allowed) ?? id)
        case .notes: return "/notes"
        case .settings: return "/settings"
        }
    }

    /// The page's own link, pointed at a route.
    public static func url(page: URL, route: PageRoute) -> URL {
        guard var parts = URLComponents(url: page, resolvingAgainstBaseURL: false) else { return page }
        parts.percentEncodedFragment = fragment(route)
        return parts.url ?? page
    }
}
