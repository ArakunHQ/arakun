import { Navigate } from "@tanstack/react-router";
import { useContext, useSyncExternalStore } from "react";
import { clock } from "../lib/format";
import { Icon } from "../lib/icons";
import { type Link, LinkContext } from "../lib/link";

const noLink = () => () => {};

/**
 * The computer is not there: said out loud, since what is on screen is what
 * was last seen, and what is written now waits on the phone. Nothing while
 * the link is up.
 */
export function OfflineCard({ link }: { link: Link }) {
  const s = useSyncExternalStore(link.subscribe, link.getState);
  if (s.connected) return null;
  const host = s.host || "the computer";
  const title = s.pcOffline ? `Arakun on ${host} is away` : `Can't reach ${host}`;
  const why = s.pcOffline
    ? `Since ${clock(s.pcOffline)}. What you write waits here and goes out when it is back.`
    : "Reconnecting… What you write waits here and goes out when it is back.";
  return (
    <div className="offline">
      <Icon name="offline" size={20} />
      <div className="grow">
        <div className="t">{title}</div>
        <div className="s">{why}</div>
      </div>
      {s.pcOffline ? null : (
        <button className="btn outline small" onClick={() => link.retryNow()}>
          Retry
        </button>
      )}
    </div>
  );
}

export function Offline() {
  const link = useContext(LinkContext);
  const connected = useSyncExternalStore(link ? link.subscribe : noLink, () => !!link?.getState().connected);
  if (!link || connected) return <Navigate to="/inbox" replace />;
  return (
    <div className="screen">
      <div className="statusbar" />
      <OfflineCard link={link} />
    </div>
  );
}
