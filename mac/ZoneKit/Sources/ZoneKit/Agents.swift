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

        public init(client: String, label: String, profile: String? = nil, path: String = "", installed: Bool = true, state: String, detail: String? = nil) {
            self.client = client
            self.label = label
            self.profile = profile
            self.path = path
            self.installed = installed
            self.state = state
            self.detail = detail
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
        }

        enum CodingKeys: String, CodingKey { case client, label, profile, path, installed, state, detail }

        var isRegistered: Bool { state == "registered" || state == "registered_read_only" }
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
