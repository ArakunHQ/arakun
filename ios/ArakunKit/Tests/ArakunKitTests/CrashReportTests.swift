import Foundation
import Testing
@testable import ArakunKit

struct CrashReportTests {
    @Test func keepsOnlyTheCrashAndWhereItRan() {
        let props: [String: Any] = [
            "$exception_level": "fatal",
            "$app_version": "0.14.0",
            "$os_version": "27.2",
            "$device_model": "iPhone17,1",
            "$exception_steps": [["$message": "opened a session"]],
            "$locale": "nl-NL",
            "$device_name": "Rik's iPhone",
            "$screen_width": 402,
            "anything": "else",
            "$exception_list": [[
                "type": "NSInvalidArgumentException",
                "value": "bad key sk-abcdefghijklmnopqrstuvwxyz0123 in /Users/rik/notes.txt",
                "stacktrace": ["frames": []],
                "extra": "dropped",
            ]],
        ]
        let out = CrashReport.clean(props)
        #expect(Set(out.keys) == ["$exception_level", "$app_version", "$os_version", "$device_model", "$exception_list"])
        let e = (out["$exception_list"] as? [[String: Any]])?.first
        #expect(e?["type"] as? String == "NSInvalidArgumentException")
        #expect(e?["extra"] == nil)
        #expect(e?["stacktrace"] != nil)
        #expect(e?["value"] as? String == "bad key <secret> in <path>")
    }

    @Test func scrubsLinksEmailsPathsAndTokens() {
        #expect(CrashReport.scrub("GET https://example.com/a?t=1 failed") == "GET <url> failed")
        #expect(CrashReport.scrub("for rik@example.com") == "for <email>")
        #expect(CrashReport.scrub("open ~/code/app and ./x") == "open <path> and <path>")
        #expect(CrashReport.scrub("Index out of range") == "Index out of range")
        #expect(CrashReport.scrub(String(repeating: "a ", count: 400)).count == CrashReport.mostMessage + 1)
    }
}
