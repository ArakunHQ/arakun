import ArakunKit
import Observation
import UIKit

/// One picture of the session on screen, as far as it got.
@MainActor
@Observable
final class PictureSlot {
    enum State {
        case loading
        case ready(UIImage)
        /// The computer had none to send, it was no image, or the link went down.
        case failed
    }

    fileprivate(set) var state: State = .loading
}

/// The pictures of the session on screen, each fetched once from the computer
/// and kept until the session goes. Every picture has its own slot, so one
/// that arrives redraws itself only, not the transcript around it.
///
/// The links carry a signature: never log them.
@MainActor
final class PictureCache {
    private var slots: [String: PictureSlot] = [:]
    private var flying: Set<String> = []
    /// Counts resets, so an answer for a session that went lands nowhere.
    private var generation = 0

    func slot(_ url: String) -> PictureSlot {
        if let s = slots[url] { return s }
        let s = PictureSlot()
        slots[url] = s
        return s
    }

    /// Fetches the picture unless it is here or on its way. One that failed
    /// is tried again, so a picture lost to a dropped link comes back with it.
    func load(_ url: String, fetch: @MainActor (String) async -> Data?) async {
        let s = slot(url)
        if case .ready = s.state { return }
        guard flying.insert(url).inserted else { return }
        let gen = generation
        s.state = .loading
        var image: UIImage?
        if let data = await fetch(url), let raw = UIImage(data: data) {
            // Decoded off the main thread, so a 5 MB photo does not stall scrolling.
            image = await raw.byPreparingForDisplay() ?? raw
        }
        guard gen == generation else { return }
        flying.remove(url)
        s.state = image.map { .ready($0) } ?? .failed
    }

    /// Lets go of every picture: the session went.
    func reset() {
        generation += 1
        slots = [:]
        flying = []
    }
}
