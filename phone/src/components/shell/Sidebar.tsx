import { useNavigate, useRouterState } from "@tanstack/react-router";
import { useMe } from "../../lib/account";
import { ago, statusLabel } from "../../lib/format";
import { Icon } from "../../lib/icons";
import { useLink } from "../../lib/link";
import type { InboxSession } from "../../lib/model";
import { inboxGroups } from "../../lib/sessions";
import { mode } from "../../lib/start";
import { PlusButton } from "../PlusMenu";
import { useOpenSession } from "../SessionRow";
import { ComputerPicker, useAskSignOut } from "./ComputerPicker";
import { WIDE_TABS } from "./Rail";

// A state is a dot and a word; a failed run is red, not amber.
const DOT: Record<string, string> = { needs_input: "needs", error: "bad", working: "working" };

function Row({ s, on }: { s: InboxSession; on: boolean }) {
  const openSession = useOpenSession();
  const status = s.status ?? "idle";
  const where = [s.project, s.env && s.env !== s.project ? s.env : "", ago(s.updated ?? 0)].filter(Boolean).join(" · ");
  return (
    <button type="button" className={`sbrow${on ? " on" : ""}`} aria-current={on ? "page" : undefined} onClick={() => !on && openSession(s.id)}>
      <span className={`dot ${DOT[status] ?? ""}`} />
      <span className="sbbody">
        <span className="sbt">{s.title || "Untitled session"}</span>
        <span className="sbs">{where}</span>
      </span>
      <span className={`sbstate ${status}`}>{status === "error" ? "Failed" : statusLabel(status)}</span>
    </button>
  );
}

function Group({ name, items, open }: { name: string; items: InboxSession[]; open: string }) {
  if (!items.length) return null;
  return (
    <div className="sbgroup">
      <h2>{name}</h2>
      {items.map((s) => (
        <Row key={s.id} s={s} on={s.id === open} />
      ))}
    </div>
  );
}

function Account() {
  const me = useMe(true);
  const askSignOut = useAskSignOut();
  const login = me.data?.me?.login;
  if (!login) return null;
  return (
    <footer className="sbfoot">
      <span className="sbav" aria-hidden="true">
        {login[0].toUpperCase()}
      </span>
      <span className="sblogin">{login}</span>
      <button type="button" className="btn ghost small" onClick={askSignOut}>
        Sign out
      </button>
    </footer>
  );
}

/** A desktop's column on the left: the computer, the tabs and every live session. */
export function Sidebar() {
  const navigate = useNavigate();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const inbox = useLink((s) => s.inbox);
  const canStart = useLink((s) => s.connected && s.places.length > 0);
  const { needs, working, recent, older } = inboxGroups(inbox);
  const open = /^\/s\/([^/]+)$/.exec(path)?.[1];
  const openId = open ? decodeURIComponent(open) : "";
  // A session open counts under Inbox: the sidebar's list is the inbox.
  const tabOn = (to: string) => path === to || (to === "/inbox" && !!openId);

  return (
    <aside className="sidebar">
      <div className="sbhead">
        <img src="icon-small.svg" alt="" />
        <b>Arakun</b>
      </div>
      <ComputerPicker look="card" />
      <nav className="sbtabs" aria-label="Tabs">
        {WIDE_TABS.map((t) => {
          const on = tabOn(t.to);
          const count = t.to === "/inbox" ? needs.length : t.to === "/sessions" ? inbox.length : 0;
          return (
            <button key={t.to} type="button" className={`sbtab${on ? " on" : ""}`} aria-label={t.label} aria-current={on ? "page" : undefined} onClick={() => path !== t.to && void navigate({ to: t.to })}>
              <Icon name={t.icon} />
              <span className="grow">{t.label}</span>
              {count ? (
                <span className={`sbcount${t.to === "/inbox" ? " needs" : ""}`}>
                  {t.to === "/inbox" ? <span className="dot needs" /> : null}
                  {count}
                </span>
              ) : null}
            </button>
          );
        })}
      </nav>
      <section className="sblist" aria-label="Sessions">
        <PlusButton disabled={!canStart} className="sbnew" label />
        <Group name="Needs you" items={needs} open={openId} />
        <Group name="Working" items={working} open={openId} />
        <Group name="Done" items={recent} open={openId} />
        {older > 0 ? (
          <button type="button" className="sbmore" onClick={() => void navigate({ to: "/sessions" })}>
            {older} older in Sessions
          </button>
        ) : null}
      </section>
      {mode() === "account" ? <Account /> : null}
    </aside>
  );
}
