import Foundation

// What the Agents page reads besides `zone mcp status`: the one switch for every agent
// (`zone agents`), what to paste into an app zone does not know (`zone mcp config`), the
// example prompts (`zone prompts`), and what agents may read, as five plain choices.
// Each command is newer than the app may be: a zone without one simply has no answer, and
// the page leaves that part out.

/// `zone agents status --json`: whether agents may use the library at all.
public struct AgentAccess: Decodable, Equatable, Sendable {
    public var enabled: Bool
    /// Unix seconds of the last change. Nil when it was never switched.
    public var changedAt: Double?
    /// zone's own sentence about what the setting means.
    public var message: String?

    enum CodingKeys: String, CodingKey {
        case enabled, agents, message
        case changedAt = "changed_at"
    }

    public init(enabled: Bool, changedAt: Double? = nil, message: String? = nil) {
        self.enabled = enabled
        self.changedAt = changedAt
        self.message = message
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // `enabled` is the answer; `agents` ("on" or "off") says the same in words.
        if let enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) {
            self.enabled = enabled
        } else {
            self.enabled = try c.decodeIfPresent(String.self, forKey: .agents) != "off"
        }
        changedAt = try c.decodeIfPresent(Double.self, forKey: .changedAt)
        message = try c.decodeIfPresent(String.self, forKey: .message)
    }
}

/// `zone prompts --json`: things to ask an agent, ready to copy.
public struct PromptList: Decodable, Equatable, Sendable {
    public struct Prompt: Decodable, Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String
        /// One line on what it does.
        public var description: String
        /// The text to paste, with its defaults and placeholders ("[topic]") filled in.
        public var text: String
        /// The kinds it works on.
        public var kinds: [String]
        /// True when it marks items as handled.
        public var writes: Bool

        enum CodingKeys: String, CodingKey { case id, title, description, text, kinds, writes }

        public init(id: String, title: String, description: String = "", text: String, kinds: [String] = [], writes: Bool = false) {
            self.id = id
            self.title = title
            self.description = description
            self.text = text
            self.kinds = kinds
            self.writes = writes
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = try c.decode(String.self, forKey: .id)
            title = try c.decodeIfPresent(String.self, forKey: .title) ?? id
            description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
            text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
            kinds = try c.decodeIfPresent([String].self, forKey: .kinds) ?? []
            writes = try c.decodeIfPresent(Bool.self, forKey: .writes) ?? false
        }
    }

    public var prompts: [Prompt]

    public init(prompts: [Prompt]) {
        self.prompts = prompts
    }

    enum CodingKeys: String, CodingKey { case prompts }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // A prompt with no text has nothing to copy.
        prompts = (try c.decodeIfPresent([Prompt].self, forKey: .prompts) ?? []).filter { !$0.text.isEmpty }
    }
}

/// `zone mcp config --client other --json`: what to paste into an app's settings.
public struct McpConfig: Decodable, Equatable, Sendable {
    public struct Snippet: Decodable, Equatable, Sendable {
        public var format: String?
        public var snippet: String?
    }

    public var name: String
    public var command: String
    public var args: [String]
    public var env: [String: String]
    public var generic: Snippet?

    enum CodingKeys: String, CodingKey { case name, command, args, env, generic }

    public init(name: String = "zone", command: String, args: [String], env: [String: String] = [:], generic: Snippet? = nil) {
        self.name = name
        self.command = command
        self.args = args
        self.env = env
        self.generic = generic
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? "zone"
        command = try c.decode(String.self, forKey: .command)
        args = try c.decodeIfPresent([String].self, forKey: .args) ?? []
        env = try c.decodeIfPresent([String: String].self, forKey: .env) ?? [:]
        generic = try c.decodeIfPresent(Snippet.self, forKey: .generic)
    }

    /// The JSON most apps take ("mcpServers"): zone's own text when it gave one, else made
    /// here from the command, arguments and environment, the same shape.
    public var snippet: String {
        if let text = generic?.snippet, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return text.hasSuffix("\n") ? text : text + "\n"
        }
        var entry: [String: Any] = ["command": command, "args": args]
        if !env.isEmpty { entry["env"] = env }
        let object: [String: Any] = ["mcpServers": [name: entry]]
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) else { return "" }
        return String(decoding: data, as: UTF8.self) + "\n"
    }
}

/// What agents may read, as the five things a person can tell apart: dictations typed by
/// voice into other apps, dictations in Zone, notes, meetings and clips. Each choice is a
/// flag of `zone mcp install`; see `view(knowsClips:)`.
public struct ReadChoices: Equatable, Sendable {
    /// Dictations whose source is `keyboard`: typed by voice into other apps.
    public var keyboard = true
    /// Dictations whose source is `app` (dictated in the Zone app) or `note` (the recording
    /// behind a voice note).
    public var inZone = true
    public var notes = true
    public var meetings = true
    public var clips = true

    public init(keyboard: Bool = true, inZone: Bool = true, notes: Bool = true, meetings: Bool = true, clips: Bool = true) {
        self.keyboard = keyboard
        self.inZone = inZone
        self.notes = notes
        self.meetings = meetings
        self.clips = clips
    }

    /// The sources hidden when only one kind of dictation is chosen.
    static let inZoneSources = ["app", "note"]
    static let keyboardSources = ["keyboard"]

    /// The view these choices give. Both kinds of dictation off leaves dictations out as a
    /// kind. One of them off hides the other's sources. A zone without clips has no clip
    /// choice: its kinds are the three spoken ones.
    public func view(knowsClips: Bool) -> AgentView {
        var kinds: [String] = []
        if keyboard || inZone { kinds.append("dictation") }
        if notes { kinds.append("note") }
        if meetings { kinds.append("meeting") }
        if knowsClips, clips { kinds.append("clip") }
        var hidden: [String] = []
        if keyboard, !inZone { hidden = Self.inZoneSources }
        if inZone, !keyboard { hidden = Self.keyboardSources }
        let universe = AgentView.allKinds + (knowsClips ? ["clip"] : [])
        let every = Set(kinds) == Set(universe)
        let scope = AgentSurvey.Scope(kinds: kinds, excludeSources: hidden, limited: !(every && hidden.isEmpty))
        return AgentView(scope, knowsClips: knowsClips)
    }

    /// The choices that give this view. Nil when it is a view these five cannot say (set
    /// from the command line), which is then shown as it is and never changed by accident.
    public init?(_ view: AgentView, knowsClips: Bool) {
        let kinds = view.kinds.isEmpty ? AgentView.allKinds + ["clip"] : view.kinds
        let hidden = view.excludeSources.sorted()
        let dictations = kinds.contains("dictation")
        if dictations, ![[], Self.keyboardSources, Self.inZoneSources].contains(hidden) { return nil }
        if !dictations, !hidden.isEmpty { return nil }
        self.init(
            keyboard: dictations && hidden != Self.keyboardSources,
            inZone: dictations && hidden != Self.inZoneSources,
            notes: kinds.contains("note"),
            meetings: kinds.contains("meeting"),
            // An older zone has no clips: the choice stays on, so nothing is lost if it learns them.
            clips: knowsClips ? kinds.contains("clip") : true
        )
        if self.view(knowsClips: knowsClips) != view { return nil }
    }

    /// True when nothing is chosen. zone reads no kinds as every kind, so the page never
    /// lets the last choice go: the switch at the top is what turns agents off.
    public var isEmpty: Bool { !(keyboard || inZone || notes || meetings || clips) }

    /// The one choice left on, when only one is.
    public func isLast(_ path: WritableKeyPath<ReadChoices, Bool>, knowsClips: Bool) -> Bool {
        guard self[keyPath: path] else { return false }
        var without = self
        without[keyPath: path] = false
        if !knowsClips { without.clips = false }
        return without.isEmpty
    }
}

/// The view most connected apps share, which the page shows as "what agents can read" and
/// gives an app that is connected next. An app that reads something else has its own limit.
public enum SharedView {
    /// The view of the most profiles; a tie goes to the wider view. Everything when none is
    /// connected or none can say.
    public static func of(_ agents: [Agent]) -> AgentView {
        var counts: [(view: AgentView, places: Int)] = []
        for agent in agents where agent.isOn {
            guard let view = agent.view else { continue }
            let places = agent.connectedPlaces.count
            if let index = counts.firstIndex(where: { $0.view == view }) {
                counts[index].places += places
            } else {
                counts.append((view, places))
            }
        }
        let best = counts.max { a, b in
            a.places != b.places ? a.places < b.places : width(a.view) < width(b.view)
        }
        return best?.view ?? .everything
    }

    /// The connected apps whose view differs from the shared one (or whose profiles differ).
    public static func exceptions(_ agents: [Agent], shared: AgentView) -> [Agent] {
        agents.filter { $0.isOn && $0.canLimitView && ($0.viewsDiffer || $0.view != shared) }
    }

    /// The apps that follow the shared view: changed with it.
    public static func followers(_ agents: [Agent], shared: AgentView) -> [Agent] {
        agents.filter { $0.isOn && $0.canLimitView && !$0.viewsDiffer && $0.view == shared }
    }

    private static func width(_ view: AgentView) -> Int {
        (view.kinds.isEmpty ? 10 : view.kinds.count) * 10 - view.excludeSources.count
    }
}

/// The Mac apps behind the clients zone knows, by bundle identifier, so the page can show
/// each one's real icon. A client with no app (a command line tool) has none here.
public enum AgentApps {
    public static let bundleIDs: [String: [String]] = [
        "claude-desktop": ["com.anthropic.claudefordesktop"],
        "codex": ["com.openai.codex"],
        "cursor": ["com.todesktop.230313mzl4w4u92"],
        "vscode": ["com.microsoft.VSCode", "com.microsoft.VSCodeInsiders"],
        "windsurf": ["com.exafunction.windsurf"],
        "zed": ["dev.zed.Zed", "dev.zed.Zed-Preview"],
        "kiro": ["dev.kiro.desktop"],
        "lm-studio": ["ai.elementlabs.lmstudio"],
        "goose": ["com.block.goose"],
    ]
}
