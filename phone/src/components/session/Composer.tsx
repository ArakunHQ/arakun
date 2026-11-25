// The bottom of the session: the stop row while the agent works, photos to
// send, the shortcuts, and the box. The keyboard's return key writes a new
// line: a prompt often has several, and a half-written one sent by accident
// sets the agent to work. Only the button sends.

import { useLayoutEffect, useRef, useState } from "react";
import { BLANK, type Cut, cutSends } from "../../lib/drafts";
import { Icon } from "../../lib/icons";
import type { Session } from "../../lib/model";
import { readShot, type Shot } from "../../lib/shots";
import { toast } from "../../lib/toast";
import { CutsRow, usedCut } from "./Shortcuts";
import { Shots } from "./Shots";
import { StopRow } from "./StopRow";

const MAX_BOX = 140;

export interface ComposerProps {
  session: Session;
  draft: string;
  setDraft: (text: string) => void;
  shots: Shot[];
  setShots: (update: (shots: Shot[]) => Shot[]) => void;
  /** Sends what is written, or `text` instead; `now` stops a running turn for it. */
  onSubmit: (opts: { now?: boolean; text?: string }) => void;
  /** Stop tapped twice, with nothing written. */
  onCancel: () => void;
  onOpenCuts: () => void;
}

export function Composer({ session: s, draft, setDraft, shots, setShots, onSubmit, onCancel, onOpenCuts }: ComposerProps) {
  const box = useRef<HTMLTextAreaElement>(null);
  const files = useRef<HTMLInputElement>(null);
  const [writing, setWriting] = useState(false);
  // The keyboard changes kind for a shortcut with a blank; the blur that takes is not the person leaving.
  const switching = useRef(false);
  const hasDraft = draft.trim().length > 0 || shots.length > 0;

  // The box grows with what is written, up to a few lines.
  useLayoutEffect(() => {
    const ta = box.current;
    if (!ta) return;
    ta.style.height = "auto";
    if (draft) ta.style.height = `${Math.min(MAX_BOX, ta.scrollHeight)}px`;
  }, [draft]);

  const pickCut = (picked: Cut) => {
    const c = usedCut(picked.id);
    const ta = box.current;
    if (!c || !ta) return;
    if (cutSends(c)) return onSubmit({ text: c.text });
    const at = c.text.indexOf(BLANK);
    const before = ta.value && !/\s$/.test(ta.value) ? `${ta.value} ` : ta.value;
    const numeric = at >= 0;
    // The keyboard only changes kind when the field is focused again.
    if (numeric !== (ta.inputMode === "numeric")) {
      switching.current = true;
      ta.blur();
      ta.inputMode = numeric ? "numeric" : "text";
      switching.current = false;
    }
    const value = before + c.text.replace(BLANK, "");
    ta.value = value;
    ta.focus();
    const pos = numeric ? before.length + at : value.length;
    ta.setSelectionRange(pos, pos);
    setDraft(value);
  };

  const waiting = s.status !== "working" && !hasDraft;
  return (
    <>
      <StopRow session={s} hasDraft={hasDraft} onStop={() => (hasDraft ? onSubmit({ now: true }) : onCancel())} />
      <Shots shots={shots} onRemove={(i) => setShots((all) => all.filter((_, k) => k !== i))} />
      {writing || waiting ? <CutsRow writing={writing} onUse={pickCut} onMore={onOpenCuts} /> : null}
      <form
        className="composer"
        onSubmit={(ev) => {
          ev.preventDefault();
          onSubmit({});
          // Sending puts the keyboard away so the reply has the screen.
          box.current?.blur();
        }}
      >
        <textarea
          ref={box}
          rows={1}
          placeholder={`Message ${s.agent || "the agent"}…`}
          value={draft}
          onChange={(ev) => setDraft(ev.target.value)}
          onFocus={() => setWriting(true)}
          onBlur={(ev) => {
            if (switching.current) return;
            ev.currentTarget.inputMode = "text";
            // After the tap that took the focus has landed on its button.
            setTimeout(() => setWriting(document.activeElement === box.current), 200);
          }}
        />
        <div className="cbar">
          <button className="iconbtn" type="button" aria-label="Attach photos" onClick={() => files.current?.click()}>
            <Icon name="image" />
          </button>
          <span className="grow" />
          <button className="btn" type="submit" aria-label="Send" disabled={!hasDraft}>
            <Icon name="send" />
          </button>
        </div>
      </form>
      <input
        ref={files}
        type="file"
        accept="image/*"
        multiple
        hidden
        onChange={(ev) => {
          const picked = [...(ev.target.files ?? [])];
          ev.target.value = "";
          for (const f of picked) {
            readShot(f)
              .then((shot) => shot && setShots((all) => [...all, shot]))
              .catch(() => toast("Could not read that photo"));
          }
        }}
      />
    </>
  );
}
