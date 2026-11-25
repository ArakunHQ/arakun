import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Icon, type IconName } from "../../lib/icons";
import { useLink } from "../../lib/link";
import { needsYou } from "../../lib/sessions";
import { PlusButton } from "../PlusMenu";
import { AgentAvatar, useOpenSession } from "../SessionRow";
import { ComputerPicker } from "./ComputerPicker";

type Tab = { to: "/inbox" | "/sessions" | "/usage" | "/settings"; label: string; icon: IconName };

export const WIDE_TABS: Tab[] = [
  { to: "/inbox", label: "Inbox", icon: "inbox" },
  { to: "/sessions", label: "Sessions", icon: "chats" },
  { to: "/usage", label: "Usage", icon: "gauge" },
  { to: "/settings", label: "Settings", icon: "settings" },
];

/** A tablet's column on the left: the tabs, the +, a tile per live session, the computer. */
export function Rail() {
  const navigate = useNavigate();
  const openSession = useOpenSession();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const inbox = useLink((s) => s.inbox);
  const canStart = useLink((s) => s.connected && s.places.length > 0);
  // What waits on you first, then what runs; what ended is in the inbox.
  const live = [...inbox.filter(needsYou), ...inbox.filter((s) => s.status === "working")];
  const waiting = inbox.some(needsYou);
  const open = /^\/s\/([^/]+)$/.exec(path)?.[1];
  const openId = open ? decodeURIComponent(open) : "";

  return (
    <nav className="rail" aria-label="Tabs">
      <img className="rlogo" src="icon-small.svg" alt="" />
      {WIDE_TABS.map((t) => {
        const on = path === t.to;
        return (
          <button key={t.to} type="button" className={`rtile${on ? " on" : ""}`} aria-label={t.label} title={t.label} aria-current={on ? "page" : undefined} onClick={() => !on && void navigate({ to: t.to })}>
            <Icon name={t.icon} size={20} />
            {t.to === "/inbox" && waiting ? <span className="rdot needs" /> : null}
          </button>
        );
      })}
      <PlusButton disabled={!canStart} className="rtile rnew" />
      {live.length ? <div className="rsep" /> : null}
      <div className="rlive">
        {live.map((s) => {
          const needs = needsYou(s);
          const title = s.title || "Untitled session";
          const on = s.id === openId;
          return (
            <button
              key={s.id}
              type="button"
              className={`rtile rsess${on ? " on" : ""}`}
              aria-label={`${title}, ${needs ? "needs you" : "working"}`}
              title={title}
              aria-current={on ? "page" : undefined}
              onClick={() => !on && openSession(s.id)}
            >
              <AgentAvatar agent={s.agent} />
              <span className={`rdot ${needs ? "needs" : "working"}`} />
            </button>
          );
        })}
      </div>
      <ComputerPicker look="icon" />
    </nav>
  );
}
