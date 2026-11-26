// The agent asks for a secret (`request_secret`): masked inputs it never sees,
// with Save and Decline. What is typed goes to the computer, which keeps it and
// hands the agent a handle; it stays out of the transcript. The computer says
// whether this connection is encrypted (`private`); over plain http the card
// offers only Decline, so a secret never crosses the network in the clear.

import { useState } from "react";
import { Icon } from "../../lib/icons";
import "./QuestionCard.css";
import "./SecretCard.css";

export interface SecretRequest {
  id: number;
  label: string;
  reason: string;
  url?: string | null;
  fields: { name: string; label: string }[];
  private: boolean;
}

export interface SecretCardProps {
  secret: SecretRequest;
  onSave: (values: string[]) => void;
  onDecline: () => void;
}

export const NOT_PRIVATE =
  "This connection is not encrypted, so the secret can't be sent over it. Decline, or answer from a device that reaches the computer through your account or Tailscale.";

export function SecretCard({ secret: r, onSave, onDecline }: SecretCardProps) {
  const [values, setValues] = useState<string[]>(() => r.fields.map(() => ""));
  const [shown, setShown] = useState<boolean[]>(() => r.fields.map(() => false));
  const ready = r.private && values.every((v) => v.length > 0);
  const save = () => {
    if (!ready) return;
    const sent = values;
    setValues(r.fields.map(() => ""));
    onSave(sent);
  };
  // Only http(s) links: the computer checks, and so does the page.
  const url = r.url && /^https?:\/\//.test(r.url) ? r.url : null;

  return (
    <div className="ask secret">
      <div className="ahead">
        <span className="label">
          <span className="dot needs" />
          Secret
        </span>
        <span className="keep">Stays out of the transcript</span>
      </div>
      <div className="body">
        <div className="text">{r.label}</div>
        <div className="why">{r.reason}</div>
        {url ? (
          <a className="where" href={url} target="_blank" rel="noopener noreferrer">
            {url}
          </a>
        ) : null}
        {r.private ? (
          r.fields.map((f, i) => (
            <label key={f.name} className="field">
              <span>{f.label}</span>
              <span className="box">
                <input
                  className="own"
                  type={shown[i] ? "text" : "password"}
                  value={values[i]}
                  autoComplete="off"
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  enterKeyHint={i === r.fields.length - 1 ? "send" : "next"}
                  onChange={(ev) => setValues(values.map((v, k) => (k === i ? ev.target.value : v)))}
                  onKeyDown={(ev) => {
                    if (ev.key !== "Enter" || ev.nativeEvent.isComposing) return;
                    ev.preventDefault();
                    save();
                  }}
                />
                <button
                  type="button"
                  className="reveal"
                  aria-label={shown[i] ? `Hide ${f.label}` : `Show ${f.label}`}
                  onClick={() => setShown(shown.map((s, k) => (k === i ? !s : s)))}
                >
                  {shown[i] ? "Hide" : "Show"}
                </button>
              </span>
            </label>
          ))
        ) : (
          <div className="clear" role="alert">
            <Icon name="alert" />
            <span>{NOT_PRIVATE}</span>
          </div>
        )}
      </div>
      <div className="actions">
        <button type="button" className="btn outline" onClick={onDecline}>
          Decline
        </button>
        {r.private ? (
          <button type="button" className="btn grow" disabled={!ready} onClick={save}>
            Save
          </button>
        ) : null}
      </div>
    </div>
  );
}
