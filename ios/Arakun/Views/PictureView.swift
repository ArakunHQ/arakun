import ArakunKit
import SwiftUI
import UIKit

/// A picture of the transcript, fetched from the computer once per session.
/// While it loads, or when it cannot, its name stands in for it. A tap opens
/// it full screen.
struct PictureView: View {
    let picture: Picture
    /// A prompt's photo: small, at the right of the bubble. Otherwise the
    /// width of the screen, as the agent's pictures show.
    var thumbnail = false
    @Environment(AppModel.self) private var model
    /// The picture open full screen. Held here, so it stays while open even
    /// if the session lets go of its pictures.
    @State private var full: Shown?

    var body: some View {
        let slot = model.pictures.slot(picture.url)
        Group {
            if case .ready(let image) = slot.state {
                Button { full = Shown(image: image) } label: { shown(image) }
                    .buttonStyle(.plain)
                    .accessibilityLabel(picture.name)
                    .accessibilityHint("Opens the picture full screen")
                    .accessibilityAddTraits(.isImage)
            } else {
                placeholder(loading: picture.served && slot.isLoading)
            }
        }
        // Again when the link comes back: a picture lost to a drop is asked anew.
        .task(id: model.isLinked) { await model.loadPicture(picture) }
        .fullScreenCover(item: $full) { shown in
            PictureFullView(name: picture.name, image: shown.image)
        }
    }

    private func shown(_ image: UIImage) -> some View {
        Image(uiImage: image)
            .resizable()
            .scaledToFit()
            // Never past its own size: a small picture stays small.
            .frame(maxWidth: min(image.size.width, thumbnail ? 200 : .infinity),
                   maxHeight: thumbnail ? 200 : nil)
            .clipShape(RoundedRectangle(cornerRadius: Radius.control))
            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.line))
            .frame(maxWidth: .infinity, alignment: thumbnail ? .trailing : .leading)
    }

    private func placeholder(loading: Bool) -> some View {
        HStack(spacing: 6) {
            if loading {
                ProgressView().controlSize(.mini).tint(Color.textFaint)
            } else {
                Icon("file01", size: 13)
            }
            Text(picture.name).lineLimit(1).truncationMode(.middle)
        }
        .font(.mono(11.5))
        .foregroundStyle(Color.textMuted)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(loading ? "\(picture.name), loading" : picture.name)
    }
}

private extension PictureSlot {
    var isLoading: Bool {
        if case .loading = state { return true }
        return false
    }
}

private struct Shown: Identifiable {
    let id = UUID()
    let image: UIImage
}

/// A picture over everything: pinch or double-tap to zoom, Share to send it on.
struct PictureFullView: View {
    let name: String
    let image: UIImage
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack(alignment: .top) {
            Color.deep.ignoresSafeArea()
            Zoomable(image: image)
                .ignoresSafeArea()
                .accessibilityLabel(name)
                .accessibilityAddTraits(.isImage)
            HStack(spacing: 12) {
                Button { dismiss() } label: {
                    Icon("cancel01", size: 18).frame(width: 36, height: 36).contentShape(Rectangle())
                }
                .accessibilityLabel("Close")
                Text(name)
                    .font(.mono(12))
                    .foregroundStyle(Color.textMuted)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity)
                ShareLink(item: Image(uiImage: image), preview: SharePreview(name, image: Image(uiImage: image))) {
                    Image(systemName: "square.and.arrow.up").font(.body16).frame(width: 36, height: 36).contentShape(Rectangle())
                }
                .accessibilityLabel("Share")
            }
            .foregroundStyle(Color.text)
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Color.deep.opacity(0.85))
        }
    }
}

/// UIScrollView zooms a picture as Photos does; SwiftUI has nothing like it.
private struct Zoomable: UIViewRepresentable {
    let image: UIImage

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UIScrollView {
        let scroll = UIScrollView()
        scroll.delegate = context.coordinator
        scroll.minimumZoomScale = 1
        scroll.maximumZoomScale = 8
        scroll.bouncesZoom = true
        scroll.showsHorizontalScrollIndicator = false
        scroll.showsVerticalScrollIndicator = false
        scroll.contentInsetAdjustmentBehavior = .never
        let view = UIImageView(image: image)
        view.contentMode = .scaleAspectFit
        view.frame = scroll.bounds
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        scroll.addSubview(view)
        context.coordinator.view = view
        let twice = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.tappedTwice(_:)))
        twice.numberOfTapsRequired = 2
        scroll.addGestureRecognizer(twice)
        return scroll
    }

    func updateUIView(_ scroll: UIScrollView, context: Context) {
        context.coordinator.view?.image = image
    }

    @MainActor
    final class Coordinator: NSObject, UIScrollViewDelegate {
        weak var view: UIImageView?

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { view }

        /// In to where it was tapped, or back out.
        @objc func tappedTwice(_ tap: UITapGestureRecognizer) {
            guard let scroll = tap.view as? UIScrollView, let view else { return }
            if scroll.zoomScale > scroll.minimumZoomScale {
                scroll.setZoomScale(scroll.minimumZoomScale, animated: true)
            } else {
                let at = tap.location(in: view)
                let size = CGSize(width: scroll.bounds.width / 3, height: scroll.bounds.height / 3)
                scroll.zoom(to: CGRect(x: at.x - size.width / 2, y: at.y - size.height / 2,
                                       width: size.width, height: size.height), animated: true)
            }
        }
    }
}
