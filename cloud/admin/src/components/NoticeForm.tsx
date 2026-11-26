import { useState } from "react";
import { parseId } from "../format";
import { useCreateNotice } from "../queries";
import type { NewNotice } from "../types";
import { Confirm } from "./Confirm";

const MAX_TEXT = 500;

/// Unix seconds for the last second of a day (`YYYY-MM-DD`) in the
/// operator's own time zone, or null for a date the input cannot give.
function endOfDay(date: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return null;
  const next = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]) + 1);
  return Math.floor(next.getTime() / 1000) - 1;
}

/// An https address with a host, which is all the Worker will take.
function isHttps(url: string): boolean {
  try {
    const u = new URL(url);
    return u.protocol === "https:" && u.hostname !== "";
  } catch {
    return false;
  }
}

/// A notice for one account (`accountId`), or, without it, for everyone or an
/// account whose ID is typed in. A notice to everyone asks first.
export function NoticeForm({ accountId }: { accountId?: number }) {
  const create = useCreateNotice();
  const [text, setText] = useState("");
  const [url, setUrl] = useState("");
  const [expires, setExpires] = useState("");
  const [everyone, setEveryone] = useState(true);
  const [target, setTarget] = useState("");
  const [sent, setSent] = useState(false);

  const targetId = accountId ?? (everyone ? undefined : parseId(target.trim()));
  // The first thing wrong, in the order of the form; null when it can be sent.
  const problem =
    text.length < 1
      ? "Write the notice text."
      : text.length > MAX_TEXT
        ? `The text is at most ${MAX_TEXT} characters.`
        : url !== "" && !(url.startsWith("https://") && isHttps(url))
          ? "The link must be an https:// address."
          : expires !== "" && endOfDay(expires) === null
            ? "Choose a date for the end."
            : expires !== "" && endOfDay(expires)! <= Date.now() / 1000
              ? "Choose a day that has not passed."
              : targetId === null
              ? "Enter an account ID: digits only."
              : null;
  const touched = text !== "" || url !== "" || target !== "";

  function send() {
    if (problem !== null) return;
    const notice: NewNotice = { text };
    if (targetId !== undefined && targetId !== null) notice.account_id = targetId;
    if (url !== "") notice.url = url;
    if (expires !== "") notice.expires_at = endOfDay(expires)!;
    setSent(false);
    create.mutate(notice, {
      onSuccess: () => {
        setText("");
        setUrl("");
        setExpires("");
        setSent(true);
      },
    });
  }

  const toEveryone = accountId === undefined && everyone;
  return (
    <form
      className="form"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        // A notice to everyone goes through its Confirm instead.
        if (!toEveryone) send();
      }}
    >
      {accountId === undefined && (
        <fieldset className="field">
          <legend>Send to</legend>
          <label className="choice">
            <input type="radio" name="notice-target" checked={everyone} onChange={() => setEveryone(true)} />
            Everyone
          </label>
          <label className="choice">
            <input type="radio" name="notice-target" checked={!everyone} onChange={() => setEveryone(false)} />
            One account
          </label>
          {!everyone && (
            <input
              className="input"
              aria-label="Account ID"
              inputMode="numeric"
              placeholder="Account ID"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
            />
          )}
        </fieldset>
      )}
      <div className="field">
        <label className="field">
          <span>Text</span>
          <textarea className="input" rows={3} value={text} onChange={(e) => setText(e.target.value)} />
        </label>
        <span className="muted hint">
          {text.length} of {MAX_TEXT}
        </span>
      </div>
      <label className="field">
        <span>Link (optional)</span>
        <input className="input" type="url" placeholder="https://" value={url} onChange={(e) => setUrl(e.target.value)} />
      </label>
      <label className="field">
        <span>Ends on (optional)</span>
        <input className="input" type="date" value={expires} onChange={(e) => setExpires(e.target.value)} />
      </label>
      <div className="form-actions">
        {toEveryone ? (
          <Confirm
            title="Send this notice to everyone?"
            body={<p>Every account will see it until it ends or you take it back.</p>}
            confirmLabel="Send to everyone"
            disabled={problem !== null || create.isPending}
            onConfirm={send}
          >
            Send notice
          </Confirm>
        ) : (
          <button type="submit" className="button primary" disabled={problem !== null || create.isPending}>
            Send notice
          </button>
        )}
        {problem !== null && touched && <span className="muted">{problem}</span>}
        {create.error && (
          <p className="message" data-tone="error" role="alert">
            {create.error.message}
          </p>
        )}
        {sent && !create.error && (
          <p className="message" data-tone="success" role="status">
            Notice sent.
          </p>
        )}
      </div>
    </form>
  );
}
