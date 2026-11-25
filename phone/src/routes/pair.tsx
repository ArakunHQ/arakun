import { useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { dropMachine } from "../lib/account";
import { pair } from "../lib/pair";
import { pairMessage, setPairMessage, startLocal } from "../lib/start";

// The computer's own server: a code from Arakun's Phone panel, traded for a token.
export function Pair() {
  const navigate = useNavigate();
  const [code, setCode] = useState("");
  const [pairing, setPairing] = useState(false);
  const [error, setError] = useState(pairMessage);

  const submit = async (ev: FormEvent) => {
    ev.preventDefault();
    if (code.length !== 6 || pairing) return;
    setPairing(true);
    setError("");
    const done = await pair(code);
    setPairing(false);
    if (!done.ok) return setError(done.error);
    setPairMessage("");
    // Start over with the new token.
    dropMachine();
    startLocal();
    void navigate({ to: "/inbox" });
  };

  return (
    <div className="screen">
      <div className="pair">
        <div className="big">🦝</div>
        <h1>Pair this phone</h1>
        <p>
          In Arakun, open <b>Phone</b> and press <b>Pair a phone</b>. Scan the code it shows, or type it here.
        </p>
        <form autoComplete="off" onSubmit={submit}>
          <input
            inputMode="numeric"
            pattern="[0-9]*"
            maxLength={6}
            placeholder="000000"
            value={code}
            disabled={pairing}
            aria-label="Pairing code"
            autoFocus
            onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
          />
          <button className="btn" type="submit" disabled={pairing || code.length !== 6}>
            {pairing ? "Pairing…" : "Pair"}
          </button>
        </form>
        <div className="err">{error}</div>
        <div className="hint">{location.hostname} · this phone only gets its own key; the computer can revoke it any time.</div>
      </div>
    </div>
  );
}
