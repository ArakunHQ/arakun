// What opens over the session in its own chrome: the files it changed and
// each diff (read-only), the model and reasoning, and the shortcuts.

import { useEffect, useRef, useState } from "react";
import { ago } from "../../lib/format";
import { Icon } from "../../lib/icons";
import { useLink } from "../../lib/link";
import type { ClientMessage, Session } from "../../lib/model";
import { Counts } from "./Entry";
import { CutSheet, type CutSeed, CutsSheet } from "./Shortcuts";
import { SheetHeader } from "./Views";
import "./Sheet.css";

export type SheetState =
  | { kind: "changes" }
  /** `diffs` are the file's edits in the turn, as [entry, index] pairs; all are shown, in order. */
  | { kind: "diff"; name: string; diffs: [number, number][] }
  | { kind: "settings" }
  | { kind: "cuts" }
  | { kind: "cut"; seed: CutSeed; fromList: boolean };

interface Turn {
  text?: string;
  at?: number;
  files: { name: string; path: string; diffs: [number, number][]; added?: number; removed?: number }[];
}

interface Diff {
  path: string;
  new_file?: boolean;
  deleted?: boolean;
  added?: number;
  removed?: number;
  /** A hunk header as a string, else [mark, line number, text]. */
  lines: (string | [string, number | null, string])[];
  cut?: number;
}

interface SettingSet {
  key: string;
  label: string;
  current?: string;
  options: { id: string; name: string; detail?: string; disabled?: boolean }[];
}

export interface SheetProps {
  sheet: SheetState;
  session: Session;
  send: (m: ClientMessage) => void;
  /** Opens another sheet; null closes. The opener sends what the sheet asks for. */
  open: (next: SheetState | null) => void;
  /** What is written, for a new shortcut. */
  draft: string;
}

const loading = (
  <div className="empty">
    <div className="hint">Loading…</div>
  </div>
);

function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

function ChangesSheet({ session: s, send, open }: SheetProps) {
  const turns = useLink((st) => st.changes) as Turn[] | null;
  // The turn just added some: ask for the list again.
  const changes = JSON.stringify(s.changes ?? null);
  const first = useRef(changes);
  useEffect(() => {
    if (changes !== first.current) send({ type: "changes", session: s.id });
    first.current = changes;
  }, [changes, s.id, send]);
  let body;
  if (!turns) body = loading;
  else if (!turns.length)
    body = (
      <div className="empty">
        <div className="big">✓</div>
        <div>No files changed</div>
      </div>
    );
  else
    body = (
      <div className="sheet-body">
        {turns.map((t, k) => (
          <div key={k} className="turn">
            <div className="turn-h">
              <span className="turn-t">{t.text || <i>Prompt</i>}</span>
              <span className="turn-when">{t.at ? ago(t.at) : ""}</span>
            </div>
            {t.files.map((f, j) => (
              <button key={j} type="button" className="frow" onClick={() => open({ kind: "diff", name: f.name, diffs: f.diffs })}>
                <span className="fname">
                  <span className="n">{f.name}</span>
                  <span className="d">{dirOf(f.path)}</span>
                </span>
                {f.diffs.length > 1 ? <span className="more">{f.diffs.length} edits</span> : null}
                <Counts added={f.added} removed={f.removed} />
              </button>
            ))}
          </div>
        ))}
      </div>
    );
  return (
    <div className="sheet">
      <SheetHeader title="Changes" sub={s.title || "This session"} onBack={() => open(null)} />
      {body}
    </div>
  );
}

function DiffLine({ l }: { l: Diff["lines"][number] }) {
  if (typeof l === "string") return <div className="dl hunk">{l}</div>;
  const [mark, num, text] = l;
  const k = mark === "+" ? "a" : mark === "-" ? "r" : "c";
  return (
    <div className={`dl ${k}`}>
      <span className="ln">{num ?? ""}</span>
      <span className="mk">{mark === " " ? "" : mark === "-" ? "−" : "+"}</span>
      <span className="tx">{text || " "}</span>
    </div>
  );
}

function DiffSheet({ sheet, open }: SheetProps & { sheet: Extract<SheetState, { kind: "diff" }> }) {
  const got = useLink((st) => st.diffs) as unknown as Record<string, Diff>;
  const n = sheet.diffs.length;
  return (
    <div className="sheet">
      {/* A diff goes back to the list it was picked from. */}
      <SheetHeader title={sheet.name} sub="Read-only · edit on the computer" onBack={() => open({ kind: "changes" })} />
      <div className="sheet-body">
        {sheet.diffs.map(([e, i], k) => {
          const d = got[`${e}:${i}`];
          const label = n > 1 ? <span className="dnth">{`Edit ${k + 1} of ${n}`}</span> : null;
          if (!d)
            return (
              <div key={k} className="dblock">
                <div className="dmeta">
                  {label}
                  <span className="dnote">Loading…</span>
                </div>
              </div>
            );
          const note = d.new_file ? "New file" : d.deleted ? "Deleted" : "";
          return (
            <div key={k} className="dblock">
              <div className="dmeta">
                {label}
                {k === 0 ? <span className="dpath">{d.path}</span> : null}
                {note ? <span className="dnote">{note}</span> : null}
                <Counts added={d.added} removed={d.removed} />
              </div>
              <div className="dwrap">
                <div className="dlines">
                  {d.lines.map((l, j) => (
                    <DiffLine key={j} l={l} />
                  ))}
                </div>
              </div>
              {d.cut ? <div className="dcut">… {d.cut} more lines</div> : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// The session's model and reasoning, as it runs; a pick applies from the next message.
function SettingsSheet({ session: s, send, open }: SheetProps) {
  const sets = useLink((st) => st.settings) as SettingSet[] | null;
  // Shown at once; the computer's answer confirms it.
  const [picked, setPicked] = useState<Record<string, string>>({});
  let body;
  if (!sets) body = loading;
  else if (!sets.length)
    body = (
      <div className="empty">
        <div className="hint">This agent has not said what it offers yet. Try again in a moment.</div>
      </div>
    );
  else
    body = sets.map((set) => {
      const current = picked[set.key] ?? set.current;
      return (
        <div key={set.key}>
          <div className="glabel">{set.label}</div>
          <div className="opts">
            {set.options.map((o) => (
              <button
                key={o.id}
                type="button"
                className={`opt${o.id === current ? " on" : ""}`}
                disabled={o.disabled}
                onClick={() => {
                  send({ type: "setting", session: s.id, key: set.key, value: o.id });
                  setPicked({ ...picked, [set.key]: o.id });
                }}
              >
                <span className="on-mark">{o.id === current ? <Icon name="check" /> : null}</span>
                <span className="oname">{o.name}</span>
                {o.detail ? <span className="odetail">{o.detail}</span> : null}
              </button>
            ))}
          </div>
        </div>
      );
    });
  return (
    <div className="sheet">
      <SheetHeader title="Model and reasoning" sub="From the next message on" onBack={() => open(null)} />
      <div className="sheet-body setform">{body}</div>
    </div>
  );
}

export function Sheet(props: SheetProps) {
  const { sheet, open, draft } = props;
  switch (sheet.kind) {
    case "changes":
      return <ChangesSheet {...props} />;
    case "diff":
      return <DiffSheet {...props} sheet={sheet} />;
    case "settings":
      return <SettingsSheet {...props} />;
    case "cuts":
      return (
        <CutsSheet
          onBack={() => open(null)}
          onEdit={(c) => open({ kind: "cut", seed: c, fromList: true })}
          onNew={() => open({ kind: "cut", seed: { text: draft.trim() }, fromList: true })}
        />
      );
    case "cut":
      return <CutSheet key={sheet.seed.id ?? ""} seed={sheet.seed} fromList={sheet.fromList} onDone={() => open(sheet.fromList ? { kind: "cuts" } : null)} />;
  }
}
