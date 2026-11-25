// Shortcuts: phrases kept on this phone, to send without typing them again.
// One either fills the box (shown while writing) or sends at once (shown
// while the agent waits). The order is the user's own: a finger learns where
// each one sits.

import { useState } from "react";
import { BLANK, CUT_SHORT, type Cut, cutName, cutSends, loadCuts, storeCuts } from "../../lib/drafts";
import { Icon } from "../../lib/icons";
import { toast } from "../../lib/toast";
import { SheetHeader } from "./Views";
import "./Shortcuts.css";

/** The row of shortcuts over the composer. `writing`: the box has the focus. */
export function CutsRow({ writing, onUse, onMore }: { writing: boolean; onUse: (c: Cut) => void; onMore: () => void }) {
  const shown = loadCuts().filter((c) => cutSends(c) === !writing);
  return (
    // A tap on the row must not take the keyboard down.
    <div className="cuts" onMouseDown={(ev) => ev.preventDefault()}>
      {shown.map((c) => (
        <button key={c.id} type="button" className={`chip${writing ? "" : " go"}`} onClick={() => onUse(c)}>
          {c.name || cutName(c.text)}
        </button>
      ))}
      <button type="button" className="chip more" aria-label="Shortcuts" onClick={onMore}>
        <Icon name="plus" />
        {shown.length ? null : <span>Shortcuts</span>}
      </button>
    </div>
  );
}

/** Counts a use; the shortcut as kept, or null when it is gone. */
export function usedCut(id: string): Cut | null {
  const cuts = loadCuts();
  const c = cuts.find((x) => x.id === id);
  if (!c) return null;
  c.n = (c.n || 0) + 1;
  storeCuts(cuts);
  return c;
}

function CutRows({ title, note, items, onEdit, onUp }: { title: string; note: string; items: Cut[]; onEdit: (c: Cut) => void; onUp: (c: Cut) => void }) {
  if (!items.length) return null;
  return (
    <>
      <div className="glabel cutlabel">
        <span>{title}</span>
        <span>{note}</span>
      </div>
      <div className="opts">
        {items.map((c, i) => (
          <div key={c.id} className="opt cutrow">
            <button type="button" className="cutmain" onClick={() => onEdit(c)}>
              <span className="oname">{c.name || cutName(c.text)}</span>
              <span className="cuttext">{c.text}</span>
            </button>
            <span className="odetail">{c.n || 0}×</span>
            <button type="button" className="iconbtn" aria-label="Move up" disabled={!i} onClick={() => onUp(c)}>
              <Icon name="send" />
            </button>
          </div>
        ))}
      </div>
    </>
  );
}

/** Every shortcut, in its two groups. */
export function CutsSheet({ onBack, onEdit, onNew }: { onBack: () => void; onEdit: (c: Cut) => void; onNew: () => void }) {
  const [cuts, setCuts] = useState(loadCuts);
  const up = (c: Cut) => {
    const all = loadCuts();
    const i = all.findIndex((x) => x.id === c.id);
    if (i < 0) return;
    // Up within its own group: the one before it that sends the same way.
    let j = i - 1;
    while (j >= 0 && cutSends(all[j]) !== cutSends(all[i])) j--;
    if (j < 0) return;
    [all[i], all[j]] = [all[j], all[i]];
    if (storeCuts(all)) setCuts(all);
  };
  return (
    <div className="sheet">
      <SheetHeader title="Shortcuts" sub="Kept on this phone" onBack={onBack}>
        <button type="button" className="iconbtn" aria-label="New shortcut" onClick={onNew}>
          <Icon name="plus" />
        </button>
      </SheetHeader>
      <div className="sheet-body setform">
        {cuts.length ? (
          <>
            <CutRows title="When you write" note="Fills the box" items={cuts.filter((c) => !cutSends(c))} onEdit={onEdit} onUp={up} />
            <CutRows title="When the agent waits" note="Sends right away" items={cuts.filter(cutSends)} onEdit={onEdit} onUp={up} />
            <div className="cutnote">Tap one to change it. The arrow moves it up its row of buttons.</div>
          </>
        ) : (
          <div className="empty">
            <div className="hint">No shortcuts yet. Hold a message you sent to keep it, or tap + to write one.</div>
          </div>
        )}
      </div>
    </div>
  );
}

/** What a shortcut starts from: one being changed, or a text to keep. */
export type CutSeed = Partial<Cut> & { text: string };

/** The form for a new shortcut, or for the one being changed. `onDone` leaves it, saved or not. */
export function CutSheet({ seed, fromList, onDone }: { seed: CutSeed; fromList: boolean; onDone: () => void }) {
  const [named, setNamed] = useState(!!seed.id);
  const [c, setC] = useState(() => ({
    id: seed.id || null,
    n: seed.n || 0,
    name: seed.name || cutName(seed.text),
    text: seed.text,
    send: seed.id ? !!seed.send : seed.text.length > 0 && seed.text.length <= CUT_SHORT,
  }));
  const blank = c.text.includes(BLANK);
  const digits = /\d+/.test(c.text);
  const sends = c.send && !blank;
  const where = blank
    ? "Off: it has a blank to fill first. It shows above the keyboard and fills the box."
    : sends
      ? "On: one tap sends it. It shows when the agent is waiting for you."
      : "Off: it shows above the keyboard and fills the box, so you can change it first.";
  // Until the name is typed by hand it follows the text.
  const setText = (text: string) => setC({ ...c, text, name: named ? c.name : cutName(text) });
  const save = () => {
    const body = c.text.trim();
    if (!body) return toast("Write what it sends first");
    const all = loadCuts();
    const keep: Cut = { id: c.id || `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, name: c.name.trim() || cutName(body), text: body, send: !!c.send, n: c.n };
    const i = all.findIndex((x) => x.id === keep.id);
    if (i >= 0) all[i] = keep;
    else all.push(keep);
    if (!storeCuts(all)) return;
    if (!fromList) toast("Shortcut saved");
    onDone();
  };
  return (
    <div className="sheet">
      <SheetHeader title={c.id ? "Shortcut" : "New shortcut"} onBack={onDone} />
      <div className="sheet-body setform">
        <div className="glabel">Name on the button</div>
        <input
          className="cutin"
          maxLength={28}
          autoComplete="off"
          aria-label="Name on the button"
          value={c.name}
          onChange={(ev) => {
            setNamed(true);
            setC({ ...c, name: ev.target.value });
          }}
        />
        <div className="glabel">What it sends</div>
        <textarea className="cutin" rows={3} aria-label="What it sends" value={c.text} onChange={(ev) => setText(ev.target.value)} />
        {blank ? <div className="cutnote">{BLANK} is a blank: you type the number each time.</div> : null}
        {!blank && digits ? (
          <button type="button" className="btn outline small cutblank" onClick={() => setText(c.text.replace(/\d+/, BLANK))}>
            Make the number a blank
          </button>
        ) : null}
        <div className="glabel">How it sends</div>
        <div className="opts">
          <button type="button" className="opt" disabled={blank} onClick={() => setC({ ...c, send: !c.send })}>
            <span className="oname">
              Send right away<span className="cuttext wrap">{where}</span>
            </span>
            <span className={`switch${sends ? " on" : ""}`}>
              <span className="knob" />
            </span>
          </button>
        </div>
        <div className="cutbtns">
          {c.id ? (
            <button type="button" className="btn danger" onClick={() => storeCuts(loadCuts().filter((x) => x.id !== c.id)) && onDone()}>
              Delete
            </button>
          ) : null}
          <button type="button" className="btn" onClick={save}>
            Save shortcut
          </button>
        </div>
      </div>
    </div>
  );
}
