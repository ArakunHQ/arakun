import { createFileRoute } from "@tanstack/react-router";
import { useState, type FormEvent } from "react";
import { isOutcome, type SignInOutcome } from "../signIn";

interface SignInSearch {
  sign_in?: SignInOutcome;
  signed_out?: 1;
  step?: "code";
}

export const Route = createFileRoute("/sign-in")({
  validateSearch: (search: Record<string, unknown>): SignInSearch => ({
    sign_in: isOutcome(search.sign_in) ? search.sign_in : undefined,
    // The router reads ?signed_out=1 as the number 1.
    signed_out: search.signed_out === 1 || search.signed_out === "1" ? 1 : undefined,
    step: search.step === "code" ? "code" : undefined,
  }),
  component: SignIn,
});

const MESSAGES = {
  denied: "This GitHub account is not an operator.",
  failed: "Sign-in failed. Try again.",
  no_key: "This operator has no authenticator app set up. Ask a maintainer to add one.",
  signedOut: "You were signed out. The last change was not made.",
};

/// What the Worker's POST /auth/code answers, as the operator should read it.
const CODE_ERRORS: Record<string, string> = {
  "wrong code": "That code is not right. Try the one your app shows now.",
  "sign-in expired": "The sign-in took too long. Sign in with GitHub again.",
  "too many codes; wait a minute": "Too many codes. Wait a minute, then try again.",
};

function SignIn() {
  const { sign_in, signed_out, step } = Route.useSearch();
  const message = sign_in ? MESSAGES[sign_in] : signed_out ? MESSAGES.signedOut : null;
  return (
    <main className="sign-in">
      <div className="sign-in-panel">
        <p className="wordmark">
          Arakun <span className="wordmark-sub">admin</span>
        </p>
        {step === "code" ? (
          <CodeStep />
        ) : (
          <>
            <h1>Sign in</h1>
            <p className="muted">The operator panel of Arakun's cloud. Only the GitHub accounts on the operator list can sign in.</p>
            {message && (
              <p className="message" data-tone={sign_in ? "error" : "attention"} role="alert">
                {message}
              </p>
            )}
            <GitHubButton />
          </>
        )}
      </div>
    </main>
  );
}

function GitHubButton() {
  // A full page load: the Worker answers this path, not the router.
  return (
    <a className="button primary large" href="/auth/github/start">
      Sign in with GitHub
    </a>
  );
}

/// The second step: GitHub said who this is, and the Worker holds the
/// session until the authenticator app's code is right.
function CodeStep() {
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [expired, setExpired] = useState(false);
  const [sending, setSending] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSending(true);
    setError(null);
    try {
      const res = await fetch("/auth/code", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ code }),
      });
      if (res.ok) {
        // A full load, so the panel starts again with the session.
        window.location.assign("/");
        return;
      }
      const text = await errorText(res);
      setExpired(text === "sign-in expired");
      setError(CODE_ERRORS[text] ?? `The server answered ${res.status}.`);
      setCode("");
    } catch {
      setError("The server did not answer. Try again.");
    }
    setSending(false);
  }

  return (
    <>
      <h1>Enter your code</h1>
      <p className="muted">Open your authenticator app and enter the six-digit code for Arakun admin.</p>
      {error && (
        <p className="message" data-tone="error" role="alert">
          {error}
        </p>
      )}
      {expired ? (
        <GitHubButton />
      ) : (
        <form className="form" onSubmit={(e) => void submit(e)}>
          <label className="field">
            <span>Code</span>
            <input
              className="input"
              name="code"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9 ]*"
              maxLength={7}
              required
              autoFocus
              value={code}
              onChange={(e) => setCode(e.target.value)}
            />
          </label>
          <button type="submit" className="button primary large" disabled={sending || code.replace(/\s/g, "").length !== 6}>
            Continue
          </button>
        </form>
      )}
    </>
  );
}

/// The Worker's `error` text, or "" for an answer that is not its JSON.
async function errorText(res: Response): Promise<string> {
  try {
    const body: unknown = await res.json();
    const error = typeof body === "object" && body !== null ? (body as { error?: unknown }).error : null;
    return typeof error === "string" ? error : "";
  } catch {
    return "";
  }
}
