import Foundation

// What the `zone` program prints with --json, as Swift values. Every field that zone might
// leave out is optional or has a default, so a newer zone with more fields still reads.

/// `zone status --json`.
public struct StatusReport: Decodable, Equatable, Sendable {
    public struct Computer: Decodable, Equatable, Sendable {
        public var name: String
        public var zoneVersion: String?

        enum CodingKeys: String, CodingKey {
            case name
            case zoneVersion = "zone_version"
        }
    }

    public struct Phone: Decodable, Equatable, Sendable, Identifiable {
        public var name: String
        public var app: String?
        public var endpointID: String
        public var lastSync: String?
        /// Seconds since this phone last sent anything. Nil when it never has.
        public var lastSyncAge: Double?
        public var itemsReceived: Int
        /// How the last connection travelled: "direct", "relay", "relay then direct".
        public var lastPath: String?

        public var id: String { endpointID }

        enum CodingKeys: String, CodingKey {
            case name, app
            case endpointID = "endpoint_id"
            case lastSync = "last_sync"
            case lastSyncAge = "last_sync_age_s"
            case itemsReceived = "items_received"
            case lastPath = "last_path"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? "iPhone"
            app = try c.decodeIfPresent(String.self, forKey: .app)
            endpointID = try c.decodeIfPresent(String.self, forKey: .endpointID) ?? name
            lastSync = try c.decodeIfPresent(String.self, forKey: .lastSync)
            lastSyncAge = try c.decodeIfPresent(Double.self, forKey: .lastSyncAge)
            itemsReceived = try c.decodeIfPresent(Int.self, forKey: .itemsReceived) ?? 0
            lastPath = try c.decodeIfPresent(String.self, forKey: .lastPath)
        }

        public init(name: String, endpointID: String, lastSyncAge: Double? = nil, itemsReceived: Int = 0, lastPath: String? = nil) {
            self.name = name
            self.endpointID = endpointID
            self.lastSyncAge = lastSyncAge
            self.itemsReceived = itemsReceived
            self.lastPath = lastPath
        }
    }

    public struct SyncTime: Decodable, Equatable, Sendable {
        public var time: String?
        public var age: Double?

        enum CodingKeys: String, CodingKey {
            case time
            case age = "age_s"
        }
    }

    public struct LastSync: Decodable, Equatable, Sendable {
        public var link: SyncTime?
        public var flash: SyncTime?
        public var backupFolder: SyncTime?

        enum CodingKeys: String, CodingKey {
            case link, flash
            case backupFolder = "backup_folder"
        }

        public init(link: SyncTime? = nil, flash: SyncTime? = nil, backupFolder: SyncTime? = nil) {
            self.link = link
            self.flash = flash
            self.backupFolder = backupFolder
        }
    }

    public struct Items: Decodable, Equatable, Sendable {
        public var total: Int
        public var byKind: [String: Int]

        enum CodingKeys: String, CodingKey {
            case total
            case byKind = "by_kind"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            total = try c.decodeIfPresent(Int.self, forKey: .total) ?? 0
            byKind = try c.decodeIfPresent([String: Int].self, forKey: .byKind) ?? [:]
        }

        public init(total: Int = 0, byKind: [String: Int] = [:]) {
            self.total = total
            self.byKind = byKind
        }
    }

    public struct Daemon: Decodable, Equatable, Sendable {
        public var running: Bool
        public var pid: Int?
        public var endpointID: String?
        public var webPage: String?
        public var relayConnected: Bool?
        public var relayURL: String?

        enum CodingKeys: String, CodingKey {
            case running, pid
            case endpointID = "endpoint_id"
            case webPage = "web_page"
            case relayConnected = "relay_connected"
            case relayURL = "relay_url"
        }

        public init(running: Bool = false, pid: Int? = nil, webPage: String? = nil, relayConnected: Bool? = nil) {
            self.running = running
            self.pid = pid
            self.webPage = webPage
            self.relayConnected = relayConnected
        }
    }

    /// How the watch on the backup folder is going: "waiting" (usually for a macOS permission
    /// prompt), "error", "not_watching", or nothing to say.
    public struct BackupState: Decodable, Equatable, Sendable {
        public var state: String?
        public var message: String?
        public var waiting: Double?

        enum CodingKeys: String, CodingKey {
            case state, message
            case waiting = "waiting_s"
        }
    }

    public var computer: Computer?
    public var phones: [Phone]
    public var lastSync: LastSync
    public var backupFolder: String?
    public var backupState: BackupState?
    public var items: Items
    public var dataDirectory: String?
    public var daemon: Daemon

    enum CodingKeys: String, CodingKey {
        case computer, phones, items, daemon
        case lastSync = "last_sync"
        case backupFolder = "backup_folder"
        case backupState = "backup_folder_state"
        case dataDirectory = "data_directory"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        computer = try c.decodeIfPresent(Computer.self, forKey: .computer)
        phones = try c.decodeIfPresent([Phone].self, forKey: .phones) ?? []
        lastSync = try c.decodeIfPresent(LastSync.self, forKey: .lastSync) ?? LastSync()
        backupFolder = try c.decodeIfPresent(String.self, forKey: .backupFolder)
        backupState = try c.decodeIfPresent(BackupState.self, forKey: .backupState)
        items = try c.decodeIfPresent(Items.self, forKey: .items) ?? Items()
        dataDirectory = try c.decodeIfPresent(String.self, forKey: .dataDirectory)
        daemon = try c.decodeIfPresent(Daemon.self, forKey: .daemon) ?? Daemon()
    }

    public init(phones: [Phone] = [], items: Items = Items(), daemon: Daemon = Daemon(), lastSync: LastSync = LastSync()) {
        self.phones = phones
        self.items = items
        self.daemon = daemon
        self.lastSync = lastSync
    }
}

/// One paired phone, from `zone devices --json`. Times are Unix seconds.
public struct Device: Decodable, Equatable, Sendable, Identifiable {
    public var name: String
    public var phoneID: String?
    public var endpointID: String
    public var pairedAt: Double?
    public var lastSync: Double?
    public var itemsReceived: Int
    public var lastPath: String?

    public var id: String { endpointID }

    enum CodingKeys: String, CodingKey {
        case name
        case phoneID = "phone_id"
        case endpointID = "endpoint_id"
        case pairedAt = "paired_at"
        case lastSync = "last_sync"
        case itemsReceived = "items_received"
        case lastPath = "last_path"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? "iPhone"
        phoneID = try c.decodeIfPresent(String.self, forKey: .phoneID)
        endpointID = try c.decodeIfPresent(String.self, forKey: .endpointID) ?? name
        pairedAt = try c.decodeIfPresent(Double.self, forKey: .pairedAt)
        lastSync = try c.decodeIfPresent(Double.self, forKey: .lastSync)
        itemsReceived = try c.decodeIfPresent(Int.self, forKey: .itemsReceived) ?? 0
        lastPath = try c.decodeIfPresent(String.self, forKey: .lastPath)
    }

    public init(name: String, endpointID: String, pairedAt: Double? = nil, itemsReceived: Int = 0) {
        self.name = name
        self.endpointID = endpointID
        self.pairedAt = pairedAt
        self.itemsReceived = itemsReceived
    }
}

/// `zone pair --json`: a one-use code for the phone to scan.
public struct PairCode: Decodable, Equatable, Sendable {
    public var url: String
    public var name: String?
    /// The code's own name. The `paired` event of the phone that uses it carries the same.
    /// Nil from an older zone.
    public var id: String?
    /// Unix seconds.
    public var expiresAt: Double

    enum CodingKeys: String, CodingKey {
        case url, name, id
        case expiresAt = "expires_at"
    }

    public var expiry: Date { Date(timeIntervalSince1970: expiresAt) }

    public init(url: String, name: String? = nil, id: String? = nil, expiresAt: Double) {
        self.url = url
        self.name = name
        self.id = id
        self.expiresAt = expiresAt
    }
}

/// One thing the person said, from `zone list --json` or `zone search --json`.
public struct ZoneItem: Decodable, Equatable, Sendable, Identifiable {
    public var id: String
    /// "dictation", "note", "meeting" or "clip" (text sent from the phone on purpose).
    public var kind: String
    public var source: String?
    public var createdAt: Double
    public var text: String
    public var title: String?
    public var words: Int
    public var duration: Double?
    /// How it got here: "link", "flash" or "backup".
    public var carrier: String?
    public var receivedAt: Double?

    enum CodingKeys: String, CodingKey {
        case id, kind, source, text, title, words, carrier
        case createdAt = "created_at"
        case duration = "duration_s"
        case receivedAt = "received_at"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "dictation"
        source = try c.decodeIfPresent(String.self, forKey: .source)
        createdAt = try c.decodeIfPresent(Double.self, forKey: .createdAt) ?? 0
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        title = try c.decodeIfPresent(String.self, forKey: .title)
        words = try c.decodeIfPresent(Int.self, forKey: .words) ?? 0
        duration = try c.decodeIfPresent(Double.self, forKey: .duration)
        carrier = try c.decodeIfPresent(String.self, forKey: .carrier)
        receivedAt = try c.decodeIfPresent(Double.self, forKey: .receivedAt)
    }

    public init(id: String, kind: String, createdAt: Double, text: String, title: String? = nil, words: Int = 0, receivedAt: Double? = nil) {
        self.id = id
        self.kind = kind
        self.createdAt = createdAt
        self.text = text
        self.title = title
        self.words = words
        self.receivedAt = receivedAt
    }

    public var created: Date { Date(timeIntervalSince1970: createdAt) }

    /// The text on one line, for a row in a list. A meeting leads with its title.
    public var preview: String {
        let flat = text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
        if let title, !title.isEmpty {
            return flat.isEmpty ? title : "\(title): \(flat)"
        }
        return flat
    }

    /// What copying this item puts on the clipboard: the words as they were said.
    public var copyText: String { text }
}

/// `zone list --json` and `zone search --json`.
public struct ItemList: Decodable, Equatable, Sendable {
    public var total: Int
    public var items: [ZoneItem]

    enum CodingKeys: String, CodingKey { case total, items }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        total = try c.decodeIfPresent(Int.self, forKey: .total) ?? 0
        items = try c.decodeIfPresent([ZoneItem].self, forKey: .items) ?? []
    }

    public init(total: Int = 0, items: [ZoneItem] = []) {
        self.total = total
        self.items = items
    }
}

/// `zone stats --json`: only the total is used.
public struct StatsReport: Decodable, Equatable, Sendable {
    public struct Total: Decodable, Equatable, Sendable {
        public var items: Int
        public var words: Int

        enum CodingKeys: String, CodingKey { case items, words }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            items = try c.decodeIfPresent(Int.self, forKey: .items) ?? 0
            words = try c.decodeIfPresent(Int.self, forKey: .words) ?? 0
        }

        public init(items: Int = 0, words: Int = 0) {
            self.items = items
            self.words = words
        }
    }

    public var total: Total
}
