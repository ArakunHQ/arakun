import { clock } from "../lib/format";
import { useLink } from "../lib/link";
import "./StatusBar.css";

/** Empty while connected: it only keeps the header clear of the notch. Says what is wrong otherwise. */
export function StatusBar() {
  const connected = useLink((s) => s.connected);
  const host = useLink((s) => s.host);
  const pcOffline = useLink((s) => s.pcOffline);
  const word = connected ? "connected" : pcOffline ? `computer away since ${clock(pcOffline)}` : "reconnecting…";
  return (
    <div className={`statusbar${connected ? "" : " off"}`}>
      <span className="dot" />
      <span>{host || location.hostname}</span>
      <span className="spacer" />
      <span>{word}</span>
    </div>
  );
}
