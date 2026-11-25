// The + in the tab bar. A tap asks which project (straight on with only one)
// and opens the quick sheet there; holding it opens a menu of the places, to
// start in one of them or reach the full form. The tiles and the quick sheet
// are one step in the history, so the phone's back closes them.

import { useNavigate, useRouter, useRouterState } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ago } from "../lib/format";
import { Icon } from "../lib/icons";
import { useLink } from "../lib/link";
import type { Place } from "../lib/model";
import { AGENTS, agentName, branchOf, currentSetting, formAt, lastPlace, placeFor, projectList, type StartForm, useStartForm } from "../lib/places";
import { refit } from "../lib/viewport";
import "./PlusMenu.css";

// How long a press must last to open the menu instead of the tiles.
const HOLD_MS = 420;

/** A project's tile mark: the raccoon for Arakun's own, else its initial. */
function ProjectMark({ name }: { name: string }) {
  if (/^(app|arakun)$/i.test(name)) return <img src="icon-small.svg" alt="" />;
  return <span>{name[0]?.toUpperCase()}</span>;
}

type Layer = null | { kind: "menu" } | { kind: "pick" } | { kind: "quick"; form: StartForm; n: number };

function Tiles({ onPick, onClose }: { onPick: (project: string) => void; onClose: () => void }) {
  const places = useLink((s) => s.places);
  const inbox = useLink((s) => s.inbox);
  const projects = projectList(places, inbox);
  return (
    <div id="projpick">
      <div className="pphead">
        <b>New session</b>
        <button type="button" className="iconbtn" aria-label="Close" onClick={onClose}>
          <Icon name="close" />
        </button>
      </div>
      <div className="ppsub">Which project?</div>
      <div className="ppgrid">
        {projects.map((g) => (
          <button key={g.name} type="button" className="pptile" onClick={() => onPick(g.name)}>
            <span className="ppmark" aria-hidden="true">
              <ProjectMark name={g.name} />
            </span>
            <b>{g.name}</b>
            <small>
              {g.working ? (
                <>
                  <span className="dot" />
                  {g.working} working
                </>
              ) : (
                `${g.updated ? ago(g.updated) : "no sessions yet"}${g.places.length > 1 ? ` · ${g.places.length} branches` : ""}`
              )}
            </small>
          </button>
        ))}
      </div>
    </div>
  );
}

function Menu({ onQuick, onFull, onClose }: { onQuick: (p: Place) => void; onFull: () => void; onClose: () => void }) {
  const places = useLink((s) => s.places);
  const inbox = useLink((s) => s.inbox);
  const shown = projectList(places, inbox)
    .slice(0, 6)
    .map((g) => placeFor(places, g.name))
    .filter((p): p is Place => !!p);
  return (
    <div id="plusmenu">
      <div className="pmscrim" onClick={onClose} />
      <div className="pmcard">
        {shown.map((p, i) => (
          <button key={p.path} type="button" className={`pmitem${i === 0 ? " hl" : ""}`} onClick={() => onQuick(p)}>
            <span className="pmic ppmark" aria-hidden="true">
              <ProjectMark name={p.project} />
            </span>
            <span className="pmtx">
              New in <b>{p.project}</b>
              <small>
                {branchOf(p)} · {agentName(p.agent || AGENTS[0][0])}
              </small>
            </span>
          </button>
        ))}
        <div className="pmsep" />
        <button type="button" className="pmitem" onClick={onFull}>
          <span className="pmic">
            <Icon name="tune" />
          </span>
          <span className="pmtx">
            More options<small>Agent, model and reasoning</small>
          </span>
        </button>
      </div>
    </div>
  );
}

// What to do, over the screen you are on; where and with whom are two chips
// already set (the place used last, its agent, the model it would start with).
function Quick({ init, onClose, onProjects, onMore }: { init: StartForm; onClose: () => void; onProjects: () => void; onMore: (f: StartForm) => void }) {
  const places = useLink((s) => s.places);
  const { form, options, sent, canSend, setPlace, setAgent, setText, pick: choose, start } = useStartForm(init, { needLink: true });
  const [open, setOpen] = useState<"place" | "agent" | null>(null);
  const text = useRef<HTMLTextAreaElement>(null);
  const place = places.find((p) => p.path === form.path);
  const branches = place ? places.filter((p) => p.project === place.project) : [];
  const model = currentSetting(options, form.picked, "modelId");
  const effort = currentSetting(options, form.picked, "reasoningEffort");
  const toggle = (k: "place" | "agent") => setOpen(open === k ? null : k);

  let list = null;
  if (open === "place")
    list = (
      <div className="qlist">
        {branches.map((p) => (
          <button
            key={p.path}
            type="button"
            className={`qopt${p.path === form.path ? " on" : ""}`}
            onClick={() => {
              setPlace(p.path);
              setOpen(null);
            }}
          >
            <span className="qbr">
              <Icon name="branch" />
              {branchOf(p)}
            </span>
            <small>{agentName(p.agent || AGENTS[0][0])}</small>
          </button>
        ))}
      </div>
    );
  else if (open === "agent")
    list = (
      <div className="qlist">
        <div className="qhead">Agent</div>
        {AGENTS.map(([k, label]) => (
          <button key={k} type="button" className={`qopt${k === form.agent ? " on" : ""}`} onClick={() => setAgent(k)}>
            <span>{label}</span>
          </button>
        ))}
        {options ? (
          options.map((set) => (
            <div key={set.key} style={{ display: "contents" }}>
              <div className="qhead">{set.label}</div>
              {set.options.map((o) => (
                <button
                  key={o.id}
                  type="button"
                  className={`qopt${o.id === (form.picked[set.key] ?? set.current) ? " on" : ""}`}
                  disabled={o.disabled}
                  onClick={() => choose(set.key, o.id)}
                >
                  <span>{o.name}</span>
                  {o.detail ? <small>{o.detail}</small> : null}
                </button>
              ))}
            </div>
          ))
        ) : (
          <div className="qhead">Loading models…</div>
        )}
      </div>
    );

  return (
    <div id="quick">
      <div className="qscrim" onClick={onClose} />
      <form
        className="qsheet"
        onSubmit={(ev) => {
          ev.preventDefault();
          if (!start()) return;
          text.current?.blur();
          refit(0, 120, 300, 600);
        }}
      >
        <div className="grab" />
        {/* Another project: back to the tiles. */}
        <button type="button" className="qctx" onClick={() => places.length > branches.length && onProjects()}>
          New session in{" "}
          <span className="ppmark" aria-hidden="true">
            {place ? <ProjectMark name={place.project} /> : null}
          </span>
          <b>{place?.project ?? ""}</b>
          {places.length > branches.length ? <Icon name="chevron" /> : null}
        </button>
        <div className="qchips">
          <button type="button" className={`qchip${open === "place" ? " open" : ""}`} disabled={branches.length < 2} onClick={() => toggle("place")}>
            <Icon name="branch" />
            <span>{place ? branchOf(place) : "Branch"}</span>
            {branches.length > 1 ? <Icon name="chevron" /> : null}
          </button>
          <button type="button" className={`qchip${open === "agent" ? " open" : ""}`} onClick={() => toggle("agent")}>
            <span>
              {agentName(form.agent)}
              {model ? ` · ${model}` : ""}
              {effort ? ` · ${effort}` : ""}
            </span>
            <Icon name="chevron" />
          </button>
        </div>
        {list}
        <textarea
          id="qtext"
          ref={text}
          rows={3}
          placeholder={`What should ${agentName(form.agent)} do?`}
          value={form.text}
          autoFocus
          onChange={(e) => setText(e.target.value)}
        />
        <div className="qbar">
          <button type="button" className="qmore" onClick={() => onMore(form)}>
            More options
          </button>
          <span className="grow" />
          <button className="btn" id="qgo" type="submit" aria-label="Start" disabled={!canSend}>
            {sent ? <span className="spin" /> : <Icon name="send" />}
          </button>
        </div>
      </form>
    </div>
  );
}

/** The + and what it opens over the screen. */
export function PlusButton({ disabled }: { disabled: boolean }) {
  const router = useRouter();
  const navigate = useNavigate();
  const places = useLink((s) => s.places);
  const inbox = useLink((s) => s.inbox);
  const connected = useLink((s) => s.connected);
  const stepped = useRouterState({ select: (s) => !!s.location.state.newStep });
  const [layer, setLayer] = useState<Layer>(null);
  const count = useRef(0);
  const hold = useRef<{ timer?: ReturnType<typeof setTimeout>; held: boolean }>({ held: false });

  // The phone's back took the step away: what it put there goes too.
  const was = useRef(stepped);
  useEffect(() => {
    if (was.current && !stepped) setLayer((l) => (l?.kind === "menu" ? l : null));
    was.current = stepped;
  }, [stepped]);
  useEffect(() => () => clearTimeout(hold.current.timer), []);

  const step = () => {
    if (!router.state.location.state.newStep) router.history.push(router.state.location.href, { newStep: true });
  };
  // The on-screen close goes through the history when it put a step there,
  // so the phone's back and this one stay one and the same.
  const close = () => {
    if (router.state.location.state.newStep) router.history.back();
    else setLayer(null);
  };
  const quick = (place?: Place) => {
    const p = place || lastPlace(places);
    if (!p || !connected) return;
    setLayer({ kind: "quick", form: formAt(p), n: ++count.current });
    step();
  };
  const pickProject = () => {
    if (!connected || !places.length) return;
    const projects = projectList(places, inbox);
    if (projects.length === 1) return quick(placeFor(places, projects[0].name));
    setLayer({ kind: "pick" });
    step();
  };
  const full = (form?: StartForm) => {
    setLayer(null);
    // The form takes the step the sheet put there: back from it goes where the + was.
    void navigate({ to: "/new", replace: !!router.state.location.state.newStep, state: form ? { startForm: form } : {} });
  };

  let over = null;
  if (layer?.kind === "pick")
    over = (
      <Tiles
        onClose={close}
        onPick={(project) => {
          const p = placeFor(places, project);
          if (p) setLayer({ kind: "quick", form: formAt(p), n: ++count.current });
        }}
      />
    );
  else if (layer?.kind === "menu")
    over = (
      <Menu
        onClose={() => setLayer(null)}
        onQuick={(p) => quick(p)}
        onFull={() => full()}
      />
    );
  else if (layer?.kind === "quick")
    over = <Quick key={layer.n} init={layer.form} onClose={close} onProjects={() => setLayer({ kind: "pick" })} onMore={full} />;

  return (
    <>
      <button
        className="tplus"
        aria-label="New session"
        disabled={disabled}
        onPointerDown={() => {
          hold.current.held = false;
          clearTimeout(hold.current.timer);
          hold.current.timer = setTimeout(() => {
            hold.current.held = true;
            navigator.vibrate?.(12);
            if (connected && places.length) setLayer({ kind: "menu" });
          }, HOLD_MS);
        }}
        onPointerUp={() => clearTimeout(hold.current.timer)}
        onPointerLeave={() => clearTimeout(hold.current.timer)}
        onPointerCancel={() => clearTimeout(hold.current.timer)}
        onContextMenu={(ev) => ev.preventDefault()}
        onClick={(ev) => {
          ev.preventDefault();
          if (hold.current.held) return;
          pickProject();
        }}
      >
        <Icon name="plus" size={20} />
      </button>
      {over ? createPortal(over, document.body) : null}
    </>
  );
}
