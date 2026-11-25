// The full New session form, for the agent's model and reasoning before the
// first message. The + reaches it through "More options".

import { useNavigate, useRouter, useRouterState } from "@tanstack/react-router";
import { useMemo } from "react";
import { StatusBar } from "../components/StatusBar";
import { Icon } from "../lib/icons";
import { useLink } from "../lib/link";
import { AGENTS, formAt, lastPlace, placeLabel, type SettingSet, type StartForm, useStartForm } from "../lib/places";

function Rows({ set, picked, onPick }: { set: SettingSet; picked?: string; onPick: (key: string, id: string) => void }) {
  const current = picked ?? set.current;
  return (
    <>
      <div className="glabel">{set.label}</div>
      <div className="opts">
        {set.options.map((o) => (
          <button key={o.id} type="button" className={`opt${o.id === current ? " on" : ""}`} disabled={o.disabled} onClick={() => onPick(set.key, o.id)}>
            <span className="on-mark">{o.id === current ? <Icon name="check" /> : null}</span>
            <span className="oname">{o.name}</span>
            {o.detail ? <span className="odetail">{o.detail}</span> : null}
          </button>
        ))}
      </div>
    </>
  );
}

function Form({ init }: { init: StartForm }) {
  const places = useLink((s) => s.places);
  const { form, options, sent, canSend, setPlace, setAgent, setText, pick, start } = useStartForm(init);
  return (
    <div className="newform">
      <label className="nlabel" htmlFor="nplace">
        Project
      </label>
      <select id="nplace" className="nfield" value={form.path} onChange={(e) => setPlace(e.target.value)}>
        {places.map((p) => (
          <option key={p.path} value={p.path}>
            {placeLabel(p)}
          </option>
        ))}
      </select>
      <div className="nlabel">Agent</div>
      <div className="chips">
        {AGENTS.map(([key, label]) => (
          <button key={key} type="button" className={`chip${form.agent === key ? " on" : ""}`} onClick={() => setAgent(key)}>
            {label}
          </button>
        ))}
      </div>
      {options ? options.map((set) => <Rows key={set.key} set={set} picked={form.picked[set.key]} onPick={pick} />) : <div className="nhint">Loading models…</div>}
      <label className="nlabel" htmlFor="ntext">
        First message
      </label>
      <textarea id="ntext" className="nfield" rows={6} placeholder="What should the agent do?" value={form.text} autoFocus onChange={(e) => setText(e.target.value)} />
      <button type="button" className="btn" disabled={!canSend} onClick={start}>
        {sent ? "Starting…" : "Start session"}
      </button>
    </div>
  );
}

export function NewSession() {
  const router = useRouter();
  const navigate = useNavigate();
  const host = useLink((s) => s.host);
  const places = useLink((s) => s.places);
  const carried = useRouterState({ select: (s) => s.location.state.startForm });
  // The quick sheet's form when it led here, else the place used last. A
  // reload before the computer listed its places waits for them. Taken once,
  // when the places first come; later lists leave the form alone.
  const first = places.length > 0;
  const init = useMemo(() => {
    if (carried && places.some((p) => p.path === carried.path)) return carried;
    const p = lastPlace(places);
    return p ? formAt(p) : null;
  }, [carried, first]);
  const back = () => {
    if (router.history.canGoBack()) router.history.back();
    else void navigate({ to: "/inbox" });
  };
  return (
    <div className="screen">
      <StatusBar />
      <div className="hdr">
        <button type="button" className="iconbtn" aria-label="Back" onClick={back}>
          <Icon name="back" />
        </button>
        <div className="grow">
          <div className="title">New session</div>
          <div className="sub">{host}</div>
        </div>
      </div>
      {init ? <Form init={init} /> : <div className="nhint">Loading…</div>}
    </div>
  );
}
