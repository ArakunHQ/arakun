import ArakunKit
import SwiftUI

/// Both agents' plan limits, as the computer reads them: a card per agent with
/// a bar per window that is running. The same as the Usage page on the phone page.
struct PlanLimitsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if let cards = model.planLimits {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 14) {
                            ForEach(cards) { PlanCardView(card: $0) }
                            footer(cards)
                        }
                        .padding(16)
                    }
                    .refreshable { model.loadPlanLimits() }
                } else {
                    VStack(spacing: 12) {
                        if model.isLinked {
                            ProgressView().tint(Color.textMuted)
                            Text("Reading usage…").font(.callout).foregroundStyle(Color.textMuted)
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
            .navigationTitle("Plan limits")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(Color.ground, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { model.loadPlanLimits() } label: { Icon("refresh01", size: 20) }
                        .foregroundStyle(Color.text)
                        .disabled(!model.isLinked)
                        .accessibilityLabel("Refresh")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationBackground(Color.ground)
        // Asked when the sheet opens, again when the link comes back, and every
        // minute while it is open: the computer reads them again when they are stale.
        .task(id: model.isLinked) {
            guard model.isLinked else { return }
            while !Task.isCancelled {
                model.loadPlanLimits()
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }

    private func footer(_ cards: [PlanCard]) -> some View {
        let at = cards.compactMap(\.at).max()
        return TimelineView(.periodic(from: .now, by: 30)) { _ in
            Text("Read from the agents' own bookkeeping on the computer, as the sidebar in Arakun shows them."
                + (at.map { $0.shortAgo == "now" ? " Updated now." : " Updated \($0.shortAgo) ago." } ?? ""))
                .font(.small)
                .foregroundStyle(Color.textFaint)
        }
        .padding(.horizontal, 4)
    }
}

/// One agent's card.
private struct PlanCardView: View {
    let card: PlanCard

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Icon(card.agent == "claude" ? "ai-idea" : "bot", size: 20).foregroundStyle(Color.textMuted)
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.name).font(.geist(16, .semibold)).foregroundStyle(Color.text)
                    if !card.signedIn {
                        Text("Not signed in").font(.small).foregroundStyle(Color.textFaint)
                    } else if let plan = card.plan {
                        Text(plan).font(.small).foregroundStyle(Color.textMuted)
                    }
                }
                Spacer(minLength: 0)
            }
            if card.signedIn {
                ForEach(Array(card.rows.enumerated()), id: \.offset) { _, r in LimitRow(row: r) }
                if let b = card.balance {
                    HStack {
                        Text("Extra usage balance")
                        Spacer()
                        Text(b).font(.mono(13))
                    }
                    .font(.small)
                    .foregroundStyle(Color.textMuted)
                }
                if !card.idle.isEmpty {
                    Text("Not active: \(card.idle.joined(separator: ", "))")
                        .font(.small)
                        .foregroundStyle(Color.textFaint)
                }
            }
            if let note = card.note {
                Text(note)
                    .font(.small)
                    .foregroundStyle(card.tone == "danger" ? Color.failure : Color.textMuted)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
    }
}

/// One window: its name, how full it is and when it resets, and a bar.
private struct LimitRow: View {
    let row: PlanCard.Row

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(row.label).font(.geist(14, .medium)).foregroundStyle(Color.text)
                Spacer(minLength: 0)
                Text(row.detail.map { "\(row.percent)% · \($0)" } ?? "\(row.percent)%")
                    .font(.mono(12))
                    .foregroundStyle(row.percent >= 100 ? Color.failure : Color.textMuted)
                    .lineLimit(1)
            }
            GeometryReader { g in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.line)
                    Capsule().fill(tone).frame(width: g.size.width * CGFloat(min(100, row.percent)) / 100)
                }
            }
            .frame(height: 6)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(row.label): \(row.percent) percent used" + (row.detail.map { ", \($0)" } ?? ""))
    }

    /// Calm until it fills: amber from 70%, red when full.
    private var tone: Color {
        if row.percent >= 100 { return .failure }
        if row.percent >= 70 { return .attention }
        return .textMuted
    }
}
