import { useContext, useEffect } from "react";
import { StatusBar } from "../components/StatusBar";
import { TabBar } from "../components/TabBar";
import { ago } from "../lib/format";
import { Icon } from "../lib/icons";
import { LinkContext, useLink } from "../lib/link";
import { connecting } from "../lib/sessions";

interface Row {
  label: string;
  percent: number;
  detail?: string;
}
interface Agent {
  agent: string;
  name: string;
  plan?: string;
  signed_in?: boolean;
  rows?: Row[];
  idle?: string[];
  balance?: string;
  note?: string;
  tone?: string;
  at?: number;
}

// The bar's color by how full the window is; each agent has its own calm color.
export function barTone(agent: string, pct: number): string {
  if (pct >= 100) return "full";
  if (pct >= 70) return "high";
  return agent;
}

function UsageCard({ a }: { a: Agent }) {
  const logo = a.agent === "claude" ? <img className="logo" src="claude.svg" alt="" /> : <span className="logo factory" />;
  if (!a.signed_in) {
    return (
      <section className="ucard">
        <div className="uhead">
          {logo}
          <div className="grow">
            <div className="uname">{a.name}</div>
            <div className="uplan">Not signed in</div>
          </div>
        </div>
        <div className="unote">{a.note}</div>
      </section>
    );
  }
  return (
    <section className="ucard">
      <div className="uhead">
        {logo}
        <div className="grow">
          <div className="uname">{a.name}</div>
          {a.plan ? <div className="uplan">{a.plan}</div> : null}
        </div>
      </div>
      {(a.rows || []).map((r) => (
        <div className="limit" key={r.label}>
          <div className="lhead">
            <span className="llabel">{r.label}</span>
            <span className="lval">
              {r.percent}%{r.detail ? ` · ${r.detail}` : ""}
            </span>
          </div>
          <div className="track">
            <div className={`fill ${barTone(a.agent, r.percent)}`} style={{ width: `${Math.min(100, r.percent)}%` }} />
          </div>
        </div>
      ))}
      {a.balance ? (
        <div className="ubal">
          <span className="grow">Extra usage balance</span>
          <span>{a.balance}</span>
        </div>
      ) : null}
      {a.idle?.length ? <div className="uidle">Not active: {a.idle.join(", ")}</div> : null}
      {a.note ? <div className={`unote ${a.tone || "normal"}`}>{a.note}</div> : null}
    </section>
  );
}

export function Usage() {
  const link = useContext(LinkContext);
  const usage = useLink((s) => s.usage);
  const connected = useLink((s) => s.connected);
  const host = useLink((s) => s.host);
  const pcOffline = useLink((s) => s.pcOffline);
  const ask = () => link?.send({ type: "usage" });
  // Asking twice (StrictMode in dev) only reads the same numbers again. A link
  // that is not up drops it, so ask again once connected.
  useEffect(() => {
    if (connected) link?.send({ type: "usage" });
  }, [link, connected]);
  // Looking at Usage keeps the numbers fresh, as the computer re-reads them
  // when they are stale.
  useEffect(() => {
    if (!connected) return;
    const every = setInterval(() => link?.send({ type: "usage" }), 60_000);
    return () => clearInterval(every);
  }, [link, connected]);

  const agents = (usage?.agents || []) as Agent[];
  const at = Math.max(0, ...agents.map((a) => a.at || 0));
  const when = ago(at);
  return (
    <div className="screen">
      <StatusBar />
      <div className="hdr">
        <div className="grow">
          <h1>Usage</h1>
        </div>
        <button className="iconbtn" aria-label="Refresh" onClick={ask}>
          <Icon name="refresh" />
        </button>
      </div>
      <div className="usage">
        {usage ? (
          <>
            {agents.map((a) => (
              <UsageCard a={a} key={a.agent} />
            ))}
            <div className="ufoot">
              Read from the agents' own bookkeeping on the computer, same as the sidebar in Arakun.{at ? ` Updated ${when}${when === "now" ? "" : " ago"}.` : ""}
            </div>
          </>
        ) : (
          <div className="empty">
            <div className="big">🦝</div>
            <div>{connected ? "Reading usage…" : connecting({ pcOffline, host })}</div>
          </div>
        )}
      </div>
      <TabBar />
    </div>
  );
}
