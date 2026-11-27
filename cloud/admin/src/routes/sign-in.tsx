// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState, type FormEvent } from "react";
import { AuthenticatorSetup, errorText, post } from "../components/AuthenticatorSetup";
import { isOutcome, type SignInOutcome } from "../signIn";

type Step = "code" | "setup";

interface SignInSearch {
  sign_in?: SignInOutcome;
  signed_out?: 1;
  step?: Step;
}

export const Route = createFileRoute("/sign-in")({
  validateSearch: (search: Record<string, unknown>): SignInSearch => ({
    sign_in: isOutcome(search.sign_in) ? search.sign_in : undefined,
    // The router reads ?signed_out=1 as the number 1.
    signed_out: search.signed_out === 1 || search.signed_out === "1" ? 1 : undefined,
    step: search.step === "code" || search.step === "setup" ? search.step : undefined,
  }),
  component: SignIn,
});

const MESSAGES = {
  denied: "This GitHub account is not an operator.",
  failed: "Sign-in failed. Try again.",
  reset: "Your authenticator app can no longer be read. Ask a maintainer to reset it, then set it up again.",
  signedOut: "You were signed out. The last change was not made.",
};

/// What the Worker's POST /auth/code answers, as the operator should read it.
const CODE_ERRORS: Record<string, string> = {
  "wrong code": "That code is not right. Try the one your app shows now.",
  "code used": "That code was just used. Wait for the next one.",
  "sign-in expired": "The sign-in took too long. Sign in with GitHub again.",
  "key unreadable": MESSAGES.reset,
  "too many codes; wait a minute": "Too many codes. Wait a minute, then try again.",
};

/// A full load, so the panel starts again with the session.
function enterPanel() {
  window.location.assign("/");
}

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
        ) : step === "setup" ? (
          <SetupStep />
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

/// The first sign-in: GitHub said who this is, and there is no key yet.
function SetupStep() {
  const navigate = useNavigate();
  const [expired, setExpired] = useState(false);
  return (
    <>
      <h1>Set up your authenticator app</h1>
      <p className="muted">The panel asks for a code from an authenticator app at every sign-in, after GitHub. Set one up now.</p>
      {expired ? (
        <GitHubButton />
      ) : (
        <AuthenticatorSetup
          replacing={false}
          onDone={enterPanel}
          onFailed={(error) => {
            if (error === "a key exists") void navigate({ to: "/sign-in", search: { step: "code" } });
            else if (error === "sign-in expired") setExpired(true);
          }}
        />
      )}
    </>
  );
}

/// The second step: GitHub said who this is, and the Worker holds the
/// session until the authenticator app's code is right.
function CodeStep() {
  const navigate = useNavigate();
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [expired, setExpired] = useState(false);
  const [sending, setSending] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSending(true);
    setError(null);
    try {
      const res = await post("/auth/code", { code });
      if (res.ok) {
        enterPanel();
        return;
      }
      const text = await errorText(res);
      // The key was reset since GitHub's answer: set up a new one.
      if (text === "no key") {
        void navigate({ to: "/sign-in", search: { step: "setup" } });
        return;
      }
      setExpired(text === "sign-in expired" || text === "key unreadable");
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
