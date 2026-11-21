// What the computer sends, as core/src/phone/wire.rs writes it.
import Foundation

public enum SessionStatus: String, Sendable {
    case idle, working, needsInput = "needs_input", error

    init(_ j: JSON) { self = SessionStatus(rawValue: j.str) ?? .idle }
}

/// A session's line in the inbox.
public struct InboxSession: Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let agent: String
    public let status: SessionStatus
    public let updated: Date
    public let added: Int
    public let removed: Int
    public let project: String
    public let env: String
    public let place: String
    public let pr: String?
    /// What a waiting session asks: a permission or a question.
    public let asks: String?

    public init(_ j: JSON) {
        id = j["id"].str
        title = j["title"].str
        agent = j["agent"].str
        status = SessionStatus(j["status"])
        updated = Date(timeIntervalSince1970: j["updated"].double ?? 0)
        added = j["added"].int ?? 0
        removed = j["removed"].int ?? 0
        project = j["project"].str
        env = j["env"].str
        place = j["place"].str
        pr = j["pr"].string
        asks = j["asks"]["text"].string
    }
}

/// Where a session can start.
public struct Place: Identifiable, Equatable, Sendable {
    public var id: String { path }
    public let path: String
    public let project: String
    public let env: String
    public let branch: String?
    public let agent: String

    public init(_ j: JSON) {
        path = j["path"].str
        project = j["project"].str
        env = j["env"].str
        branch = j["branch"].string
        agent = j["agent"].str
    }
}

public struct Inbox: Sendable {
    public let host: String
    public let sessions: [InboxSession]
    public let places: [Place]

    public init(_ j: JSON) {
        host = j["host"].str
        sessions = j["sessions"].array.map(InboxSession.init)
        places = j["places"].array.map(Place.init)
    }
}

/// One entry of a session's transcript, cut down for a phone.
public enum Entry: Equatable, Sendable {
    case user(text: String, at: Date, elapsedMs: Int?, pastes: [String])
    case agent(String)
    case thought(String)
    case tool(Tool)
    case plan([PlanItem])
    case notice(text: String, error: Bool)
    case compacted(before: Int?, after: Int?)
    case unknown

    public struct Tool: Equatable, Sendable {
        public let id: String
        public let title: String
        public let kind: String
        /// pending, in_progress, completed or failed.
        public let status: String
        public let command: String?
        public let output: String?
        public let exitCode: Int?
        public let added: Int
        public let removed: Int
        public let diffs: [(path: String, added: Int, removed: Int)]

        public static func == (a: Tool, b: Tool) -> Bool {
            a.id == b.id && a.title == b.title && a.status == b.status && a.output == b.output
                && a.command == b.command && a.added == b.added && a.removed == b.removed
        }
    }

    public struct PlanItem: Equatable, Sendable {
        public let content: String
        /// pending, in_progress or completed.
        public let status: String
    }

    public init(_ j: JSON) {
        switch j["type"].str {
        case "user":
            self = .user(text: j["text"].str, at: Date(timeIntervalSince1970: j["at"].double ?? 0),
                         elapsedMs: j["elapsed_ms"].int, pastes: j["pastes"].array.map { $0["text"].str })
        case "agent": self = .agent(j["text"].str)
        case "thought": self = .thought(j["text"].str)
        case "tool":
            self = .tool(Tool(
                id: j["id"].str, title: j["title"].str, kind: j["kind"].str, status: j["status"].str,
                command: j["command"].string, output: j["output"].string, exitCode: j["exit_code"].int,
                added: j["added"].int ?? 0, removed: j["removed"].int ?? 0,
                diffs: j["diffs"].array.map { ($0["path"].str, $0["added"].int ?? 0, $0["removed"].int ?? 0) }))
        case "plan":
            self = .plan(j["entries"].array.map { PlanItem(content: $0["content"].str, status: $0["status"].str) })
        case "notice": self = .notice(text: j["text"].str, error: j["error"].bool)
        case "compacted": self = .compacted(before: j["before"].int, after: j["after"].int)
        default: self = .unknown
        }
    }
}

/// What the agent waits for the user to allow.
public struct PermissionAsk: Equatable, Sendable {
    public struct Option: Equatable, Sendable, Identifiable {
        public let id: String
        public let name: String
        /// allow_once, allow_always, reject_once, reject_always.
        public let kind: String
    }

    public let toolCallId: String
    public let title: String
    public let options: [Option]

    public init?(_ j: JSON) {
        guard !j.isNull else { return nil }
        toolCallId = j["tool_call_id"].str
        title = j["title"].str
        options = j["options"].array.map { Option(id: $0["id"].str, name: $0["name"].str, kind: $0["kind"].str) }
    }
}

/// A questionnaire the agent waits on.
public struct QuestionAsk: Equatable, Sendable {
    public struct Question: Equatable, Sendable {
        public let topic: String
        public let question: String
        public let options: [String]
        public let multi: Bool
    }

    public let toolCallId: String
    public let questions: [Question]

    public init?(_ j: JSON) {
        guard !j.isNull else { return nil }
        toolCallId = j["tool_call_id"].str
        questions = j["questions"].array.map {
            Question(topic: $0["topic"].str, question: $0["question"].str, options: $0["options"].array.map(\.str), multi: $0["multi"].bool)
        }
    }
}

/// A session open on the phone: its loaded entries `from..<from+entries.count`
/// of `count`, and its state.
public struct LiveSession: Sendable {
    public let id: String
    public var title: String
    public var agent: String
    public var status: SessionStatus
    public var count: Int
    public var from: Int
    public var entries: [Entry]
    public var permission: PermissionAsk?
    public var question: QuestionAsk?
    public var usage: String?
    public var queue: [String]
    public var turnStarted: Date?
    public var changes: (files: Int, added: Int, removed: Int)

    public init(id: String, title: String, agent: String) {
        self.id = id
        self.title = title
        self.agent = agent
        status = .idle
        count = 0
        from = 0
        entries = []
        queue = []
        changes = (0, 0, 0)
    }

    /// A `session` message: its tail of entries and its state. Opened again
    /// after a reconnect, the tail overlaps what is loaded, older ones included.
    public mutating func apply(session j: JSON) {
        let tailFrom = j["from"].int ?? 0
        let tail = j["entries"].array.map(Entry.init)
        if !entries.isEmpty, tailFrom >= from, tailFrom <= from + entries.count {
            entries = Array(entries.prefix(tailFrom - from)) + tail
        } else {
            from = tailFrom
            entries = tail
        }
        count = j["count"].int ?? from + entries.count
        if let t = j["title"].string, !t.isEmpty { title = t }
        if let a = j["agent"].string { agent = a }
        apply(state: j)
    }

    /// An `entries` message: from `from` on changed, or older ones (`older`).
    public mutating func apply(entries j: JSON) {
        let at = j["from"].int ?? 0
        let list = j["entries"].array.map(Entry.init)
        count = j["count"].int ?? count
        if j["older"].bool {
            entries = Array(list.prefix(max(0, from - at))) + entries
            from = at
        } else if at <= from {
            from = at
            entries = list
        } else {
            entries = Array(entries.prefix(at - from)) + list
        }
    }

    /// A `state` message (and the same fields of a `session` one).
    public mutating func apply(state j: JSON) {
        status = SessionStatus(j["status"])
        permission = PermissionAsk(j["permission"])
        question = QuestionAsk(j["question"])
        usage = j["usage"]["summary"].string
        queue = j["queue"].array.map(\.str)
        if let t = j["title"].string, !t.isEmpty { title = t }
        turnStarted = j["elapsed_ms"].double.map { Date().addingTimeInterval(-$0 / 1000) }
        let c = j["changes"]
        changes = (c["files"].int ?? 0, c["added"].int ?? 0, c["removed"].int ?? 0)
    }
}
