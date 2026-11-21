import Foundation

/// Any JSON value. The app reads the computer's messages through it so a
/// field it does not know, or one a newer computer leaves out, never fails a
/// whole message.
public enum JSON: Decodable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSON])
    case object([String: JSON])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSON].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSON].self)) }
    }

    public static func parse(_ data: Data) -> JSON? {
        try? JSONDecoder().decode(JSON.self, from: data)
    }

    public subscript(key: String) -> JSON {
        if case .object(let o) = self { return o[key] ?? .null }
        return .null
    }

    public var string: String? { if case .string(let s) = self { s } else { nil } }
    public var str: String { string ?? "" }
    public var double: Double? { if case .number(let n) = self { n } else { nil } }
    public var int: Int? { double.map { Int($0) } }
    public var bool: Bool { if case .bool(let b) = self { b } else { false } }
    public var array: [JSON] { if case .array(let a) = self { a } else { [] } }
    public var isNull: Bool { self == .null }
}
