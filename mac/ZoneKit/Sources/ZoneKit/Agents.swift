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
    }

    /// What one registered agent may see: which kinds of item, and which dictations are
    /// hidden by where they were spoken ("keyboard" is what was typed by voice in other apps).
    public struct Scope: Decodable, Equatable, Sendable {
        public var kinds: [String]
        public var excludeSources: [String]

        public init(kinds: [String] = AgentView.allKinds, excludeSources: [String] = []) {
            self.kinds = kinds
            self.excludeSources = excludeSources
        }

        enum CodingKeys: String, CodingKey {
            case kinds
            case excludeSources = "exclude_sources"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            kinds = try c.decodeIfPresent([String].self, forKey: .kinds) ?? AgentView.allKinds
            excludeSources = try c.decodeIfPresent([String].self, forKey: .excludeSources) ?? []
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
    case custom(kinds: [String], excludeSources: [String])

    public static let allKinds = ["dictation", "note", "meeting"]
    /// The views the app offers, in the order they are listed.
    public static let choices: [AgentView] = [.everything, .notesAndMeetings, .noKeyboard]

    public init(_ scope: AgentSurvey.Scope) {
        let kinds = Self.allKinds.filter(scope.kinds.contains)
        let hidden = scope.excludeSources.sorted()
        switch (kinds, hidden) {
        case (Self.allKinds, []): self = .everything
        case (["note", "meeting"], []): self = .notesAndMeetings
        case (Self.allKinds, ["keyboard"]): self = .noKeyboard
        default: self = .custom(kinds: kinds, excludeSources: hidden)
        }
    }

    public var kinds: [String] {
        switch self {
        case .everything, .noKeyboard: return Self.allKinds
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
    /// installing again without them would quietly go back to everything.
    public var arguments: [String] {
        [
            "--kinds", kinds == Self.allKinds || kinds.isEmpty ? "all" : kinds.joined(separator: ","),
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
        case .everything: return "Dictations, voice notes and meetings."
        case .notesAndMeetings: return "Nothing you dictated, in any app."
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
            if kinds != Self.allKinds, !kinds.isEmpty {
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

    /// The places where zone is (whichever copy of it).
    var connectedPlaces: [AgentSurvey.Place] {
        places.filter { $0.isRegistered || $0.state == "other_copy" }
    }

    /// What this agent may read. Nil when it is not connected, when this zone cannot say,
    /// or when its profiles do not agree (see `viewsDiffer`).
    public var view: AgentView? {
        let views = connectedPlaces.compactMap { $0.scope.map(AgentView.init) }
        guard let first = views.first, views.count == connectedPlaces.count, views.allSatisfy({ $0 == first }) else { return nil }
        return first
    }

    /// True when the profiles of this client were given different views.
    public var viewsDiffer: Bool {
        let views = connectedPlaces.compactMap { $0.scope.map(AgentView.init) }
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

    /// A few words under the name.
    public var note: String {
        switch standing {
        case .connected:
            if viewsDiffer { return "Its profiles read different things" }
            if let view {
                let reads = "Reads \(view.words)"
                return readOnly ? reads + ", cannot mark items" : reads
            }
            return places.count > 1 ? "Connected in all \(places.count) profiles" : "Connected"
        case let .partly(places, total):
            return "Connected in \(places) of \(total) profiles"
        case .otherCopy:
            return "Connected to another copy of zone"
        case .notConnected:
            return installed ? "Not connected" : "Not on this Mac"
        case .unreadable:
            return "Its settings could not be read"
        }
    }

    /// The clients on this computer (and any other that has zone), in zone's own order.
    public static func list(from survey: AgentSurvey) -> [Agent] {
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
            let unreadable = places.first(where: { $0.state == "unreadable" })
            let standing: Standing
            if registered == places.count, registered > 0 {
                standing = .connected
            } else if registered > 0 {
                standing = .partly(places: registered, total: places.count)
            } else if let other {
                standing = .otherCopy(other.detail ?? "")
            } else if let unreadable {
                standing = .unreadable(unreadable.detail ?? "")
            } else {
                standing = .notConnected
            }
            let agent = Agent(id: name, title: title(for: places), installed: installed, standing: standing, places: places)
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
