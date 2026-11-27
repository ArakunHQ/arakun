// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { useEffect, useRef, useState, type FormEvent } from "react";
import { QrCode } from "./QrCode";

/// What the Worker's /auth/setup routes answer, as the operator should read it.
const ERRORS: Record<string, string> = {
  "wrong code": "That code is not right. Try the one the new app shows now.",
  "wrong current code": "The code from your current app is not right.",
  "no current code": "Enter a code from your current app too.",
  "code used": "That code was just used. Wait for the next one.",
  "setup expired": "This setup took too long. Start again.",
  "sign-in expired": "The sign-in took too long. Sign in with GitHub again.",
  "a key exists": "An authenticator app is already set up for this account. Sign in with its code.",
  "too many codes; wait a minute": "Too many codes. Wait a minute, then try again.",
};

/// The Worker's `error` text, or "" for an answer that is not its JSON.
export async function errorText(res: Response): Promise<string> {
  try {
    const body: unknown = await res.json();
    const error = typeof body === "object" && body !== null ? (body as { error?: unknown }).error : null;
    return typeof error === "string" ? error : "";
  } catch {
    return "";
  }
}

export function post(path: string, body?: unknown): Promise<Response> {
  return fetch(path, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

/// Six digits, with the space some apps show in the middle.
function sixDigits(code: string): boolean {
  return /^\d{6}$/.test(code.replace(/\s/g, ""));
}

interface Props {
  /// Settings: replacing a key needs a code from the current app too.
  replacing: boolean;
  /// After the new key is stored. `error` is the Worker's text when it failed for good.
  onDone: () => void;
  onFailed?: (error: string) => void;
}

/// Makes a new key, shows it as a QR code and as text, and stores it once a
/// code from the new app is right.
export function AuthenticatorSetup({ replacing, onDone, onFailed }: Props) {
  const [key, setKey] = useState<{ key: string; uri: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [current, setCurrent] = useState("");
  const [sending, setSending] = useState(false);
  const started = useRef(false);

  async function start() {
    setError(null);
    setKey(null);
    try {
      const res = await post("/auth/setup/start");
      if (res.ok) {
        setKey((await res.json()) as { key: string; uri: string });
        return;
      }
      const text = await errorText(res);
      setError(ERRORS[text] ?? `The server answered ${res.status}.`);
      onFailed?.(text);
    } catch {
      setError("The server did not answer. Try again.");
    }
  }

  useEffect(() => {
    // Once: React runs effects twice in development, and each start makes a new key.
    if (started.current) return;
    started.current = true;
    void start();
  });

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSending(true);
    setError(null);
    try {
      const res = await post("/auth/setup", replacing ? { code, current_code: current } : { code });
      if (res.ok) {
        onDone();
        return;
      }
      const text = await errorText(res);
      setError(ERRORS[text] ?? `The server answered ${res.status}.`);
      setCode("");
      if (text === "wrong current code" || text === "code used") setCurrent("");
      if (text === "setup expired") void start();
      if (text === "sign-in expired" || text === "a key exists") onFailed?.(text);
    } catch {
      setError("The server did not answer. Try again.");
    }
    setSending(false);
  }

  return (
    <div className="setup">
      {error && (
        <p className="message" data-tone="error" role="alert">
          {error}
        </p>
      )}
      {key ? (
        <>
          <ol className="setup-steps">
            <li>Open an authenticator app on your phone, such as 1Password, Google Authenticator or Authy.</li>
            <li>Scan this code with it. On the phone itself, open the link instead.</li>
          </ol>
          <QrCode text={key.uri} label="QR code for your authenticator app" />
          <p className="hint muted">
            Or type this key: <code className="setup-key">{key.key.replace(/(.{4})/g, "$1 ").trim()}</code>{" "}
            <a href={key.uri}>Open in app</a>
          </p>
          <form className="form" onSubmit={(e) => void submit(e)}>
            {replacing && (
              <label className="field">
                <span>Code from your current app</span>
                <input
                  className="input"
                  name="current_code"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  maxLength={7}
                  required
                  value={current}
                  onChange={(e) => setCurrent(e.target.value)}
                />
              </label>
            )}
            <label className="field">
              <span>{replacing ? "Code from the new app" : "Code from the app"}</span>
              <input
                className="input"
                name="code"
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={7}
                required
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </label>
            <button type="submit" className="button primary" disabled={sending || !sixDigits(code) || (replacing && !sixDigits(current))}>
              {replacing ? "Replace authenticator app" : "Finish setup"}
            </button>
          </form>
        </>
      ) : (
        !error && <p className="muted">Making a key…</p>
      )}
    </div>
  );
}
