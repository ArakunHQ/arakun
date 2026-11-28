import ArakunKit
import Foundation
import ImageIO
import UIKit

/// A photo waiting in the composer, drawn small and re-encoded as JPEG as the
/// phone page does, so what goes through the relay is a few hundred kilobytes
/// and always under the computer's limit.
struct Photo: Identifiable, Sendable {
    let id = UUID()
    let jpeg: Data
    /// A small JPEG for the chip.
    let thumb: Data

    /// The longest side a photo is sent at: what the agents read at full detail.
    private static let longestSide: CGFloat = 1568
    private static let thumbSide: CGFloat = 160

    /// A picture from the library, in any format iOS decodes. ImageIO draws it
    /// small without decoding all of it, and turns it upright. Nil when it
    /// does not decode.
    nonisolated static func shrink(_ data: Data) -> Photo? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let big = thumbnail(source, longestSide),
              let small = thumbnail(source, thumbSide)
        else { return nil }
        return encode(UIImage(cgImage: big), thumb: UIImage(cgImage: small))
    }

    /// A photo the camera took.
    nonisolated static func shrink(_ image: UIImage) -> Photo? {
        guard image.size.width > 0, image.size.height > 0 else { return nil }
        return encode(redraw(image, longestSide), thumb: redraw(image, thumbSide))
    }

    private nonisolated static func thumbnail(_ source: CGImageSource, _ side: CGFloat) -> CGImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: Int(side),
            kCGImageSourceShouldCacheImmediately: true,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    /// `image` upright, its longest side at most `side` pixels.
    private nonisolated static func redraw(_ image: UIImage, _ side: CGFloat) -> UIImage {
        let scale = min(1, side / max(image.size.width, image.size.height))
        let size = CGSize(width: max(1, (image.size.width * scale).rounded()),
                          height: max(1, (image.size.height * scale).rounded()))
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        return UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
    }

    /// Lower quality until it fits; a photo at this size almost always fits at the first.
    private nonisolated static func encode(_ image: UIImage, thumb: UIImage) -> Photo? {
        guard let small = thumb.jpegData(compressionQuality: 0.6) else { return nil }
        for quality in [0.82, 0.7, 0.55, 0.4] {
            if let jpeg = image.jpegData(compressionQuality: quality), jpeg.count <= PhotoUpload.maxBytes {
                return Photo(jpeg: jpeg, thumb: small)
            }
        }
        return nil
    }
}
