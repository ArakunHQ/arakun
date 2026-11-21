@preconcurrency import AVFoundation
import SwiftUI

/// The camera, until it sees a QR code; then the code's text, once.
struct ScannerView: UIViewControllerRepresentable {
    let found: (String) -> Void

    func makeUIViewController(context: Context) -> Scanner {
        let s = Scanner()
        s.found = found
        return s
    }

    func updateUIViewController(_: Scanner, context: Context) {}

    final class Scanner: UIViewController, @preconcurrency AVCaptureMetadataOutputObjectsDelegate {
        var found: ((String) -> Void)?
        private let session = AVCaptureSession()
        /// One queue for start and stop, so a quick close cannot stop the
        /// camera before it has started and leave it running.
        private let queue = DispatchQueue(label: "com.arakun.ios.scanner")
        private var done = false

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            guard let camera = AVCaptureDevice.default(for: .video),
                  let input = try? AVCaptureDeviceInput(device: camera), session.canAddInput(input)
            else {
                let label = UILabel()
                label.text = "No camera. Paste the link instead."
                label.textColor = .white
                label.frame = view.bounds
                label.textAlignment = .center
                view.addSubview(label)
                return
            }
            session.addInput(input)
            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]
            let preview = AVCaptureVideoPreviewLayer(session: session)
            preview.videoGravity = .resizeAspectFill
            preview.frame = view.layer.bounds
            view.layer.addSublayer(preview)
            let session = self.session
            queue.async { session.startRunning() }
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            view.layer.sublayers?.first?.frame = view.layer.bounds
        }

        override func viewWillDisappear(_ animated: Bool) {
            super.viewWillDisappear(animated)
            let session = self.session
            queue.async { session.stopRunning() }
        }

        func metadataOutput(_: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from _: AVCaptureConnection) {
            guard !done, let code = (objects.first as? AVMetadataMachineReadableCodeObject)?.stringValue else { return }
            done = true
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            found?(code)
        }
    }
}
