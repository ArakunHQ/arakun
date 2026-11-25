// Which sheet is open is a step in the history, `?sheet=` on the session's
// address, so the phone's back gesture closes it (and goes from a diff back
// to the list of changes) as it did in the old app.js. What the sheet shows beyond
// its kind (the diff picked, the shortcut being written) stays in memory: a
// reload without it falls back to the list it came from.

import { useRouter, useSearch } from "@tanstack/react-router";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ClientMessage } from "../../lib/model";
import type { SheetState } from "./Sheet";

export type SheetKind = SheetState["kind"];
const KINDS: readonly string[] = ["changes", "diff", "settings", "cuts", "cut"];

declare module "@tanstack/history" {
  interface HistoryState {
    /** Set on a step that opened a sheet: the sheet it was opened over, "" for none. */
    sheetFrom?: string;
  }
}

/** The session route's search: only a sheet it knows. */
export function validateSessionSearch(search: Record<string, unknown>): { sheet?: SheetKind } {
  return typeof search.sheet === "string" && KINDS.includes(search.sheet) ? { sheet: search.sheet as SheetKind } : {};
}

type Diff = Extract<SheetState, { kind: "diff" }>;
type CutForm = Extract<SheetState, { kind: "cut" }>;

/** The sheet shown over session `id`, and `go` to open another or close it (null). */
export function useSheetNav(id: string, send: (m: ClientMessage) => void) {
  const router = useRouter();
  const kind = useSearch({ strict: false, select: (s: Record<string, unknown>) => validateSessionSearch(s).sheet ?? null });
  const [diff, setDiff] = useState<Diff | null>(null);
  const [cut, setCut] = useState<CutForm | null>(null);

  let sheet: SheetState | null = null;
  if (kind === "diff") sheet = diff ?? { kind: "changes" };
  else if (kind === "cut") sheet = cut ?? { kind: "cuts" };
  else if (kind) sheet = { kind };

  // Each sheet asks the computer for what it shows as it comes up, also when
  // the back gesture brings it back.
  const asked = useRef("");
  const key = sheet ? (sheet.kind === "diff" ? `diff:${JSON.stringify(sheet.diffs)}` : sheet.kind) : "";
  useEffect(() => {
    if (key === asked.current) return;
    asked.current = key;
    if (key === "changes") send({ type: "changes", session: id });
    else if (key === "settings") send({ type: "settings", session: id });
    else if (key.startsWith("diff:")) for (const [entry, index] of JSON.parse(key.slice(5)) as [number, number][]) send({ type: "diff", session: id, entry, index });
  }, [key, id, send]);

  const from = sheet?.kind ?? "";
  const go = useCallback(
    (next: SheetState | null) => {
      if (next?.kind === "diff") setDiff(next);
      if (next?.kind === "cut") setCut(next);
      const to = next?.kind ?? "";
      // Deeper: a sheet over the session, a diff from its list, a shortcut from the list.
      const deeper = (!from && !!to) || (from === "changes" && to === "diff") || (from === "cuts" && to === "cut");
      // Back to where this step came from: the history goes back, so the
      // phone's back and the screen's stay one and the same.
      if (!deeper && router.state.location.state.sheetFrom === to) return router.history.back();
      void router.navigate({
        to: "/s/$session",
        params: { session: id },
        search: to ? { sheet: to } : {},
        replace: !deeper,
        state: deeper ? { sheetFrom: from } : {},
      });
    },
    [router, id, from],
  );

  return { sheet, go };
}
