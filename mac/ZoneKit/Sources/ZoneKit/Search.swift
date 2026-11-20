import Foundation
import Observation

// Searching the library from the menu as the person types: a short wait after each key, the
// search before it given up, and the words zone matched picked out of its snippet.

/// A snippet from `zone search --json`, cut into plain text and the words that matched.
/// zone wraps every matched word in square brackets: "the [lease] on Wednesday".
public enum SearchSnippet {
    public struct Part: Equatable, Sendable {
        public var text: String
        public var matched: Bool

        public init(_ text: String, matched: Bool) {
            self.text = text
            self.matched = matched
        }
    }

    /// The snippet in parts, on one line. A bracket that is never closed is kept as text, so a
    /// "[" the person said is not lost.
    public static func parts(_ snippet: String) -> [Part] {
        let flat = snippet.split(whereSeparator: \.isNewline).joined(separator: " ")
        var parts: [Part] = []
        var rest = Substring(flat)
        func add(_ text: Substring, _ matched: Bool) {
            guard !text.isEmpty else { return }
            if let last = parts.last, last.matched == matched {
                parts[parts.count - 1].text += text
            } else {
                parts.append(Part(String(text), matched: matched))
            }
        }
        while let open = rest.firstIndex(of: "[") {
            guard let close = rest[open...].firstIndex(of: "]") else { break }
            add(rest[..<open], false)
            add(rest[rest.index(after: open)..<close], true)
            rest = rest[rest.index(after: close)...]
        }
        add(rest, false)
        return parts
    }

    /// The parts with the text before the first match cut to its last `lead` characters (at a
    /// word), so the matched word is on screen in a row of one line.
    public static func leading(_ parts: [Part], lead: Int = 24) -> [Part] {
        guard let first = parts.first, !first.matched, parts.count > 1, first.text.count > lead else { return parts }
        let tail = first.text.suffix(lead)
        // A word cut in half is dropped, unless the cut fell between words.
        let atWord = first.text.dropLast(lead).last == " "
        let cut = atWord ? tail : tail.firstIndex(of: " ").map { tail[tail.index(after: $0)...] } ?? tail
        var trimmed = parts
        trimmed[0].text = "\u{2026}" + cut
        return trimmed
    }

    /// The words that matched, each once, in the order they first appear: "lease, Wednesday".
    public static func matchedWords(_ snippet: String) -> [String] {
        var seen = Set<String>()
        return parts(snippet).filter(\.matched).map(\.text).filter { seen.insert($0.lowercased()).inserted }
    }
}

/// The search in the menu. Each change of the words waits a moment (typing usually goes on),
/// then asks zone. A newer change gives up the older search, and an answer that comes back
/// for words that are no longer in the field is thrown away.
@MainActor
@Observable
public final class LiveSearch {
    public enum Phase: Equatable, Sendable {
        /// The field is empty: the menu shows what it always shows.
        case idle
        /// Waiting for the typing to stop, or for zone to answer.
        case searching
        case found(ItemList)
        case failed(String)
    }

    public private(set) var query = ""
    public private(set) var phase: Phase = .idle
    /// The last answer, kept while the next words are searched so the list does not blink
    /// empty on every key. Nil when the field is empty.
    public private(set) var results: ItemList?
    /// How many times zone was actually asked. For tests and nothing else.
    @ObservationIgnored public private(set) var asked = 0

    @ObservationIgnored private let delay: Duration
    @ObservationIgnored private let search: @Sendable (String) async throws -> ItemList
    @ObservationIgnored private var task: Task<Void, Never>?

    public init(delay: Duration = .milliseconds(160), search: @escaping @Sendable (String) async throws -> ItemList) {
        self.delay = delay
        self.search = search
    }

    /// The words that are searched: the field without spaces around it.
    public var words: String { query.trimmingCharacters(in: .whitespacesAndNewlines) }

    public var isActive: Bool { !words.isEmpty }

    /// The items found, for these words or (while those are searched) the ones before.
    public var items: [ZoneItem] { results?.items ?? [] }

    public func update(_ text: String) {
        guard text != query else { return }
        query = text
        task?.cancel()
        let words = self.words
        guard !words.isEmpty else {
            task = nil
            phase = .idle
            results = nil
            return
        }
        phase = .searching
        let delay = self.delay, search = self.search
        task = Task { [weak self] in
            do {
                try await Task.sleep(for: delay)
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            self?.asked += 1
            let result: Result<ItemList, Error>
            do {
                result = .success(try await search(words))
            } catch {
                result = .failure(error)
            }
            guard let self, !Task.isCancelled, self.words == words else { return }
            switch result {
            case let .success(list):
                self.phase = .found(list)
                self.results = list
            case let .failure(error): self.phase = .failed(error.localizedDescription)
            }
        }
    }

    public func clear() {
        update("")
    }

    /// Waits for the search under way, for tests.
    public func settle() async {
        await task?.value
    }
}

/// The row the arrow keys are on, in a list that can change under it.
public struct ListCursor: Equatable, Sendable {
    public private(set) var index: Int?

    public init(index: Int? = nil) {
        self.index = index
    }

    /// Down from nothing is the first row, up from the first stays there. A list that is
    /// empty has no row.
    public mutating func move(_ step: Int, count: Int) {
        guard count > 0 else {
            index = nil
            return
        }
        guard let index else {
            self.index = step > 0 ? 0 : count - 1
            return
        }
        self.index = min(max(index + step, 0), count - 1)
    }

    /// A new list: the first row, when there is one.
    public mutating func reset(count: Int) {
        index = count > 0 ? 0 : nil
    }

    /// The same list, shorter or longer: keep the row, or the last one.
    public mutating func clamp(count: Int) {
        guard let index else { return }
        self.index = count > 0 ? min(index, count - 1) : nil
    }
}
