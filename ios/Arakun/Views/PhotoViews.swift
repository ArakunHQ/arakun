import ArakunKit
import PhotosUI
import SwiftUI
import UIKit

/// Adds photos to `photos`: from the library, or the camera when there is one.
struct PhotoButton: View {
    @Binding var photos: [Photo]
    /// With its name beside the icon, in an outline, for a form.
    var labelled = false
    @State private var choosing = false
    @State private var shooting = false
    @State private var picked: [PhotosPickerItem] = []
    @State private var reading = false

    private var room: Int { PhotoUpload.maxPhotos - photos.count }

    var body: some View {
        Menu {
            Button { choosing = true } label: { Label("Choose photos", systemImage: "photo.on.rectangle") }
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button { shooting = true } label: { Label("Take photo", systemImage: "camera") }
            }
        } label: {
            label
        }
        .disabled(room <= 0 || reading)
        .opacity(room <= 0 ? 0.5 : 1)
        .accessibilityLabel("Attach photos")
        .photosPicker(isPresented: $choosing, selection: $picked, maxSelectionCount: max(1, room), matching: .images)
        .fullScreenCover(isPresented: $shooting) {
            CameraView { image in
                shooting = false
                guard let image else { return }
                Task { await add { Photo.shrink(image) } }
            }
            .ignoresSafeArea()
        }
        .onChange(of: picked) { _, items in
            guard !items.isEmpty else { return }
            picked = []
            reading = true
            Task {
                for item in items {
                    guard let data = try? await item.loadTransferable(type: Data.self) else { continue }
                    await add { Photo.shrink(data) }
                }
                reading = false
            }
        }
    }

    @ViewBuilder private var label: some View {
        let icon = Group {
            if reading {
                ProgressView().tint(Color.textMuted)
            } else {
                Image(systemName: "photo").font(.body16)
            }
        }
        if labelled {
            HStack(spacing: 8) {
                icon
                Text("Photos").font(.geist(15, .medium))
            }
            .foregroundStyle(Color.text)
            .padding(.horizontal, 14)
            .frame(minHeight: 40)
            .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.raised))
            .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.lineStrong))
        } else {
            icon
                .foregroundStyle(Color.textMuted)
                .frame(width: 34, height: 34)
                .contentShape(Rectangle())
        }
    }

    /// Shrinks off the main thread: a photo from the camera is 12 megapixels or more.
    private func add(_ make: @escaping @Sendable () -> Photo?) async {
        let photo = await Task.detached(priority: .userInitiated, operation: make).value
        if let photo, photos.count < PhotoUpload.maxPhotos { photos.append(photo) }
    }
}

/// The photos waiting to go, small; a tap takes one out.
struct PhotoChips: View {
    @Binding var photos: [Photo]

    var body: some View {
        if !photos.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(Array(photos.enumerated()), id: \.element.id) { i, p in
                        Button { photos.removeAll { $0.id == p.id } } label: {
                            ZStack(alignment: .topTrailing) {
                                Image(uiImage: UIImage(data: p.thumb) ?? UIImage())
                                    .resizable()
                                    .scaledToFill()
                                    .frame(width: 56, height: 56)
                                    .clipShape(RoundedRectangle(cornerRadius: Radius.control))
                                    .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.line))
                                Icon("cancel01", size: 12)
                                    .foregroundStyle(Color.text)
                                    .padding(3)
                                    .background(Circle().fill(Color.ground.opacity(0.85)))
                                    .padding(4)
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(photos.count == 1 ? "Remove photo" : "Remove photo \(i + 1)")
                    }
                }
            }
            .scrollClipDisabled()
        }
    }
}

/// The system camera, for one photo; nil when it was closed without one.
struct CameraView: UIViewControllerRepresentable {
    let done: (UIImage?) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_: UIImagePickerController, context: Context) {}

    @MainActor
    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let done: (UIImage?) -> Void

        init(done: @escaping (UIImage?) -> Void) { self.done = done }

        func imagePickerController(_: UIImagePickerController,
                                   didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            done(info[.originalImage] as? UIImage)
        }

        func imagePickerControllerDidCancel(_: UIImagePickerController) { done(nil) }
    }
}
