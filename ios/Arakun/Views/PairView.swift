import SwiftUI

/// The first screen: scan the code in Arakun's Phone tab, or paste its link.
struct PairView: View {
    @Environment(AppModel.self) private var model
    @State private var scanning = false
    @State private var pasted = ""
    @FocusState private var pasting: Bool

    var body: some View {
        ScrollView {
            VStack(spacing: 32) {
                VStack(spacing: 16) {
                    Mascot(mood: model.connection == .pairing ? .thinking : problem != nil ? .error : .idle, width: 132)
                        .padding(.top, 24)
                    Text("Arakun")
                        .font(.geist(40, .semibold, relativeTo: .largeTitle))
                        .tracking(-1.6)
                    Text("Your coding sessions, on this phone.")
                        .font(.geist(17, relativeTo: .title3))
                        .foregroundStyle(Color.textMuted)
                        .multilineTextAlignment(.center)
                }

                VStack(alignment: .leading, spacing: 14) {
                    Eyebrow("Pair this phone")
                    Step(n: 1, text: "Open Arakun on your computer.")
                    Step(n: 2, text: "Open the Phone tab and choose the relay.")
                    Step(n: 3, text: "Scan the code it shows.")
                }
                .padding(18)
                .frame(maxWidth: .infinity, alignment: .leading)
                .card()

                VStack(spacing: 12) {
                    if model.connection == .pairing {
                        HStack(spacing: 10) {
                            ProgressView().tint(Color.textMuted)
                            Text("Pairing with your computer…").foregroundStyle(Color.textMuted)
                        }
                        .frame(minHeight: 48)
                    } else {
                        Button {
                            scanning = true
                        } label: {
                            HStack(spacing: 10) {
                                Icon("search01", size: 18)
                                Text("Scan the code")
                            }
                            .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.primary)

                        HStack(spacing: 8) {
                            TextField("", text: $pasted, prompt: Text("Or paste the pairing link").foregroundStyle(Color.textFaint))
                                .font(.mono(14))
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .keyboardType(.URL)
                                .submitLabel(.go)
                                .focused($pasting)
                                .onSubmit(pairPasted)
                                .padding(.horizontal, 12)
                                .frame(minHeight: 44)
                                .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
                                .overlay(RoundedRectangle(cornerRadius: Radius.control)
                                    .strokeBorder(pasting ? Color.amberInk : Color.lineStrong))
                            Button("Pair", action: pairPasted)
                                .buttonStyle(.outline)
                                .disabled(pasted.isEmpty)
                        }
                    }
                    if let e = problem {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Icon("alert-circle", size: 15)
                            Text(e)
                        }
                        .font(.small)
                        .foregroundStyle(Color.failure)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }

                HStack(spacing: 8) {
                    Icon("link01", size: 14)
                    Text("End to end encrypted. The relay only passes bytes along.")
                }
                .font(.mono(11.5, relativeTo: .caption))
                .foregroundStyle(Color.textFaint)
                .multilineTextAlignment(.center)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 32)
            .frame(maxWidth: 520)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Color.ground.ignoresSafeArea())
        .sheet(isPresented: $scanning) {
            ScanSheet { code in
                scanning = false
                model.pair(code)
            }
        }
    }

    /// What went wrong, from pairing or from a computer that did not prove its key.
    private var problem: String? {
        if let e = model.pairError { return e }
        if case .failed(let why) = model.connection { return why }
        return nil
    }

    private func pairPasted() {
        model.pair(pasted)
        pasted = ""
    }
}

private struct Step: View {
    let n: Int
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("\(n)")
                .font(.mono(12, .medium))
                .foregroundStyle(Color.textMuted)
                .frame(width: 22, height: 22)
                .background(Circle().strokeBorder(Color.lineStrong))
                .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5 }
            Text(text).font(.callout)
        }
    }
}

/// The camera with a frame to aim at, and a way out.
private struct ScanSheet: View {
    let found: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack {
            ScannerView(found: found).ignoresSafeArea()
            VStack(spacing: 20) {
                HStack {
                    Spacer()
                    Button { dismiss() } label: { Icon("cancel01", size: 20).padding(10) }
                        .foregroundStyle(.white)
                        .background(Circle().fill(.black.opacity(0.5)))
                        .accessibilityLabel("Close")
                }
                Spacer()
                RoundedRectangle(cornerRadius: Radius.dialog)
                    .strokeBorder(Color.amber, lineWidth: 2)
                    .frame(width: 240, height: 240)
                Text("Point the camera at the code in Arakun's Phone tab.")
                    .font(.callout)
                    .foregroundStyle(.white)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 32)
                Spacer()
            }
            .padding(20)
        }
        .background(Color.black)
    }
}
