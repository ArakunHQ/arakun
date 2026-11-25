import ArakunKit
import SwiftUI

/// What a session changed, in a sheet over it: the files turn by turn with the
/// newest first, and each file's diffs.
struct ChangesView: View {
    let id: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let s = model.session?.id == id ? model.session : nil
        NavigationStack {
            Group {
                if let turns = model.changes {
                    if turns.isEmpty {
                        Text("No files changed")
                            .font(.callout)
                            .foregroundStyle(Color.textMuted)
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                            .padding(.top, 60)
                    } else {
                        list(turns)
                    }
                } else {
                    Waiting()
                }
            }
            .background(Color.ground.ignoresSafeArea())
            .navigationTitle("Changes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(Color.ground, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .navigationDestination(for: ChangedFile.self) { DiffView(file: $0) }
        }
        // Asked when the sheet opens, again when a turn changes more, and again
        // when the link comes back: a request sent offline is dropped.
        .task(id: Asked(files: s?.changes.files, added: s?.changes.added, removed: s?.changes.removed, linked: model.isLinked)) {
            model.loadChanges()
        }
        // The session went away under the sheet: there is nothing left to show.
        .onChange(of: s == nil) { _, gone in if gone { dismiss() } }
    }

    private struct Asked: Equatable {
        let files: Int?
        let added: Int?
        let removed: Int?
        let linked: Bool
    }

    private func list(_ turns: [ChangedTurn]) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 18) {
                ForEach(Array(turns.enumerated()), id: \.offset) { _, turn in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 10) {
                            Group {
                                if turn.text.isEmpty { Text("Prompt").italic() } else { Text(turn.text) }
                            }
                            .font(.small)
                            .foregroundStyle(Color.textMuted)
                            .lineLimit(1)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            if let at = turn.at {
                                Text(at.shortAgo).font(.mono(11.5)).foregroundStyle(Color.textFaint)
                            }
                        }
                        .padding(.horizontal, 4)
                        ForEach(Array(turn.files.enumerated()), id: \.offset) { _, f in
                            NavigationLink(value: f) { FileRow(file: f) }.buttonStyle(.plain)
                        }
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
        }
    }
}

/// Shown until the computer answers.
private struct Waiting: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Group {
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

private struct FileRow: View {
    let file: ChangedFile

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(file.name).font(.mono(13.5)).foregroundStyle(Color.text).lineLimit(1).truncationMode(.middle)
                if !file.dir.isEmpty {
                    // The end of a long folder says more than its start.
                    Text(file.dir).font(.caption12).foregroundStyle(Color.textFaint).lineLimit(1).truncationMode(.head)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if file.diffs.count > 1 {
                Text("\(file.diffs.count) edits").font(.caption12).foregroundStyle(Color.textFaint)
            }
            Changes(added: file.added, removed: file.removed)
            Icon("arrow-right01", size: 13).foregroundStyle(Color.textFaint)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .card()
        .contentShape(Rectangle())
    }
}

/// A file's edits in one turn, in order. Read-only: the phone changes no file.
private struct DiffView: View {
    let file: ChangedFile
    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(Array(file.diffs.enumerated()), id: \.offset) { k, ref in
                    let d = model.diffs[ref]
                    meta(d, nth: file.diffs.count > 1 ? "Edit \(k + 1) of \(file.diffs.count)" : nil)
                    if let d {
                        ForEach(Array(d.lines.enumerated()), id: \.offset) { _, l in
                            DiffLineRow(line: l)
                        }
                        if d.cut > 0 {
                            Text("… \(d.cut) more lines")
                                .font(.small)
                                .foregroundStyle(Color.textFaint)
                                .padding(.horizontal, 16)
                                .padding(.vertical, 6)
                        }
                    }
                }
            }
            .padding(.vertical, 8)
        }
        .background(Color.ground.ignoresSafeArea())
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Color.ground, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(file.name).font(.geist(15, .semibold, relativeTo: .headline)).lineLimit(1)
                    Text("Read-only · edit on the computer")
                        .font(.mono(11, relativeTo: .caption2))
                        .foregroundStyle(Color.textMuted)
                        .lineLimit(1)
                }
                .frame(maxWidth: 260)
            }
        }
        // Again when the link comes back, for the ones that did not come.
        .task(id: model.isLinked) { model.loadDiffs(file.diffs) }
    }

    private func meta(_ d: FileDiff?, nth: String?) -> some View {
        HStack(spacing: 10) {
            if let nth {
                Text(nth).font(.geist(13, .semibold, relativeTo: .footnote)).foregroundStyle(Color.textMuted)
            }
            Text(d?.path ?? file.path)
                .font(.mono(11.5))
                .foregroundStyle(Color.textMuted)
                .lineLimit(1)
                .truncationMode(.head)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let d {
                if d.newFile {
                    Text("New file").font(.caption12).foregroundStyle(Color.textFaint)
                } else if d.deleted {
                    Text("Deleted").font(.caption12).foregroundStyle(Color.textFaint)
                }
                Changes(added: d.added, removed: d.removed)
            } else {
                Text(model.isLinked ? "Loading…" : "Waiting for the computer").font(.caption12).foregroundStyle(Color.textFaint)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

/// One line: its number, its mark and its text. A long line wraps, so a diff
/// reads top to bottom on a narrow screen without scrolling sideways.
private struct DiffLineRow: View {
    let line: DiffLine

    var body: some View {
        switch line {
        case .hunk(let header):
            Text(header)
                .font(.mono(11))
                .foregroundStyle(Color.textFaint)
                .lineLimit(1)
                .padding(.horizontal, 16)
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.raised)
        case .text(let mark, let number, let text):
            let tone: Color? = mark == "+" ? Color.success : mark == "-" ? Color.failure : nil
            HStack(alignment: .top, spacing: 0) {
                Text(number.map { String($0) } ?? "")
                    .foregroundStyle(Color.textFaint)
                    .lineLimit(1)
                    .frame(width: 38, alignment: .trailing)
                    .padding(.trailing, 6)
                Text(mark == "+" ? "+" : mark == "-" ? "−" : " ")
                    .foregroundStyle(tone ?? Color.textFaint)
                    .frame(width: 14)
                // An empty line still takes a line's height.
                Text(text.isEmpty ? " " : text)
                    .foregroundStyle(tone ?? Color.textMuted)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .font(.mono(11.5))
            .padding(.trailing, 12)
            .padding(.vertical, 1)
            .background(tone?.opacity(0.12) ?? Color.clear)
            .overlay(alignment: .leading) {
                if let tone { Rectangle().fill(tone).frame(width: 2) }
            }
        }
    }
}
