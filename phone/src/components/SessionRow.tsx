import { useNavigate } from "@tanstack/react-router";
import { useContext } from "react";
import { ago, statusLabel } from "../lib/format";
import { Icon } from "../lib/icons";
import { LinkContext } from "../lib/link";
import type { InboxSession } from "../lib/model";
import { asksKind, asksLine } from "../lib/sessions";
import { openSession } from "../lib/start";
import "./SessionRow.css";

/** Who runs a session, as a tile: Claude's mark, the cloud for Workers AI, else the agent's first letter. */
export function AgentAvatar({ agent }: { agent?: string }) {
  const a = String(agent || "").toLowerCase();
  if (a.includes("claude"))
    return (
      <span className="av claude">
        <Icon name="claude" size={20} />
      </span>
    );
  if (a.includes("workers"))
    return (
      <span className="av">
        <Icon name="cloud" size={20} />
      </span>
    );
  return <span className="av">{(a[0] || "?").toUpperCase()}</span>;
}

/** Opens a session from a list: through the link first, so the screen finds it open. */
export function useOpenSession(): (id: string) => void {
  const link = useContext(LinkContext);
  const navigate = useNavigate();
  return (id) => {
    if (link) openSession(link, id);
    void navigate({ to: "/s/$session", params: { session: id } });
  };
}

/** A row of the inbox (`inbox`: what it asks, the changes) or of Sessions (`list`: its status). */
export function SessionRow({ s, variant }: { s: InboxSession; variant: "inbox" | "list" }) {
  const openRow = useOpenSession();
  const open = () => openRow(s.id);
  const asks = variant === "inbox" ? asksLine(s) : "";
  const status = s.status ?? "idle";
  const sub =
    variant === "inbox"
      ? [s.project, s.env && s.env !== s.project ? s.env : ""]
      : [s.env && s.env !== s.project ? s.env : ""];
  const parts = [sub.filter(Boolean).join(" · "), s.agent].filter(Boolean) as string[];
  return (
    <button className={`srow ${status}${variant === "inbox" && s.asks ? ` asks-${s.asks.kind}` : ""}`} onClick={open}>
      <AgentAvatar agent={s.agent} />
      <span className="body">
        <div className="t">{s.title || (variant === "inbox" ? "Untitled session" : "New session")}</div>
        {asks ? (
          <div className={`ask-line ${asksKind(s)}`}>{asks}</div>
        ) : (
          <div className="s">
            {parts.map((x, i) => (
              <span key={i} className={i === parts.length - 1 && s.agent ? "agent" : undefined}>
                {x}
              </span>
            ))}
          </div>
        )}
      </span>
      <span className="right">
        <span className="time">{ago(s.updated ?? 0)}</span>
        {variant === "inbox" ? (
          s.added || s.removed ? (
            <span className="stats">
              <span className="add">+{s.added}</span> <span className="del">−{s.removed}</span>
            </span>
          ) : null
        ) : (
          <span className={`badge ${status}`}>
            <span className="dot" />
            {statusLabel(status)}
          </span>
        )}
      </span>
    </button>
  );
}
