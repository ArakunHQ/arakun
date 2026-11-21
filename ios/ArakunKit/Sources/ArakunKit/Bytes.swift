import Foundation

/// Base64url without padding, as the computer and the phone page write it.
public enum Base64URL {
    public static func encode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    public static func decode(_ string: String) -> Data? {
        guard string.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }) else {
            return nil
        }
        var s = string.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s += "=" }
        return Data(base64Encoded: s)
    }
}

/// RFC 4648 base32 without padding, lower case: 10 bytes make 16 characters.
enum Base32 {
    private static let alphabet = Array("abcdefghijklmnopqrstuvwxyz234567")

    static func encode(_ bytes: some Sequence<UInt8>) -> String {
        var out = ""
        var acc: UInt32 = 0
        var bits = 0
        for b in bytes {
            acc = (acc << 8) | UInt32(b)
            bits += 8
            while bits >= 5 {
                out.append(alphabet[Int((acc >> UInt32(bits - 5)) & 31)])
                bits -= 5
            }
        }
        if bits > 0 { out.append(alphabet[Int((acc << UInt32(5 - bits)) & 31)]) }
        return out
    }
}

extension Data {
    /// Big-endian bytes of an integer.
    init<T: FixedWidthInteger>(bigEndian value: T) {
        var v = value.bigEndian
        self = Swift.withUnsafeBytes(of: &v) { Data($0) }
    }

    /// The big-endian integer at `offset`.
    func readBigEndian<T: FixedWidthInteger>(_: T.Type, at offset: Int = 0) -> T? {
        let size = MemoryLayout<T>.size
        guard offset >= 0, offset <= count, size <= count - offset else { return nil }
        var v: T = 0
        for b in self[(startIndex + offset)..<(startIndex + offset + size)] {
            v = (v << 8) | T(truncatingIfNeeded: b)
        }
        return v
    }
}
