import { useNavigate } from "@tanstack/react-router";
import { NoticeBanners } from "../components/NoticeBanners";
import { SessionRow } from "../components/SessionRow";
import { SetupCards } from "../components/SetupCards";
import { StatusBar } from "../components/StatusBar";
import { TabBar } from "../components/TabBar";
import { useMe } from "../lib/account";
import { Icon } from "../lib/icons";
import { useLink } from "../lib/link";
import { connecting, connectingHint, needsYou } from "../lib/sessions";
import { mode } from "../lib/start";
import type { InboxSession } from "../lib/model";

// How far back Finished reaches in the inbox, in seconds.
const RECENT = 24 * 3600;

function Group({ kind, name, items }: { kind: string; name: string; items: InboxSession[] }) {
  return (
    <div className={`group ${kind}`}>
      <h2>
        {kind === "needs" ? <Icon name="alert" size={16} /> : null}
        {kind === "working" ? <span className="dot" style={{ background: "var(--success)" }} /> : null}
        {name}
      </h2>
      {items.map((s) => (
        <SessionRow key={s.id} s={s} variant="inbox" />
      ))}
    </div>
  );
}

function Tile({ label, n, color }: { label: string; n: number; color?: string }) {
  return (
    <div className="tile">
      <div className="l">{label}</div>
      <div className="v">
        {n && color ? <span className="dot" style={{ background: color }} /> : null}
        {n}
      </div>
    </div>
  );
}

export function Inbox() {
  const navigate = useNavigate();
  const inbox = useLink((s) => s.inbox);
  const connected = useLink((s) => s.connected);
  const host = useLink((s) => s.host);
  const pcOffline = useLink((s) => s.pcOffline);
  const me = useMe(mode() === "account");
  const notices = me.data && me.data.screen !== "signin" ? me.data.me?.notices || [] : [];

  // What waits on you first (a failed run too), then what runs, then what ended
  // lately: older sessions are in Sessions, not here.
  const since = Date.now() / 1000 - RECENT;
  const finished = inbox.filter((s) => s.status === "idle");
  const recent = finished.filter((s) => (s.updated ?? 0) >= since).slice(0, 12);
  const needs = inbox.filter(needsYou);
  const working = inbox.filter((s) => s.status === "working");
  const older = finished.length - recent.length;
  const toSessions = () => void navigate({ to: "/sessions" });

  return (
    <div className="screen" data-screen="inbox">
      <StatusBar />
      <div className="ihead">
        <img className="mark" src="apple-touch-icon.png" alt="" />
        <span className="host">{host || "Arakun"}</span>
        <span className={`pill ${connected ? "on" : "off"}`}>
          <span className="dot" />
          {connected ? "online" : "away"}
        </span>
      </div>
      <div className="inbox">
        <NoticeBanners notices={notices} />
        <SetupCards />
        {inbox.length ? (
          <div className="tiles">
            <Tile label="Needs you" n={needs.length} color="var(--accent)" />
            <Tile label="Working" n={working.length} color="var(--success)" />
            <Tile label="Done today" n={recent.length} />
          </div>
        ) : null}
        {needs.length ? <Group kind="needs" name="Needs you" items={needs} /> : null}
        {working.length ? <Group kind="working" name="Working now" items={working} /> : null}
        {recent.length ? <Group kind="done" name="Finished" items={recent} /> : null}
        {older > 0 ? (
          <button className="more" onClick={toSessions}>
            {older} older in Sessions
          </button>
        ) : null}
        {inbox.length ? null : (
          <div className="empty">
            <div className="big">🦝</div>
            <div>{connected ? "No sessions yet" : connecting({ pcOffline, host })}</div>
            <div className="hint">{connected ? "Tap + to start one, or start it on the computer." : connectingHint()}</div>
          </div>
        )}
      </div>
      <button className="jump" onClick={toSessions}>
        <Icon name="search" />
        <span>Jump to a session or search…</span>
      </button>
      <TabBar />
    </div>
  );
}
