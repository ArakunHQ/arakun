import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Icon, type IconName } from "../lib/icons";
import { useLink } from "../lib/link";
import { needsYou } from "../lib/sessions";
import { PlusButton } from "./PlusMenu";

type Tab = { to: "/inbox" | "/sessions" | "/usage" | "/settings"; label: string; icon: IconName };

// The words change in Task 9.
const LEFT: Tab[] = [
  { to: "/inbox", label: "Inbox", icon: "inbox" },
  { to: "/sessions", label: "Sessions", icon: "layers" },
];
const RIGHT: Tab[] = [
  { to: "/usage", label: "Usage", icon: "gauge" },
  { to: "/settings", label: "Phone", icon: "phone" },
];

/** The tabs under the top-level screens, a new session in the middle. */
export function TabBar() {
  const navigate = useNavigate();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const waiting = useLink((s) => s.inbox.filter(needsYou).length);
  const canStart = useLink((s) => s.connected && s.places.length > 0);
  const tab = (t: Tab) => {
    const on = path === t.to;
    return (
      <button key={t.to} className={`tab${on ? " on" : ""}`} aria-label={t.label} aria-current={on ? "page" : undefined} onClick={() => !on && void navigate({ to: t.to })}>
        <Icon name={t.icon} size={20} />
        <span>{t.label}</span>
        {t.to === "/inbox" && waiting ? <span className="tbadge">{waiting}</span> : null}
      </button>
    );
  };
  return (
    <nav className="tabs">
      {LEFT.map(tab)}
      <PlusButton disabled={!canStart} />
      {RIGHT.map(tab)}
    </nav>
  );
}
