import Foundation

/// Numbers and times as a person reads them.
public enum Formats {
    /// 1764 becomes "1,764".
    public static func number(_ value: Int) -> String {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        formatter.locale = Locale(identifier: "en_US")
        return formatter.string(from: NSNumber(value: value)) ?? String(value)
    }

    /// How long ago, in a few words: "just now", "5 min ago", "2 hr ago", "3 days ago".
    public static func ago(_ seconds: TimeInterval) -> String {
        let s = max(0, seconds)
        if s < 20 { return "just now" }
        if s < 60 { return "\(Int(s)) sec ago" }
        if s < 3600 { return "\(Int(s / 60)) min ago" }
        if s < 86400 { return "\(Int(s / 3600)) hr ago" }
        let days = Int(s / 86400)
        return days == 1 ? "yesterday" : "\(days) days ago"
    }

    public static func ago(_ date: Date, now: Date = Date()) -> String {
        ago(now.timeIntervalSince(date))
    }

    /// Time left on a pair code: "9:41".
    public static func countdown(_ seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds.rounded(.up)))
        return String(format: "%d:%02d", s / 60, s % 60)
    }

    /// The time of day an item was said, or the day when it was not today: "9:12 AM",
    /// "Yesterday", "Nov 11".
    public static func when(_ date: Date, now: Date = Date(), calendar: Calendar = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US")
        formatter.timeZone = calendar.timeZone
        if calendar.isDate(date, inSameDayAs: now) {
            formatter.dateFormat = "h:mm a"
            return formatter.string(from: date)
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) {
            return "Yesterday"
        }
        formatter.dateFormat = calendar.component(.year, from: date) == calendar.component(.year, from: now) ? "MMM d" : "MMM d, yyyy"
        return formatter.string(from: date)
    }

    /// A folder as a person writes it: under the home folder it starts with ~.
    public static func tilde(_ path: String, home: String) -> String {
        guard !home.isEmpty, path.hasPrefix(home) else { return path }
        let rest = path.dropFirst(home.count)
        if rest.isEmpty { return "~" }
        return rest.hasPrefix("/") ? "~" + rest : path
    }
}

/// Compares version numbers like "0.1.0" and "v0.2".
public enum Versions {
    public static func parts(_ text: String) -> [Int] {
        let trimmed = text.trimmingCharacters(in: .whitespaces).drop { !$0.isNumber }
        let core = trimmed.prefix { $0.isNumber || $0 == "." }
        return core.split(separator: ".").map { Int($0) ?? 0 }
    }

    /// True when `candidate` is a later version than `current`.
    public static func isNewer(_ candidate: String, than current: String) -> Bool {
        let a = parts(candidate), b = parts(current)
        guard !a.isEmpty else { return false }
        for index in 0..<max(a.count, b.count) {
            let x = index < a.count ? a[index] : 0
            let y = index < b.count ? b[index] : 0
            if x != y { return x > y }
        }
        return false
    }
}
