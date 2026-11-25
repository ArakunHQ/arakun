import { useState } from "react";
import { SessionRow } from "../components/SessionRow";
import { StatusBar } from "../components/StatusBar";
import { TabBar } from "../components/TabBar";
import { Icon } from "../lib/icons";
import { useLink } from "../lib/link";
import { connecting, connectingHint, projects } from "../lib/sessions";

export function Sessions() {
  const inbox = useLink((s) => s.inbox);
  const connected = useLink((s) => s.connected);
  const host = useLink((s) => s.host);
  const pcOffline = useLink((s) => s.pcOffline);
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState("");

  const names = projects(inbox);
  // A project that has no session left is no filter.
  const project = names.includes(picked) ? picked : "";
  const q = query.trim().toLowerCase();
  const matches = (s: (typeof inbox)[number]) => !q || [s.title, s.env, s.agent, s.project].some((x) => String(x || "").toLowerCase().includes(q));
  const groups = names
    .filter((p) => !project || p === project)
    .map((p) => [p, inbox.filter((s) => s.project === p && matches(s))] as const)
    .filter(([, items]) => items.length);
  const sub = `${inbox.length} session${inbox.length === 1 ? "" : "s"} · ${names.length} project${names.length === 1 ? "" : "s"}`;

  let list;
  if (!inbox.length) {
    list = (
      <div className="empty">
        <div className="big">🦝</div>
        <div>{connected ? "No sessions yet" : connecting({ pcOffline, host })}</div>
        <div className="hint">{connected ? "Start one on the computer and it shows up here." : connectingHint()}</div>
      </div>
    );
  } else if (!groups.length) {
    list = (
      <div className="empty">
        <div>Nothing matches “{query.trim()}”</div>
      </div>
    );
  } else {
    list = groups.map(([p, items]) => (
      <div className="group" key={p}>
        <div className="proj">
          <span className="picon">
            <Icon name="folder" />
          </span>
          <span className="pname">{p}</span>
          <span className="pcount">{items.length}</span>
        </div>
        {items.map((s) => (
          <SessionRow key={s.id} s={s} variant="list" />
        ))}
      </div>
    ));
  }

  const chip = (id: string, label: string) => (
    <button key={id} className={`chip${project === id ? " on" : ""}`} onClick={() => setPicked(id)}>
      {label}
    </button>
  );
  return (
    <div className="screen">
      <StatusBar />
      <div className="hdr">
        <div className="grow">
          <h1>Sessions</h1>
          <div className="sub">{sub}</div>
        </div>
      </div>
      <label className="search">
        <Icon name="search" />
        <input type="search" placeholder="Search sessions…" autoComplete="off" value={query} onChange={(e) => setQuery(e.target.value)} />
      </label>
      {names.length > 1 ? (
        <div className="chips">
          {chip("", "All")}
          {names.map((p) => chip(p, p))}
        </div>
      ) : null}
      <div className="inbox">{list}</div>
      <TabBar />
    </div>
  );
}
