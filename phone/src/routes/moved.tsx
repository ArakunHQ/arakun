import { RELAY } from "../lib/globals";
import "./pair.css";

// An old relay host, `<label>.<base>`, from before sign-in.
export function Moved() {
  const host = RELAY.accountHost();
  return (
    <div className="screen">
      <div className="pair">
        <img className="mascot" src="mascot.svg" alt="" />
        <h1>This address has moved</h1>
        <p>{RELAY.sentence("update")}</p>
        <a className="btn" href={`https://${host}/`}>
          Open {host}
        </a>
        <div className="hint">{location.hostname}</div>
      </div>
    </div>
  );
}
