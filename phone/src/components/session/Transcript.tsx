// The transcript: only the entries near the viewport are drawn, placed by
// translateY from summed heights. Heights are guessed first (`estimate`) and
// measured once painted; a wrong guess just moves what is below. A port of
// VList in the old app.js, with the same rules:
//
// - The scroll position is never written while the list moves: on Android any
//   write to scrollTop cancels the fling in progress, and a drag fights it.
// - Entries are placed from an anchor, an entry and its y. Older entries go
//   above it, into the room reserved for what is not loaded yet, so nothing
//   on screen moves when they arrive. A height that changes above the
//   viewport moves the anchor the other way, not the scroll.
// - Only once the scroll rests, with no finger down, the reserve is resized
//   to what is still unloaded, moving the scroll by the same amount in the
//   same frame, which is invisible.
// - The list sticks to the end only when content grows while it is there,
//   never in answer to a scroll.

import { memo, type ReactNode, type Ref, useEffect, useImperativeHandle, useLayoutEffect, useReducer, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import type { Entry, Session } from "../../lib/model";
import { type Edits, EntryBody, entryClass, estimate, turnEdits } from "./Entry";

/** Entries are asked for when the loaded ones come this close to the viewport. */
export const LOAD_AHEAD = 1500;
/** Each unloaded entry reserves this much more than the loaded average, so the reserve rarely runs short. */
const RESERVE_MARGIN = 1.3;
/** Silence this long after the last scroll event means the scroll rests. */
export const REST_MS = 200;
/** This close to the bottom still counts as at the end. */
export const END_SLACK = 24;
/** Drawn beyond the viewport on each side. */
const OVERDRAW = 600;
/** Before the list has a size (the first frame, or no layout at all), the newest this many are drawn. */
const FIRST_DRAW = 30;
const HOLD_MS = 600;

export interface TranscriptHandle {
  scrollToEnd(): void;
}

export interface TranscriptProps {
  session: Session;
  loadingOlder: boolean;
  onLoadOlder: () => void;
  /** What was sent and the computer has not shown yet, under the last entry. */
  pending: ReactNode;
  /** A message of yours was held: offer to keep it as a shortcut. */
  onHoldBubble: (text: string) => void;
  ref?: Ref<TranscriptHandle>;
}

interface RowProps {
  abs: number;
  y: number;
  cls: string;
  who: string;
  e: Entry;
  edits: Edits | null;
  open: boolean[] | undefined;
}

const Row = memo(
  function Row({ abs, y, cls, who, e, edits, open }: RowProps) {
    const node = useRef<HTMLDivElement>(null);
    // Its <details> as they were before it scrolled away.
    useLayoutEffect(() => {
      if (open) node.current?.querySelectorAll("details").forEach((d, k) => (d.open = !!open[k]));
      // Only when it is drawn anew.
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);
    return (
      <div ref={node} className={cls} data-abs={abs} style={{ transform: `translateY(${y}px)` }}>
        {who ? <div className="who">{who}</div> : null}
        <EntryBody e={e} edits={edits} />
      </div>
    );
  },
  (a, b) =>
    a.abs === b.abs &&
    a.y === b.y &&
    a.cls === b.cls &&
    a.who === b.who &&
    a.e === b.e &&
    a.edits?.files === b.edits?.files &&
    a.edits?.added === b.edits?.added &&
    a.edits?.removed === b.edits?.removed,
);

function List({ session: s, loadingOlder, onLoadOlder, pending, onHoldBubble, ref }: TranscriptProps) {
  const log = useRef<HTMLDivElement>(null);
  const spacer = useRef<HTMLDivElement>(null);
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  const [unseen, setUnseen] = useState(0);
  const v = useRef({
    anchorAbs: -1,
    anchorY: 0,
    // Measured heights, valid while the entry is the same object.
    heights: new Map<number, { e: Entry; h: number }>(),
    open: new Map<number, boolean[]>(),
    atEnd: true,
    grew: true,
    touching: false,
    top: 0,
    adjust: 0,
    frame: 0,
    rest: undefined as ReturnType<typeof setTimeout> | undefined,
    hold: undefined as ReturnType<typeof setTimeout> | undefined,
    firstShown: -1,
    prevFrom: s.from,
    prevEnd: s.from + s.entries.length,
    prevEntries: s.entries,
    prevPending: pending,
  }).current;
  // The newest of what changes on every render, for the listeners.
  const now = useRef({ s, loadingOlder, onLoadOlder, onHoldBubble });
  now.current = { s, loadingOlder, onLoadOlder, onHoldBubble };

  const heightOf = (abs: number) => {
    const { s } = now.current;
    const e = s.entries[abs - s.from];
    const got = v.heights.get(abs);
    return got && got.e === e ? got.h : estimate(e);
  };
  const total = () => {
    const { s } = now.current;
    let y = 0;
    for (let i = 0; i < s.entries.length; i++) y += heightOf(s.from + i);
    return y;
  };
  const avg = () => {
    const n = now.current.s.entries.length;
    return n ? Math.max(40, total() / n) : 80;
  };
  const reserveFor = (unloaded: number) => Math.round(unloaded * avg() * RESERVE_MARGIN);
  // Where entry `abs` sits, from the anchor.
  const yOf = (abs: number) => {
    let y = v.anchorY;
    if (abs >= v.anchorAbs) for (let k = v.anchorAbs; k < abs; k++) y += heightOf(k);
    else for (let k = abs; k < v.anchorAbs; k++) y -= heightOf(k);
    return y;
  };

  const schedule = () => {
    if (v.frame) return;
    v.frame = requestAnimationFrame(() => {
      v.frame = 0;
      redraw();
    });
  };
  const loadAhead = () => {
    const el = log.current;
    const { s, loadingOlder, onLoadOlder } = now.current;
    if (el && s.from > 0 && !loadingOlder && el.scrollTop - yOf(s.from) < LOAD_AHEAD) onLoadOlder();
  };
  // At rest: the reserve above the first entry fits what is still unloaded.
  const settle = () => {
    if (!log.current || v.touching) return;
    const unloaded = now.current.s.from;
    const target = reserveFor(unloaded);
    const b = yOf(unloaded);
    // A reserve that runs short (owed room above the top), none left when all
    // is loaded, or one far from the mark: fix it. Small drift can wait.
    const off = b < 0 || (unloaded === 0 && b !== 0) || (unloaded > 0 && (b < target * 0.5 || b > target * 2));
    if (!off) return;
    v.anchorY += target - b;
    v.adjust += target - b;
    redraw();
  };
  const armRest = () => {
    clearTimeout(v.rest);
    v.rest = setTimeout(settle, REST_MS);
  };
  const scrollToEnd = () => {
    const el = log.current;
    v.atEnd = true;
    v.grew = true;
    clearTimeout(v.rest);
    setUnseen(0);
    if (el) {
      el.scrollTop = el.scrollHeight;
      v.top = el.scrollTop;
    }
    schedule();
  };
  useImperativeHandle(ref, () => ({ scrollToEnd }));

  // ---- what changed since the last draw ----
  const len = s.entries.length;
  const end = s.from + len;
  if (len === 0 || v.anchorAbs < s.from || v.anchorAbs >= end) {
    // Nothing loaded, or none of what was: start over at the end.
    v.anchorAbs = s.from;
    v.anchorY = reserveFor(s.from);
    v.atEnd = true;
    v.grew = true;
  }
  if (s.entries !== v.prevEntries) {
    v.grew = true;
    // Opened again after a reconnect, the computer sends entries this list
    // has already: equal ones keep their measured height.
    for (const [abs, got] of v.heights) {
      const e = s.entries[abs - s.from];
      if (!e) v.heights.delete(abs);
      else if (got.e !== e && JSON.stringify(got.e) === JSON.stringify(e)) got.e = e;
    }
  }
  if (pending !== v.prevPending) v.grew = true;

  // ---- placing ----
  const el = log.current;
  const height = el?.clientHeight ?? 0;
  const top = v.top - OVERDRAW;
  const bottom = v.top + height + OVERDRAW;
  const rows: ReactNode[] = [];
  let y = yOf(s.from);
  v.firstShown = -1;
  for (let i = 0; i < len; i++) {
    const abs = s.from + i;
    const h = heightOf(abs);
    const shown = height ? y + h >= top && y <= bottom : i >= len - FIRST_DRAW;
    if (shown) {
      if (v.firstShown < 0 && y + h > v.top) v.firstShown = abs;
      const e = s.entries[i];
      const prev = s.entries[i - 1];
      const cls = entryClass(e, prev, s.entries[i + 1]);
      const who = cls.includes(" lead") ? s.agent || "Agent" : "";
      const edits = e.type === "user" && e.elapsed_ms ? turnEdits(s.entries, i) : null;
      rows.push(<Row key={abs} abs={abs} y={y} cls={cls} who={who} e={e} edits={edits} open={v.open.get(abs)} />);
    }
    y += h;
  }
  // With a negative start the first entries hang above the spacer, out of reach, until settle() brings them back.
  const spacerHeight = Math.max(0, y);

  useLayoutEffect(() => {
    const el = log.current;
    if (!el) return;
    if (v.adjust) {
      // Grown or shrunk above: the scroll moves with it, in this same frame.
      el.scrollTop = v.top + v.adjust;
      v.top = el.scrollTop;
      v.adjust = 0;
    }
    // Older entries came in before the ones loaded, which are the same objects still.
    const older = s.from < v.prevFrom && s.entries[v.prevFrom - s.from] === v.prevEntries[0] && v.prevEntries.length > 0;
    if (older) {
      // Older entries went into the reserve; the scroll may rest inside it:
      // keep filling it, and settle once nothing more is coming.
      v.atEnd = false;
      armRest();
    } else if (end > v.prevEnd && s.from >= v.prevFrom && !v.atEnd) {
      // Read further up while it grew: a pill says so, and goes back down.
      setUnseen((n) => n + end - v.prevEnd);
    }
    v.prevFrom = s.from;
    v.prevEnd = end;
    v.prevEntries = s.entries;
    v.prevPending = pending;
    // Content grew while the list was at the end: keep it there.
    if (v.grew) {
      v.grew = false;
      if (v.atEnd && el.scrollTop + el.clientHeight < el.scrollHeight - 1) {
        el.scrollTop = el.scrollHeight;
        v.top = el.scrollTop;
        schedule();
      }
    }
    loadAhead();
  });

  // Which of its <details> are open, kept while the entry scrolls away. The
  // event does not bubble: listened to on the way down.
  useEffect(() => {
    const el = log.current;
    if (!el) return;
    const toggled = (ev: Event) => {
      const row = (ev.target as Element).closest<HTMLElement>("[data-abs]");
      if (row) v.open.set(Number(row.dataset.abs), [...row.querySelectorAll("details")].map((d) => d.open));
    };
    el.addEventListener("toggle", toggled, true);
    return () => el.removeEventListener("toggle", toggled, true);
  }, [v]);

  // Heights as painted, and the list's own size.
  useEffect(() => {
    const el = log.current;
    const sp = spacer.current;
    if (!el || !sp || typeof ResizeObserver === "undefined") return;
    const rows = new ResizeObserver((changes) => {
      const { s } = now.current;
      const ref = v.firstShown;
      const before = ref >= 0 ? yOf(ref) : 0;
      let moved = false;
      for (const c of changes) {
        const n = c.target as HTMLElement;
        const abs = Number(n.dataset.abs);
        const e = s.entries[abs - s.from];
        const h = n.offsetHeight;
        if (!e || !h || h === heightOf(abs)) continue;
        v.heights.set(abs, { e, h });
        moved = true;
      }
      if (!moved) return;
      // What is on screen stays put; only the end follows the content.
      if (!v.atEnd && ref >= 0) v.anchorY += before - yOf(ref);
      v.grew = true;
      schedule();
    });
    sp.querySelectorAll<HTMLElement>(":scope > [data-abs]").forEach((n) => rows.observe(n));
    const added = new MutationObserver((changes) => {
      for (const c of changes) {
        c.addedNodes.forEach((n) => n instanceof HTMLElement && rows.observe(n));
        c.removedNodes.forEach((n) => n instanceof HTMLElement && rows.unobserve(n));
      }
    });
    added.observe(sp, { childList: true });
    // The list gets shorter with no scroll event when the keyboard opens or
    // the draft grows: stay on the last entry if that is where the reader was.
    const size = new ResizeObserver(() => {
      if (!v.atEnd || v.touching) return;
      v.grew = true;
      schedule();
    });
    size.observe(el);
    return () => {
      rows.disconnect();
      added.disconnect();
      size.disconnect();
    };
    // The observers read the newest state through refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(
    () => () => {
      cancelAnimationFrame(v.frame);
      v.frame = 0;
      clearTimeout(v.rest);
      clearTimeout(v.hold);
    },
    [v],
  );

  const onScroll = () => {
    const el = log.current!;
    // Being at the end is a position, not a direction: iOS moves the scroll up
    // on its own (the bounce at the bottom, the keyboard, a shorter viewport),
    // and reading those as the reader leaving would stop following along.
    v.atEnd = el.scrollHeight - el.clientHeight - el.scrollTop <= END_SLACK;
    if (v.atEnd) setUnseen(0);
    v.top = el.scrollTop;
    clearTimeout(v.hold);
    armRest();
    loadAhead();
    schedule();
  };
  const lift = () => {
    v.touching = false;
    armRest();
  };
  // Holding a message of yours offers to keep it.
  const press = (ev: React.PointerEvent) => {
    clearTimeout(v.hold);
    const bubble = (ev.target as Element).closest(".entry.user .bubble");
    const text = bubble?.querySelector(".text")?.textContent?.trim() || "";
    if (!text) return;
    v.hold = setTimeout(() => {
      navigator.vibrate?.(10);
      now.current.onHoldBubble(text);
    }, HOLD_MS);
  };
  const drop = () => clearTimeout(v.hold);

  return (
    <div className="logwrap">
      <div
        className="log"
        ref={log}
        onScroll={onScroll}
        onTouchStart={() => {
          v.touching = true;
          clearTimeout(v.rest);
        }}
        onTouchEnd={lift}
        onTouchCancel={lift}
        onPointerDown={press}
        onPointerUp={drop}
        onPointerCancel={drop}
        onPointerLeave={drop}
      >
        <div className="spacer" ref={spacer} style={{ height: `${spacerHeight}px` }}>
          {rows}
        </div>
        <div className="pending">{pending}</div>
      </div>
      {unseen > 0 ? (
        <button type="button" className="newpill" onClick={scrollToEnd}>
          <Icon name="down" />
          <span>{unseen} new</span>
        </button>
      ) : null}
    </div>
  );
}

/** Drawn again only when its props change, not on every key typed in the box. */
export const Transcript = memo(List);
