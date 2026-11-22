import Foundation

/// What of a crash event leaves the phone: the fields the app's question
/// promises and nothing else. The desktop does the same to a panic before it
/// sends it (`scrub` in src/telemetry.rs).
public enum CrashReport {
    /// The event's own fields that go: the crash, where in the code it
    /// happened, and the app, system and device model it happened on.
    static let kept: Set<String> = [
        "$exception_list", "$exception_level", "$exception_source", "$exception_fingerprint",
        "$debug_images",
        "$lib", "$lib_version",
        "$app_name", "$app_version", "$app_build",
        "$os_name", "$os_version",
        "$device_type", "$device_model", "$device_manufacturer",
        "$process_person_profile", "$geoip_disable",
    ]

    /// The fields of one exception in `$exception_list` that go.
    static let keptInException: Set<String> = ["type", "value", "module", "mechanism", "stacktrace", "thread_id"]

    /// How much of a message goes, after scrubbing.
    static let mostMessage = 300

    /// `properties` cut down to the kept fields, with every exception's
    /// message scrubbed. Anything else the SDK adds (steps, locale, screen,
    /// network, device name) is dropped.
    public static func clean(_ properties: [String: Any]) -> [String: Any] {
        var out = properties.filter { kept.contains($0.key) }
        if let list = out["$exception_list"] as? [[String: Any]] {
            out["$exception_list"] = list.map { e in
                var e = e.filter { keptInException.contains($0.key) }
                if let v = e["value"] as? String { e["value"] = scrub(v) }
                return e
            }
        }
        return out
    }

    /// A message without what could be someone's data: links, emails, paths
    /// and anything shaped like a key or token, then cut short.
    public static func scrub(_ message: String) -> String {
        var s = message
        for (pattern, mark) in rules {
            s = s.replacingOccurrences(of: pattern, with: mark, options: .regularExpression)
        }
        return s.count > mostMessage ? String(s.prefix(mostMessage)) + "…" : s
    }

    // In this order: a link holds paths and tokens of its own.
    private static let rules: [(String, String)] = [
        (#"[A-Za-z][A-Za-z0-9+.-]*://[^\s"'<>]+"#, "<url>"),
        (#"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+"#, "<email>"),
        (#"(~|\.{1,2})?/[^\s:"'<>]+"#, "<path>"),
        (#"[A-Za-z0-9_+=-]{24,}"#, "<secret>"),
    ]
}
