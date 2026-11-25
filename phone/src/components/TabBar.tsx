import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Icon, type IconName } from "../lib/icons";
import { useLink } from "../lib/link";
import { needsYou } from "../lib/sessions";
import { mode } from "../lib/start";
import "./TabBar.css";

type Tab = { to: "/sessions" | "/inbox" | "/computers" | "/settings"; label: string; icon: IconName };

// As in the iOS app. The + sits in the top bar, Usage is a row in Settings.
const TABS: Tab[] = [
  { to: "/sessions", label: "Sessions", icon: "chats" },
  { to: "/inbox", label: "Inbox", icon: "inbox" },
  { to: "/computers", label: "Computers", icon: "monitor" },
  { to: "/settings", label: "Settings", icon: "settings" },
];

/** The paths that show the tab bar on a phone, Usage under Settings. */
export const TAB_PATHS = ["/sessions", "/inbox", "/computers", "/settings", "/usage"];

/** The tabs under a phone's top-level screens. */
export function TabBar() {
  const navigate = useNavigate();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const waiting = useLink((s) => s.inbox.filter(needsYou).length);
  // The computer's own server has one computer: no list to pick from.
  const tabs = mode() === "account" ? TABS : TABS.filter((t) => t.to !== "/computers");
  const tab = (t: Tab) => {
    const on = path === t.to || (t.to === "/settings" && path === "/usage");
    return (
      <button key={t.to} className={`tab${on ? " on" : ""}`} aria-label={t.label} aria-current={on ? "page" : undefined} onClick={() => path !== t.to && void navigate({ to: t.to })}>
        <Icon name={t.icon} size={20} />
        <span>{t.label}</span>
        {t.to === "/inbox" && waiting ? <span className="tbadge">{waiting}</span> : null}
      </button>
    );
  };
  return (
    <nav className="tabs" aria-label="Tabs">
      {tabs.map(tab)}
    </nav>
  );
}
