import Foundation
import Testing
@testable import ArakunKit

struct UploadTests {
    @Test func aSmallPhotoGoesInOnePiece() {
        let pieces = PhotoUpload.pieces(of: Data([0, 1, 2]), id: "u1", name: "photo.jpg", session: "s1")
        #expect(pieces.count == 1)
        let p = pieces[0]
        #expect(p["type"] as? String == "upload")
        #expect(p["session"] as? String == "s1")
        #expect(p["upload"] as? String == "u1")
        #expect(p["name"] as? String == "photo.jpg")
        #expect(p["part"] as? Int == 0)
        #expect(p["of"] as? Int == 1)
        #expect(p["data"] as? String == "AAEC")
    }

    @Test func aBigPhotoComesApartAndBackWhole() throws {
        // Just over two pieces of base64.
        let jpeg = Data((0..<(PhotoUpload.pieceSize / 4 * 3 * 2 + 10)).map { UInt8($0 % 251) })
        let pieces = PhotoUpload.pieces(of: jpeg, id: "u2", name: "photo.jpg", session: "")
        #expect(pieces.count == 3)
        var whole = Data()
        for (i, p) in pieces.enumerated() {
            #expect(p["part"] as? Int == i)
            #expect(p["of"] as? Int == 3)
            let data = try #require(p["data"] as? String)
            #expect(data.utf8.count <= PhotoUpload.pieceSize)
            // The computer decodes each piece alone.
            whole.append(try #require(Data(base64Encoded: data)))
        }
        #expect(whole == jpeg)
        #expect(pieces.allSatisfy { JSONSerialization.isValidJSONObject($0) })
    }

    @Test func anEmptyPhotoIsStillOnePiece() {
        let pieces = PhotoUpload.pieces(of: Data(), id: "u3", name: "photo.jpg", session: "s1")
        #expect(pieces.count == 1)
        #expect(pieces[0]["data"] as? String == "")
    }

    @Test func idsDifferAndPiecesDecodeAlone() {
        #expect(PhotoUpload.newID(0) != PhotoUpload.newID(0))
        #expect(PhotoUpload.pieceSize % 4 == 0)
    }

    @Test func aPromptOfOnlyPhotosSaysSo() {
        #expect(PhotoUpload.text("", photos: 1) == "(photo)")
        #expect(PhotoUpload.text("", photos: 3) == "(3 photos)")
        #expect(PhotoUpload.text("look", photos: 2) == "look")
        #expect(PhotoUpload.text("", photos: 0) == "")
    }
}
