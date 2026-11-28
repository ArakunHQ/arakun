import ArakunKit
import SwiftUI

/// The session's model and reasoning, as it runs, in a sheet over it. A pick
/// applies from the next message on.
struct SessionSettingsView: View {
    let id: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    /// Shown at once; the computer's answer confirms it.
    @State private var picked: [String: String] = [:]

    var body: some View {
        let s = model.session?.id == id ? model.session : nil
        NavigationStack {
            Group {
                if let sets = model.settings {
                    if sets.isEmpty {
                        Text("This agent has not said what it offers yet. Try again in a moment.")
                            .font(.callout)
                            .foregroundStyle(Color.textMuted)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                            .padding(.horizontal, 24)
                            .padding(.top, 60)
                    } else {
                        ScrollView {
                            VStack(alignment: .leading, spacing: 24) {
                                ForEach(sets) { set in
                                    SettingRows(set: set, picked: picked[set.key], enabled: model.isLinked) { option in
                                        if model.setSetting(set.key, to: option) { picked[set.key] = option }
                                    }
                                }
                                Text("From the next message on.")
                                    .font(.small)
                                    .foregroundStyle(Color.textFaint)
                            }
                            .padding(16)
                        }
                    }
                } else {
                    VStack(spacing: 12) {
                        if model.isLinked {
                            ProgressView().tint(Color.textMuted)
                        } else {
                            // The request went nowhere; it is sent again when the link is back.
                            Text("Waiting for the computer").font(.callout).foregroundStyle(Color.textMuted)
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                    .padding(.top, 60)
                }
            }
            .background(Color.ground.ignoresSafeArea())
            .navigationTitle("Model and reasoning")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(Color.ground, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationBackground(Color.ground)
        // Asked when the sheet opens and again when the link comes back: a
        // request sent offline is dropped.
        .task(id: model.isLinked) { model.loadSettings() }
        // The computer's answer is what the session runs with now.
        .onChange(of: model.settings) { _, _ in picked = [:] }
        // The session went away under the sheet: there is nothing left to show.
        .onChange(of: s == nil) { _, gone in if gone { dismiss() } }
    }
}

/// One setting's choices as a list with the current one ticked, for the
/// session's sheet and the New session form.
struct SettingRows: View {
    let set: SettingSet
    /// Picked here and not yet confirmed, or not yet sent.
    let picked: String?
    var enabled = true
    let pick: (String) -> Void

    var body: some View {
        let current = picked ?? set.current
        VStack(alignment: .leading, spacing: 10) {
            Eyebrow(set.label.isEmpty ? set.key : set.label)
            VStack(spacing: 0) {
                ForEach(Array(set.options.enumerated()), id: \.element.id) { i, o in
                    if i > 0 { Rectangle().fill(Color.line).frame(height: 1).padding(.leading, 44) }
                    row(o, on: o.id == current)
                }
            }
            .card()
        }
    }

    private func row(_ o: SettingSet.Option, on: Bool) -> some View {
        Button {
            if !on { pick(o.id) }
        } label: {
            HStack(spacing: 12) {
                Group {
                    if on { Icon("tick02", size: 18).foregroundStyle(Color.amberInk) } else { Color.clear }
                }
                .frame(width: 18, height: 18)
                VStack(alignment: .leading, spacing: 3) {
                    Text(o.name).font(.geist(15, .medium)).foregroundStyle(Color.text)
                    if let d = o.disabled, !d.isEmpty {
                        Text(d).font(.small).foregroundStyle(Color.textFaint)
                    } else if let d = o.detail, !d.isEmpty {
                        Text(d).font(.small).foregroundStyle(Color.textMuted)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(minHeight: 48)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(o.isDisabled || !enabled)
        .opacity(o.isDisabled ? 0.5 : 1)
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityLabel(o.name)
        .accessibilityHint(o.disabled ?? o.detail ?? "")
    }
}
