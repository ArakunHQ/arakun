import Foundation

/// Photos sent with a prompt, as phone/src/lib/shots.ts sends them and
/// core/src/phone/upload.rs takes them: each one in `upload` pieces of base64
/// under an id the phone makes up, then the `send` or `start` names the ids.
public enum PhotoUpload {
    /// Each piece carries this much of the base64 at most: a relay frame is
    /// small. A multiple of 4, so every piece decodes on its own, as the
    /// computer decodes them.
    public static let pieceSize = 160 * 1024
    /// The most one photo may hold on the computer (MAX_IMAGE_BYTES); a bigger
    /// one is dropped there.
    public static let maxBytes = 5 * 1024 * 1024
    /// The most photos one prompt carries. The computer keeps 32 at once, so
    /// a few prompts in a row still fit.
    public static let maxPhotos = 10

    /// The `upload` messages that carry `jpeg` as upload `id`. A new session
    /// has no id yet: `session` is empty then, and the computer keeps the
    /// photo by its id alone.
    public static func pieces(of jpeg: Data, id: String, name: String, session: String) -> [[String: Any]] {
        let b64 = Array(jpeg.base64EncodedString().utf8)
        let of = max(1, (b64.count + pieceSize - 1) / pieceSize)
        return (0..<of).map { i in
            let slice = b64[min(b64.count, i * pieceSize)..<min(b64.count, (i + 1) * pieceSize)]
            return [
                "type": "upload", "session": session, "upload": id, "name": name,
                "part": i, "of": of, "data": String(decoding: slice, as: UTF8.self),
            ]
        }
    }

    /// A fresh id for photo `index` of one prompt.
    public static func newID(_ index: Int) -> String {
        "\(Int(Date().timeIntervalSince1970 * 1000))-\(index)-\(UUID().uuidString.prefix(8).lowercased())"
    }

    /// What a prompt says when it is only photos: the computer sends no empty prompt.
    public static func text(_ text: String, photos: Int) -> String {
        guard text.isEmpty, photos > 0 else { return text }
        return photos == 1 ? "(photo)" : "(\(photos) photos)"
    }
}
