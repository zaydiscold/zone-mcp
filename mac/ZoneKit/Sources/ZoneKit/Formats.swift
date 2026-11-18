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

    /// Time left on a pair code: "9:41". Part of a second is not counted, so a code that
    /// lasts ten minutes starts at 10:00 and never shows 10:01.
    public static func countdown(_ seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds.rounded(.down)))
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

    /// How long something ran, in a few words: "38 sec", "42 min", "1 hr 5 min".
    public static func length(_ seconds: TimeInterval) -> String {
        let s = max(0, Int(seconds.rounded()))
        if s < 60 { return "\(s) sec" }
        let minutes = Int((Double(s) / 60).rounded())
        if minutes < 60 { return "\(minutes) min" }
        let rest = minutes % 60
        return rest == 0 ? "\(minutes / 60) hr" : "\(minutes / 60) hr \(rest) min"
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

    /// "v0.1.3" as people say it: "0.1.3".
    public static func plain(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        return trimmed.hasPrefix("v") || trimmed.hasPrefix("V") ? String(trimmed.dropFirst()) : trimmed
    }

    /// The app's version, once: "Zone 0.1.2". The zone program inside is the same version
    /// by design, so it is named only when it really is another one.
    ///
    /// `running` is the version of the zone that is running, when zone says (0.1.4 and
    /// later do). A login item can run another copy than the app's, and after an update the
    /// one running can be the one from before: then it is named too.
    public static func line(app: String, zone: String?, running: String? = nil) -> String {
        var line = "Zone \(plain(app))"
        if let zone, parts(zone) != parts(app) { line += ", with zone \(plain(zone)) inside" }
        if let running, !parts(running).isEmpty, parts(running) != parts(zone ?? app) { line += ". Running: zone \(plain(running))" }
        return line
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
