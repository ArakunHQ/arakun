import ArakunKit
import SwiftUI
import UIKit

/// The first screen: sign in with GitHub to reach the account's computers
/// through the relay, or pair on this network with the code Arakun shows.
struct SignInView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openURL) private var openURL
    @State private var scanning = false
    @State private var pasted = ""
    @FocusState private var pasting: Bool

    var body: some View {
        ScrollView {
            VStack(spacing: 32) {
                VStack(spacing: 16) {
                    Mascot(mood: model.signIn != nil ? .thinking : model.signInError != nil ? .error : .idle, width: 132)
                        .padding(.top, 24)
                    Text("Arakun")
                        .font(.geist(40, .semibold, relativeTo: .largeTitle))
                        .tracking(-1.6)
                    Text("Your coding sessions, on this phone.")
                        .font(.geist(17, relativeTo: .title3))
                        .foregroundStyle(Color.textMuted)
                        .multilineTextAlignment(.center)
                }

                signInCard
                pairCard

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
                pair(code)
            }
        }
    }

    // ---- sign in --------------------------------------------------------------

    private var signInCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            Eyebrow("Sign in to reach your computers")
            Text("Sign in with the GitHub account you use in Arakun on your computer.")
                .font(.callout)
                .foregroundStyle(Color.textMuted)
            switch model.signIn {
            case nil:
                Button { model.startSignIn() } label: {
                    Text("Sign in with GitHub").frame(maxWidth: .infinity)
                }
                .buttonStyle(.primary)
            case .asking?:
                waiting("Asking GitHub for a code…")
            case .code(let code)?:
                codeView(code)
            case .finishing?:
                waiting("Signing in…")
            }
            if let e = model.signInError { Problem(text: e) }
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
    }

    private func codeView(_ code: DeviceCode) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Enter this code on GitHub:").font(.callout)
            Text(code.userCode)
                .font(.mono(34, .medium, relativeTo: .largeTitle))
                .tracking(2)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 14)
                .background(RoundedRectangle(cornerRadius: Radius.control).fill(Color.deep))
                .overlay(RoundedRectangle(cornerRadius: Radius.control).strokeBorder(Color.lineStrong))
                .accessibilityLabel("Code \(code.userCode.map { String($0) }.joined(separator: " "))")
            HStack(spacing: 8) {
                Button("Copy") { UIPasteboard.general.string = code.userCode }
                    .buttonStyle(.outline)
                Button {
                    openURL(AccountClient.githubDevicePage)
                } label: {
                    Text("Open GitHub").frame(maxWidth: .infinity)
                }
                .buttonStyle(.primaryCompact)
            }
            HStack(spacing: 10) {
                ProgressView().tint(Color.textMuted)
                Text("Waiting for GitHub…").foregroundStyle(Color.textMuted)
                Spacer()
                Button("Cancel") { model.cancelSignIn() }.buttonStyle(.outline)
            }
            .font(.callout)
        }
    }

    private func waiting(_ text: String) -> some View {
        HStack(spacing: 10) {
            ProgressView().tint(Color.textMuted)
            Text(text).foregroundStyle(Color.textMuted)
            Spacer()
            Button("Cancel") { model.cancelSignIn() }.buttonStyle(.outline)
        }
        .font(.callout)
        .frame(minHeight: 48)
    }

    // ---- pair on this network ---------------------------------------------------

    private var pairCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            Eyebrow("Pair on this network")
            Text("On the same network or over Tailscale, scan the code in Arakun's Phone tab. The computer's page opens in the browser and pairs there; this app reaches your computers when you sign in.")
                .font(.callout)
                .foregroundStyle(Color.textMuted)
            Button { scanning = true } label: {
                HStack(spacing: 10) {
                    Icon("search01", size: 18)
                    Text("Scan the code")
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.outline)

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
            if let said = model.pairSaid { Problem(text: said) }
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
    }

    private func pair(_ text: String) {
        if let page = model.pair(text) { openURL(page) }
    }

    private func pairPasted() {
        pair(pasted)
        pasted = ""
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
