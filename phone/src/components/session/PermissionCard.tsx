// What the agent wants to do, with what it would touch, and the answers: allow
// once and deny side by side, and "always" apart, since it lasts the session.
// The card folds like the question does, to read the log first.

import { type PointerEvent, type MouseEvent, useCallback, useEffect, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import type { Entry } from "../../lib/model";
import { toast } from "../../lib/toast";

export interface PermissionOption {
  id: string;
  name: string;
  /** allow_once, allow_always, reject_once… */
  kind: string;
}

export interface Permission {
  tool_call_id: string;
  title: string;
  options: PermissionOption[];
}

/**
 * Taps that undo nothing take two: the first arms for `windowMs`. `tap()`
 * says whether this tap is the second one in time.
 */
export function useArmed(windowMs = 3000): [armed: boolean, tap: () => boolean, disarm: () => void] {
  const [at, setAt] = useState(0);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const disarm = useCallback(() => {
    clearTimeout(timer.current);
    setAt(0);
  }, []);
  const tap = useCallback(() => {
    clearTimeout(timer.current);
    if (at && Date.now() - at < windowMs) {
      setAt(0);
      return true;
    }
    setAt(Date.now());
    timer.current = setTimeout(() => setAt(0), windowMs);
    return false;
  }, [at, windowMs]);
  return [at !== 0, tap, disarm];
}

/**
 * Answers only once the finger (or the mouse button) has stayed down for
 * `ms`. A plain tap says to hold instead.
 */
export function useHold(ms = 700, onDone: () => void) {
  const [holding, setHolding] = useState(false);
  const held = useRef(false);
  const done = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const latest = useRef(onDone);
  latest.current = onDone;
  useEffect(() => () => clearTimeout(timer.current), []);
  const stop = () => {
    clearTimeout(timer.current);
    held.current = false;
    setHolding(false);
  };
  return {
    holding,
    handlers: {
      onPointerDown(ev: PointerEvent) {
        ev.preventDefault();
        done.current = false;
        held.current = true;
        setHolding(true);
        clearTimeout(timer.current);
        timer.current = setTimeout(() => {
          stop();
          done.current = true;
          navigator.vibrate?.(15);
          latest.current();
        }, ms);
      },
      onPointerUp: stop,
      onPointerLeave: stop,
      onPointerCancel: stop,
      onClick(ev: MouseEvent) {
        ev.preventDefault();
        // The click that ends a hold that answered.
        if (done.current) {
          done.current = false;
          return;
        }
        if (!held.current) toast("Hold to allow");
      },
    },
  };
}

function HoldButton({ option, onAnswer }: { option: PermissionOption; onAnswer: (id: string) => void }) {
  const { holding, handlers } = useHold(700, () => onAnswer(option.id));
  return (
    <button type="button" className={`btn hold${holding ? " holding" : ""}`} {...handlers}>
      <i />
      <span>Hold to {option.name.toLowerCase()}</span>
    </button>
  );
}

function AlwaysButton({ option, onAnswer }: { option: PermissionOption; onAnswer: (id: string) => void }) {
  const [armed, tap] = useArmed(3000);
  return (
    <button type="button" className="always" onClick={() => tap() && onAnswer(option.id)}>
      <span className="warn">!</span>
      <span className="grow">
        <span className={`t${armed ? " armed" : ""}`}>{armed ? "Tap again to always allow" : option.name}</span>
        <span className="s">For the rest of this session · tap twice</span>
      </span>
    </button>
  );
}

export interface PermissionCardProps {
  permission: Permission;
  onAnswer: (optionId: string) => void;
  /** Where the session runs, said under the command. */
  place?: string;
  /** The tool call that asks, for its kind and the files it would touch. */
  tool?: Entry;
}

export function PermissionCard({ permission: p, onAnswer, place, tool }: PermissionCardProps) {
  const [folded, setFolded] = useState(false);
  const kind = tool?.kind ? String(tool.kind) : "";
  const diffs = (tool?.diffs as { path: string }[] | undefined) ?? [];
  const files = diffs.map((d) => String(d.path).split(/[\\/]/).pop());
  const where = [place, ...files.slice(0, 3)].filter(Boolean).join(" · ");
  const always = p.options.filter((o) => o.kind === "allow_always");
  // Deny first, then Allow: the thumb rests on the right.
  const main = p.options.filter((o) => o.kind !== "allow_always").reverse();
  return (
    <div className={`perm${folded ? " folded" : ""}`}>
      <button type="button" className="phead" aria-expanded={!folded} onClick={() => setFolded(!folded)}>
        <span className="label">
          <Icon name="alert" />
          Asks permission
        </span>
        {kind ? <span className="kind">{kind}</span> : null}
        <span className="peek">{p.title}</span>
        <Icon name="chevron" />
      </button>
      <div className="pbody">
        <div className="what">
          <code>{p.title}</code>
        </div>
        {where ? <div className="where">{where}</div> : null}
        <div className="opts">
          {main.map((o) =>
            o.kind.startsWith("allow") ? (
              <HoldButton key={o.id} option={o} onAnswer={onAnswer} />
            ) : (
              <button key={o.id} type="button" className="btn outline" onClick={() => onAnswer(o.id)}>
                {o.name}
              </button>
            ),
          )}
        </div>
        {always.map((o) => (
          <AlwaysButton key={o.id} option={o} onAnswer={onAnswer} />
        ))}
      </div>
    </div>
  );
}
