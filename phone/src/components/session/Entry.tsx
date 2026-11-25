// One line of the transcript, as entryHtml() in the old app.js drew it: a prompt of
// yours, the agent's words, a tool call or thought, a plan, a notice.

import { createContext, Fragment, memo, type ReactNode, useContext } from "react";
import { clock, fmtElapsed } from "../../lib/format";
import { Icon } from "../../lib/icons";
import { Markdown } from "../../lib/markdown";
import type { Entry } from "../../lib/model";

/** What an entry needs from the screen: pictures, the full-screen views, the changes sheet. */
export interface EntryHandlers {
  /** Draws a picture of the transcript; through the relay it is fetched apart. */
  renderPic: (url: string, alt: string) => ReactNode;
  onPic: (url: string, alt: string) => void;
  onTable: (table: HTMLTableElement) => void;
  onDiagram: (lang: string, code: string) => ReactNode | null;
  openChanges: () => void;
}

export const EntryContext = createContext<EntryHandlers | null>(null);

export interface Edits {
  files: number;
  added: number;
  removed: number;
}

// What the agent does between its words: tool calls and thoughts, drawn as one
// branch of a tree, each step on an elbow off the same line.
const isStep = (e?: Entry) => !!e && (e.type === "tool" || e.type === "thought");

export function entryClass(e: Entry, prev?: Entry, next?: Entry): string {
  let c = `entry ${String(e.type)}${e.type === "notice" && e.error ? " error" : ""}`;
  if (isStep(e)) c += ` step${isStep(prev) ? "" : " first"}${isStep(next) ? "" : " last"}`;
  // What comes first after your words carries who is talking.
  if (e.type !== "user" && prev && prev.type === "user") c += " lead";
  return c;
}

const str = (v: unknown) => (typeof v === "string" ? v : "");
const len = (v: unknown) => (Array.isArray(v) ? v.length : 0);

/** Rough height before an entry is measured; close enough to keep scrolling smooth. */
export function estimate(e: Entry): number {
  const n = (str(e.text) || str(e.output) || str(e.title)).length;
  switch (e.type) {
    case "tool":
      return 38 + (e.status === "failed" ? 120 : 0);
    case "thought":
      return 38;
    case "plan":
      return 20 + 30 * (len(e.entries) || 1);
    case "notice":
    case "compacted":
      return 28;
    case "agent": {
      // A drawing is much taller than the lines of its source.
      const drawn = str(e.text).match(/^\s*```(chart|mermaid)\b[\s\S]*?^\s*```/gim) || [];
      const rest = n - drawn.reduce((a, b) => a + b.length, 0);
      return 30 + 21 * Math.ceil(rest / 42) + 330 * drawn.length;
    }
    case "user": {
      const k = len(e.images);
      const pics = k ? (k === 1 ? 228 : 128 * Math.ceil(k / 2)) : 0;
      return (n ? 30 + 21 * Math.ceil(n / 42) : 16) + pics + 34 * len(e.pastes);
    }
    default:
      return 30 + 21 * Math.ceil(n / 42);
  }
}

/** The files the turn of prompt `at` changed: its shell edits and what its tools edited. */
export function turnEdits(entries: Entry[], at: number): Edits {
  const prompt = entries[at];
  const files = new Set<string>();
  let added = 0;
  let removed = 0;
  for (const c of (prompt.edits as { path: string; added?: number; removed?: number }[] | undefined) ?? []) {
    files.add(c.path);
    added += c.added || 0;
    removed += c.removed || 0;
  }
  for (let i = at + 1; i < entries.length && entries[i].type !== "user"; i++) {
    const t = entries[i];
    const diffs = t.diffs as { path: string }[] | undefined;
    if (t.type !== "tool" || t.status !== "completed" || !diffs?.length) continue;
    for (const d of diffs) files.add(d.path);
    added += Number(t.added) || 0;
    removed += Number(t.removed) || 0;
  }
  return { files: files.size, added, removed };
}

/** "+3 −2", each part in its color; nothing when both are zero. */
export function Counts({ added, removed }: { added?: number; removed?: number }) {
  if (!added && !removed) return null;
  return (
    <span className="stats">
      {added ? <span className="add">+{added}</span> : null} {removed ? <span className="del">−{removed}</span> : null}
    </span>
  );
}

interface Pic {
  name: string;
  url: string;
}

function UserEntry({ e, edits }: { e: Entry; edits: Edits | null }) {
  const h = useContext(EntryContext);
  const meta: string[] = [];
  if (e.at) meta.push(clock(Number(e.at)));
  if (e.elapsed_ms) meta.push(`Worked for ${fmtElapsed(Number(e.elapsed_ms))}`);
  const images = (e.images as Pic[] | undefined) ?? [];
  const pastes = (e.pastes as { lines: number; text: string }[] | undefined) ?? [];
  return (
    <>
      <div className="bubble">
        {e.text ? <div className="text">{str(e.text)}</div> : null}
        {/* Fixed heights: the list measures the bubble before the pictures arrive. */}
        {images.length ? (
          <div className={`pics ${images.length === 1 ? "one" : ""}`}>
            {images.map((p, k) => (
              <Fragment key={k}>{h ? h.renderPic(p.url, p.name) : <img src={p.url} alt={p.name} className="pic" loading="lazy" />}</Fragment>
            ))}
          </div>
        ) : null}
        {pastes.map((p, k) => (
          <details key={k} className="paste">
            <summary>
              Pasted text · {p.lines} line{p.lines === 1 ? "" : "s"}
            </summary>
            <pre>{p.text}</pre>
          </details>
        ))}
      </div>
      {meta.length || edits?.files ? (
        <div className="meta">
          <span>{meta.join(" · ")}</span>
          {edits?.files ? (
            <button type="button" className="tchip" onClick={() => h?.openChanges()}>
              <Icon name="diff" size={12} />
              <span>
                {edits.files} file{edits.files === 1 ? "" : "s"}
              </span>
              <Counts added={edits.added} removed={edits.removed} />
            </button>
          ) : null}
        </div>
      ) : null}
    </>
  );
}

function ToolEntry({ e }: { e: Entry }) {
  const locations = (e.locations as string[] | undefined) ?? [];
  const diffs = (e.diffs as { path: string; added: number; removed: number }[] | undefined) ?? [];
  const failedExit = e.exit_code != null && e.exit_code !== 0 ? `\n[exit ${String(e.exit_code)}]` : "";
  return (
    <details className={`tool ${str(e.status)}`} open={e.status === "failed"}>
      <summary>
        <span className="ttitle">
          {e.command ? (
            <>
              Ran <code>{str(e.command)}</code>
            </>
          ) : (
            str(e.title)
          )}
        </span>
        {e.added || e.removed ? (
          <span className="stats">
            <span className="add">+{Number(e.added) || 0}</span> <span className="del">−{Number(e.removed) || 0}</span>
          </span>
        ) : null}
        <span className="st" />
      </summary>
      {diffs.length ? (
        <div className="files">
          {diffs.map((d, k) => (
            <Fragment key={k}>
              <code>{String(d.path).split(/[\\/]/).pop()}</code> <span className="add">+{d.added}</span> <span className="del">−{d.removed}</span>
            </Fragment>
          ))}
        </div>
      ) : null}
      {locations.length ? (
        <div className="files">
          {locations.map((f, k) => (
            <code key={k}>{f}</code>
          ))}
        </div>
      ) : null}
      {e.output ? (
        <pre>
          {str(e.output)}
          {failedExit}
        </pre>
      ) : null}
    </details>
  );
}

function Body({ e, edits }: { e: Entry; edits: Edits | null }) {
  const h = useContext(EntryContext);
  switch (e.type) {
    case "user":
      return <UserEntry e={e} edits={edits} />;
    case "agent":
      return (
        <div className="md">
          <Markdown src={str(e.text)} renderPic={h?.renderPic} onPic={h?.onPic} onTable={h?.onTable} onDiagram={h?.onDiagram} />
        </div>
      );
    case "thought":
      return (
        <details className="tool thought">
          <summary>
            <span className="ttitle">Thought</span>
            <span className="st" />
          </summary>
          <div className="body">{str(e.text)}</div>
        </details>
      );
    case "tool":
      return <ToolEntry e={e} />;
    case "plan":
      return (
        <ul>
          {((e.entries as { status: string; content: string }[] | undefined) ?? []).map((p, k) => (
            <li key={k} className={p.status}>
              <span className="box">{p.status === "completed" ? "✓" : ""}</span>
              <span>{p.content}</span>
            </li>
          ))}
        </ul>
      );
    case "notice":
      return <>{str(e.text)}</>;
    case "compacted":
      return <>Context compacted{e.before != null && e.after != null ? ` · ${String(e.before)}% → ${String(e.after)}%` : ""}</>;
    default:
      return <>{String(e.type)}</>;
  }
}

/** The inside of an entry; drawn again only when the entry itself changes. */
export const EntryBody = memo(Body, (a, b) => a.e === b.e && a.edits?.files === b.edits?.files && a.edits?.added === b.edits?.added && a.edits?.removed === b.edits?.removed);
