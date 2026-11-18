import Foundation

/// `zone mcp status --json`: every place where an AI client keeps its list of MCP servers,
/// and whether zone is in it.
public struct AgentSurvey: Decodable, Equatable, Sendable {
    public struct Place: Decodable, Equatable, Sendable {
        /// The client's short name, as `zone mcp install` takes it: "claude-code".
        public var client: String
        /// For people: "Claude Code (default)".
        public var label: String
        public var profile: String?
        public var path: String
        /// True when the client is on this computer.
        public var installed: Bool
        /// "registered", "registered_read_only", "other_copy", "not_registered",
        /// "removed_by_you" or "unreadable".
        public var state: String
        public var detail: String?
        /// True when the agent there cannot mark items as handled.
        public var readOnly: Bool
        /// What the agent there may see. Nil when this zone is too old to say (it then
        /// cannot limit a view either).
        public var scope: Scope?

        public init(client: String, label: String, profile: String? = nil, path: String = "", installed: Bool = true, state: String, detail: String? = nil, readOnly: Bool = false, scope: Scope? = nil) {
            self.client = client
            self.label = label
            self.profile = profile
            self.path = path
            self.installed = installed
            self.state = state
            self.detail = detail
            self.readOnly = readOnly
            self.scope = scope
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            client = try c.decode(String.self, forKey: .client)
            label = try c.decodeIfPresent(String.self, forKey: .label) ?? client
            profile = try c.decodeIfPresent(String.self, forKey: .profile)
            path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
            installed = try c.decodeIfPresent(Bool.self, forKey: .installed) ?? false
            state = try c.decodeIfPresent(String.self, forKey: .state) ?? "not_registered"
            detail = try c.decodeIfPresent(String.self, forKey: .detail)
            readOnly = try c.decodeIfPresent(Bool.self, forKey: .readOnly) ?? (state == "registered_read_only")
            scope = try c.decodeIfPresent(Scope.self, forKey: .scope)
        }

        enum CodingKeys: String, CodingKey {
            case client, label, profile, path, installed, state, detail, scope
            case readOnly = "read_only"
        }

        var isRegistered: Bool { state == "registered" || state == "registered_read_only" }

        /// How zone stands in this one place, exactly as `zone mcp status` says it.
        public enum Standing: Equatable, Sendable {
            /// zone is registered here and it is the zone that was asked.
            case registered
            /// zone is registered here, but the entry starts a different copy of the program.
            case otherCopy(String)
            /// zone was here and the person took it out, so nothing puts it back by itself.
            case removedByYou
            case notRegistered
            /// The settings file could not be read.
            case unreadable(String)
        }

        public var standing: Standing {
            switch state {
            case "registered", "registered_read_only": return .registered
            case "other_copy": return .otherCopy(detail ?? "")
            case "removed_by_you": return .removedByYou
            case "unreadable": return .unreadable(detail ?? "")
            default: return .notRegistered
            }
        }

        /// True when an agent started from this place gets a zone (whichever copy).
        public var hasZone: Bool { isRegistered || state == "other_copy" }

        /// The place's own name inside its app: "default", "~/.claude_work", "profile hooke".
        /// An app with one place has none.
        public var name: String { profile ?? label }

        /// The standing in a few words, for a line under the place's name.
        public func words(home: String = "") -> String {
            switch standing {
            case .registered: return "Connected"
            case let .otherCopy(program):
                return program.isEmpty ? "Starts another copy of zone" : "Starts another copy of zone: \(Formats.tilde(program, home: home))"
            case .removedByYou: return "Removed by you"
            case .notRegistered: return "Not connected"
            case let .unreadable(why): return why.isEmpty ? "Its settings could not be read" : "Its settings could not be read: \(why)"
            }
        }

        /// What an agent here may read, in a few words. Nil when it has no zone, or when
        /// this zone is too old to say.
        public func limit(knowsClips: Bool) -> String? {
            guard hasZone, let scope else { return nil }
            let reads = "reads \(AgentView(scope, knowsClips: knowsClips).words)"
            return readOnly ? reads + ", cannot mark items" : reads
        }
    }

    /// What one registered agent may see: which kinds of item, and which dictations are
    /// hidden by where they were spoken ("keyboard" is what was typed by voice in other apps).
    public struct Scope: Decodable, Equatable, Sendable {
        public var kinds: [String]
        public var excludeSources: [String]
        /// False when zone itself says this is the whole library. Nil when it does not say.
        public var limited: Bool?

        public init(kinds: [String] = AgentView.allKinds, excludeSources: [String] = [], limited: Bool? = nil) {
            self.kinds = kinds
            self.excludeSources = excludeSources
            self.limited = limited
        }

        enum CodingKeys: String, CodingKey {
            case kinds, limited
            case excludeSources = "exclude_sources"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            kinds = try c.decodeIfPresent([String].self, forKey: .kinds) ?? AgentView.allKinds
            excludeSources = try c.decodeIfPresent([String].self, forKey: .excludeSources) ?? []
            limited = try c.decodeIfPresent(Bool.self, forKey: .limited)
        }
    }

    /// The zone program these answers are about.
    public var zone: String?
    public var clients: [Place]

    public init(zone: String? = nil, clients: [Place]) {
        self.zone = zone
        self.clients = clients
    }

    enum CodingKeys: String, CodingKey { case zone, clients }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        zone = try c.decodeIfPresent(String.self, forKey: .zone)
        clients = try c.decodeIfPresent([Place].self, forKey: .clients) ?? []
    }
}

/// What an agent may read. The app offers three views. Anything else was set from the
/// command line and is shown as it is, never changed by accident.
public enum AgentView: Equatable, Sendable {
    /// The whole library.
    case everything
    /// Voice notes and meeting transcripts. Nothing that was dictated.
    case notesAndMeetings
    /// Everything except what was typed by voice into other apps (messages, emails, searches).
    case noKeyboard
    /// Some other limit, set with `zone mcp install --kinds ... --exclude-sources ...`.
    /// No kinds means every kind.
    case custom(kinds: [String], excludeSources: [String])

    /// The kinds of thing a person says. A zone that has clips (text sent from the phone
    /// on purpose) has a fourth kind, "clip".
    public static let allKinds = ["dictation", "note", "meeting"]
    /// The views the app offers, in the order they are listed.
    public static let choices: [AgentView] = [.everything, .notesAndMeetings, .noKeyboard]

    /// Reads a view as zone reports it. `knowsClips` says whether this zone has clips: for
    /// one that does, "dictation, note, meeting" is a limit (clips stay out), and it is
    /// kept as that limit, never read as everything.
    public init(_ scope: AgentSurvey.Scope, knowsClips: Bool = false) {
        let given = scope.kinds.map { $0.lowercased() }
        let hidden = scope.excludeSources.sorted()
        var universe = Self.allKinds
        // Three ways to know this zone has clips: the app asked it, an entry names them, or
        // zone calls the three older kinds alone a limit.
        let threeIsALimit = scope.limited == true && hidden.isEmpty && Set(given) == Set(Self.allKinds)
        if knowsClips || given.contains("clip") || threeIsALimit {
            universe.append("clip")
        }
        // In zone's order, and a kind this app has never heard of is kept, not dropped.
        let kinds = universe.filter(given.contains) + given.filter { !universe.contains($0) }
        let all = scope.limited == false || Set(kinds).isSuperset(of: universe)
        switch (all, kinds, hidden) {
        case (true, _, []): self = .everything
        case (true, _, ["keyboard"]): self = .noKeyboard
        case (false, ["note", "meeting"], []): self = .notesAndMeetings
        default: self = .custom(kinds: all ? [] : kinds, excludeSources: hidden)
        }
    }

    public var kinds: [String] {
        switch self {
        case .everything, .noKeyboard: return []
        case .notesAndMeetings: return ["note", "meeting"]
        case let .custom(kinds, _): return kinds
        }
    }

    public var excludeSources: [String] {
        switch self {
        case .everything, .notesAndMeetings: return []
        case .noKeyboard: return ["keyboard"]
        case let .custom(_, hidden): return hidden
        }
    }

    public var isLimited: Bool { self != .everything }

    /// The flags that give `zone mcp install` exactly this view. Both are always given:
    /// installing again without them would quietly go back to everything. A list of kinds
    /// is repeated as it is, never rounded up to "all".
    public var arguments: [String] {
        [
            "--kinds", kinds.isEmpty ? "all" : kinds.joined(separator: ","),
            "--exclude-sources", excludeSources.isEmpty ? "none" : excludeSources.joined(separator: ","),
        ]
    }

    /// The name of the choice in a list.
    public var title: String {
        switch self {
        case .everything: return "Everything"
        case .notesAndMeetings: return "Notes and meetings only"
        case .noKeyboard: return "All but keyboard dictations"
        case .custom: return "A view set from the command line"
        }
    }

    /// One line under the name of the choice.
    public var detail: String {
        switch self {
        case .everything: return "Your whole library."
        case .notesAndMeetings: return "Nothing you dictated, and no clips."
        case .noKeyboard: return "Hides what you typed by voice into other apps."
        case .custom: return words.prefix(1).uppercased() + words.dropFirst() + "."
        }
    }

    /// What the agent reads, to follow "Reads": "everything", "notes and meetings only".
    public var words: String {
        switch self {
        case .everything: return "everything"
        case .notesAndMeetings: return "notes and meetings only"
        case .noKeyboard: return "all but keyboard dictations"
        case let .custom(kinds, hidden):
            var parts: [String] = []
            if !kinds.isEmpty {
                parts.append(Self.list(kinds.map { $0 + "s" }) + " only")
            }
            if !hidden.isEmpty {
                parts.append("no \(Self.list(hidden)) dictations")
            }
            return parts.isEmpty ? "everything" : parts.joined(separator: ", ")
        }
    }

    private static func list(_ parts: [String]) -> String {
        guard parts.count > 1, let last = parts.last else { return parts.first ?? "" }
        return parts.dropLast().joined(separator: ", ") + " and " + last
    }
}

/// One AI client as the app shows it: a name and a switch. A client can keep its settings
/// in several places (Claude Code has one per profile), and one switch covers them all.
public struct Agent: Equatable, Sendable, Identifiable {
    public enum Standing: Equatable, Sendable {
        /// zone is in every place this client has.
        case connected
        /// zone is in some places but not all: `places` of `total`.
        case partly(places: Int, total: Int)
        /// The client starts a different copy of zone (one installed from the command line).
        case otherCopy(String)
        case notConnected
        /// The client's settings could not be read.
        case unreadable(String)
    }

    /// The name `zone mcp install` takes.
    public var id: String
    public var title: String
    public var installed: Bool
    public var standing: Standing
    public var places: [AgentSurvey.Place]
    /// True when this zone has clips, which changes how a list of kinds is read.
    public var knowsClips = false

    /// The places where zone is (whichever copy of it).
    public var connectedPlaces: [AgentSurvey.Place] {
        places.filter(\.hasZone)
    }

    /// The places whose entry starts a different copy of zone than the one that was asked.
    public var otherCopyPlaces: [AgentSurvey.Place] {
        places.filter { $0.state == "other_copy" }
    }

    /// The other copy those places start, when they agree on one.
    public var otherCopyProgram: String? {
        let programs = Set(otherCopyPlaces.compactMap(\.detail).filter { !$0.isEmpty })
        return programs.count == 1 ? programs.first : nil
    }

    /// True when the person took zone out of every place this app has (and it is in none).
    public var removedByYou: Bool {
        !isOn && !places.isEmpty && places.contains { $0.state == "removed_by_you" }
    }

    /// What this agent may read. Nil when it is not connected, when this zone cannot say,
    /// or when its profiles do not agree (see `viewsDiffer`).
    public var view: AgentView? {
        let views = connectedPlaces.compactMap { place in place.scope.map { AgentView($0, knowsClips: knowsClips) } }
        guard let first = views.first, views.count == connectedPlaces.count, views.allSatisfy({ $0 == first }) else { return nil }
        return first
    }

    /// True when the profiles of this client were given different views.
    public var viewsDiffer: Bool {
        let views = connectedPlaces.compactMap { place in place.scope.map { AgentView($0, knowsClips: knowsClips) } }
        return views.count > 1 && !views.allSatisfy { $0 == views[0] }
    }

    /// True when this zone can limit what an agent reads.
    public var canLimitView: Bool {
        isOn && !connectedPlaces.isEmpty && connectedPlaces.allSatisfy { $0.scope != nil }
    }

    /// True when the agent cannot mark items as handled (set from the command line).
    public var readOnly: Bool {
        !connectedPlaces.isEmpty && connectedPlaces.allSatisfy(\.readOnly)
    }

    /// The flags that connect this agent again exactly as it is: the same view, the same
    /// read-only choice. A plain `zone mcp install` would reset both.
    public func installArguments(view newView: AgentView? = nil) -> [String] {
        var arguments = ["mcp", "install", id]
        if readOnly { arguments.append("--read-only") }
        if let view = newView ?? view, connectedPlaces.isEmpty || canLimitView {
            arguments += view.arguments
        }
        return arguments
    }

    public var isOn: Bool {
        switch standing {
        case .connected, .partly, .otherCopy: return true
        case .notConnected, .unreadable: return false
        }
    }

    /// A few words under the name: where it stands and what it may read. An app with
    /// several profiles says how many, so seven profiles are one line, not seven.
    public var note: String {
        let count = places.count
        let profiles = count > 1 ? "\(count) profiles" : nil
        func joined(_ parts: String?...) -> String {
            let said = parts.compactMap { $0 }
            guard let first = said.first else { return "" }
            return ([first.prefix(1).uppercased() + first.dropFirst()] + said.dropFirst()).joined(separator: ", ")
        }
        switch standing {
        case .connected:
            if viewsDiffer { return joined(profiles, "they read different things") }
            if let view {
                return joined(profiles, "reads \(view.words)", readOnly ? "cannot mark items" : nil)
            }
            return count > 1 ? "Connected in all \(count) profiles" : "Connected"
        case let .partly(places, total):
            let other = otherCopyPlaces.count
            let base = "Connected in \(places) of \(total) profiles"
            return other > 0 ? base + ", \(other) with another copy of zone" : base
        case .otherCopy:
            let other = otherCopyPlaces.count
            if count > 1, other < count { return "\(other) of \(count) profiles start another copy of zone" }
            return joined(profiles, "starts another copy of zone")
        case .notConnected:
            if !installed { return "Not on this Mac" }
            return removedByYou ? "Off, removed by you" : "Not connected"
        case .unreadable:
            return "Its settings could not be read"
        }
    }

    /// An entry that starts another copy of zone: what that means, in two sentences.
    /// (The zone that runs on this Mac is named once, at the foot of the panel.)
    public func otherCopyExplanation(home: String = "") -> String {
        let there = otherCopyProgram.map { " (\(Formats.tilde($0, home: home)))" } ?? ""
        let who = otherCopyPlaces.count < places.count ? "\(otherCopyPlaces.count) of its profiles start" : "\(title) starts"
        return "\(who) another copy of zone\(there), not the one that runs on this Mac. It reads the same library, but is updated on its own and can fall behind."
    }

    /// What "Use this one" does to what the agent may read: kept, when zone can say what
    /// the other entry allows. An older zone cannot, and then the agent reads everything.
    public var otherCopyKeepsView: Bool {
        !otherCopyPlaces.contains { $0.scope == nil }
    }

    /// The clients on this computer (and any other that has zone), in zone's own order.
    public static func list(from survey: AgentSurvey, knowsClips: Bool = false) -> [Agent] {
        var order: [String] = []
        var grouped: [String: [AgentSurvey.Place]] = [:]
        for place in survey.clients {
            if grouped[place.client] == nil { order.append(place.client) }
            grouped[place.client, default: []].append(place)
        }
        return order.compactMap { name -> Agent? in
            let places = grouped[name] ?? []
            let installed = places.contains(where: \.installed)
            let registered = places.filter(\.isRegistered).count
            let other = places.first(where: { $0.state == "other_copy" })
            let withZone = places.filter(\.hasZone).count
            let unreadable = places.first(where: { $0.state == "unreadable" })
            let standing: Standing
            if registered == places.count, registered > 0 {
                standing = .connected
            } else if let other, withZone == places.count {
                // In every place, and at least one of them starts another copy.
                standing = .otherCopy(other.detail ?? "")
            } else if withZone > 0 {
                standing = .partly(places: withZone, total: places.count)
            } else if let unreadable {
                standing = .unreadable(unreadable.detail ?? "")
            } else {
                standing = .notConnected
            }
            let agent = Agent(id: name, title: title(for: places), installed: installed, standing: standing, places: places, knowsClips: knowsClips)
            // A client that is not on this computer and has no zone entry is not worth a row.
            return installed || agent.isOn ? agent : nil
        }
    }

    /// "Claude Code (default)" becomes "Claude Code". A label without a profile is kept.
    static func title(for places: [AgentSurvey.Place]) -> String {
        guard let first = places.first else { return "" }
        if first.profile != nil, let cut = first.label.range(of: " (", options: .backwards) {
            return String(first.label[..<cut.lowerBound])
        }
        return first.label
    }
}

/// The agents as the panel lists them: the ones that have zone (or whose settings need a
/// look) first, each app once however many profiles it has, and the rest folded away under
/// one line, so twenty apps that are off do not bury the five that are on.
public struct AgentSections: Equatable, Sendable {
    public var on: [Agent]
    public var off: [Agent]

    public init(_ agents: [Agent]) {
        func listed(_ agent: Agent) -> Bool {
            if case .unreadable = agent.standing { return true }
            return agent.isOn
        }
        on = agents.filter(listed)
        off = agents.filter { !listed($0) }
    }

    /// "5 connected, 16 profiles" style counts for the top of the panel.
    public var summary: String {
        let connected = on.filter(\.isOn)
        guard !connected.isEmpty else { return "None connected yet" }
        let places = connected.reduce(0) { $0 + $1.connectedPlaces.count }
        let apps = "\(connected.count) \(connected.count == 1 ? "app" : "apps") connected"
        return places > connected.count ? "\(apps), \(places) profiles in all" : apps
    }

    /// The one line the apps that are off fold under.
    public var offTitle: String {
        let removed = off.filter(\.removedByYou).count
        let count = "\(off.count) not connected"
        if removed == off.count, removed > 0 { return count + ", removed by you" }
        return removed > 0 ? count + " (\(removed) removed by you)" : count
    }
}
