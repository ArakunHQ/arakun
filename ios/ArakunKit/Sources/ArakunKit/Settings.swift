// A session's model and reasoning, the choices for a new session, and the
// agents' plan limits, as core/src/phone/wire.rs writes them (`settings`,
// `options` and `usage`).
import Foundation

/// One setting with its value and its choices.
public struct SettingSet: Identifiable, Equatable, Sendable {
    public struct Option: Identifiable, Equatable, Sendable {
        public let id: String
        public let name: String
        public let detail: String?
        /// Why it cannot be picked now, when it cannot.
        public let disabled: String?

        public var isDisabled: Bool { disabled != nil }
    }

    public var id: String { key }
    /// `modelId` or `reasoningEffort`: what `setting` and `start` name it by.
    public let key: String
    public let label: String
    public let current: String
    public let options: [Option]

    public init(_ j: JSON) {
        key = j["key"].str
        label = j["label"].str
        current = j["current"].str
        options = j["options"].array.compactMap { o -> Option? in
            guard let id = o["id"].string else { return nil }
            // The computer says why; an older one may only say that it is.
            let disabled = o["disabled"].string ?? (o["disabled"].bool ? "" : nil)
            return Option(id: id, name: o["name"].string ?? id, detail: o["detail"].string, disabled: disabled)
        }
    }

    /// The `settings` of a `settings` or `options` message; one without a key is left out.
    public static func list(_ j: JSON) -> [SettingSet] {
        j["settings"].array.map(SettingSet.init).filter { !$0.key.isEmpty }
    }

    /// The name of the option `id`, or of the current one.
    public func name(of id: String? = nil) -> String? {
        let id = id ?? current
        return options.first { $0.id == id }?.name
    }
}

/// An `options` message: what a new session of `agent` in `env` would start with.
public struct StartOptions: Equatable, Sendable {
    public let env: String
    public let agent: String
    public let settings: [SettingSet]

    public init(env: String, agent: String, settings: [SettingSet]) {
        self.env = env
        self.agent = agent
        self.settings = settings
    }

    public init(_ j: JSON) {
        env = j["env"].str
        agent = j["agent"].str
        settings = SettingSet.list(j)
    }

    public func isFor(env: String, agent: String) -> Bool {
        self.env == env && self.agent == agent
    }
}

/// What the New session form picked, by key; the rest stay as the agent would start.
public struct StartPicks: Equatable, Sendable {
    public static let model = "modelId"
    public static let reasoning = "reasoningEffort"

    public private(set) var picked: [String: String] = [:]

    public init() {}

    public subscript(key: String) -> String? { picked[key] }

    public mutating func pick(_ key: String, _ id: String) {
        picked[key] = id
        // The reasoning on offer depends on the model: a new model drops the old pick.
        if key == Self.model { picked[Self.reasoning] = nil }
    }

    public mutating func clear() { picked = [:] }

    /// As `start` sends them, `[key, value]`, the model first: the reasoning
    /// on offer depends on it.
    public var pairs: [[String]] {
        [Self.model, Self.reasoning].compactMap { k in picked[k].map { [k, $0] } }
    }
}

/// The agents a session can start with, as the phone page lists them.
public enum Agents {
    public static let all: [(id: String, name: String)] = [
        ("claude", "Claude Code"),
        ("droid", "Droid"),
        ("opencode", "OpenCode"),
        ("codex", "Codex"),
        ("devin", "Devin"),
        ("cursor", "Cursor"),
        ("antigravity", "Antigravity"),
    ]

    public static func name(_ id: String) -> String {
        all.first { $0.id == id }?.name ?? id
    }
}

/// One agent's plan limits, a card of the `usage` message.
public struct PlanCard: Identifiable, Equatable, Sendable {
    /// One window that is running: how full it is, and when it resets.
    public struct Row: Equatable, Sendable {
        public let label: String
        public let percent: Int
        public let detail: String?
    }

    public var id: String { agent }
    /// `claude` or `droid`.
    public let agent: String
    public let name: String
    public let signedIn: Bool
    public let plan: String?
    public let rows: [Row]
    /// The windows not running.
    public let idle: [String]
    public let balance: String?
    public let note: String?
    /// `danger` when the note says something went wrong.
    public let tone: String
    /// When the numbers were read.
    public let at: Date?

    public init(_ j: JSON) {
        agent = j["agent"].str
        name = j["name"].string ?? j["agent"].str
        signedIn = j["signed_in"].bool
        plan = j["plan"].string.flatMap { $0.isEmpty ? nil : $0 }
        rows = j["rows"].array.map {
            Row(label: $0["label"].str, percent: max(0, $0["percent"].int ?? 0), detail: $0["detail"].string)
        }
        idle = j["idle"].array.map(\.str)
        balance = j["balance"].string
        note = j["note"].string.flatMap { $0.isEmpty ? nil : $0 }
        tone = j["tone"].string ?? "normal"
        at = j["at"].double.flatMap { $0 > 0 ? Date(timeIntervalSince1970: $0) : nil }
    }

    /// The cards of a `usage` message.
    public static func list(_ j: JSON) -> [PlanCard] {
        j["agents"].array.map(PlanCard.init)
    }
}
