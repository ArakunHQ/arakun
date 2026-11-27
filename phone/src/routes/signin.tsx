import { type FormEvent, useState } from "react";
import { useSaid } from "../lib/account";
import { Icon } from "../lib/icons";
import { toast } from "../lib/toast";
import "./signin.css";

const INSTALL = "curl -fsSL https://arakun.dev/install | sh";

function copyInstall() {
  // The clipboard needs a secure origin; the sign-in page is on https.
  navigator.clipboard?.writeText(INSTALL).then(
    () => toast("Copied"),
    () => toast("Could not copy. Select the line instead."),
  );
}

export function SignIn() {
  const said = useSaid();
  return (
    <div className="screen signin">
      <div className="sitop">Arakun</div>
      <main className="simain">
        <img className="simascot" src="mascot.svg" alt="" />
        <div className="sicopy">
          <div className="sieyebrow">app.arakun.dev</div>
          <h1>Reach your coding agents from any screen.</h1>
          <p>
            Sign in with the account you use in Arakun on your computer. Your sessions stay on that computer. This page talks to it end-to-end
            encrypted.
          </p>
          <a className="btn" href="/auth/github/start">
            <Icon name="github" size={20} />
            Sign in with GitHub
          </a>
          {said ? <div className="sierr">{said}</div> : null}
          <EmailSignIn />
          <div className="siinstall">
            <div className="silabel">
              <Icon name="monitor" />
              No Arakun yet? On your computer, run
            </div>
            <div className="siline">
              <code>
                <span className="siprompt">$</span> curl -fsSL <span className="siurl">https://arakun.dev/install</span> | sh
              </code>
              <button type="button" className="sicopybtn" aria-label="Copy the install command" onClick={copyInstall}>
                <Icon name="copy" />
              </button>
            </div>
          </div>
        </div>
      </main>
      <div className="sifoot">
        <span>End-to-end encrypted</span>
        <span>Works on phone, tablet and desktop</span>
      </div>
    </div>
  );
}

/** What the account host's answer says went wrong, or a sentence of our own. */
async function failure(res: Response, fallback: string): Promise<string> {
  try {
    const body = (await res.json()) as { error?: unknown };
    if (typeof body.error === "string" && body.error) return body.error.charAt(0).toUpperCase() + body.error.slice(1) + ".";
  } catch {
    // Not JSON: a proxy's page, say.
  }
  return fallback;
}

/**
 * Sign-in with a code mailed to the address, for people without GitHub. The
 * code is typed here, so it signs in this browser and no other. The account
 * host sets the session cookie, which this script never sees; the page then
 * starts again, signed in, as after GitHub's sign-in.
 */
export function EmailSignIn() {
  const [email, setEmail] = useState("");
  const [sentTo, setSentTo] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const ask = async (address: string) => {
    setBusy(true);
    setErr("");
    try {
      const res = await fetch("/auth/email", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ email: address }) });
      if (!res.ok) return setErr(await failure(res, "Could not send the code. Try again."));
      setSentTo(address);
      setCode("");
    } catch {
      setErr(`Could not reach ${location.host}. Try again.`);
    } finally {
      setBusy(false);
    }
  };

  const verify = async () => {
    setBusy(true);
    setErr("");
    try {
      const res = await fetch("/auth/email/web", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ email: sentTo, code }) });
      if (!res.ok) return setErr(await failure(res, "Could not sign in. Try again."));
      location.replace("/");
    } catch {
      setErr(`Could not reach ${location.host}. Try again.`);
    } finally {
      setBusy(false);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (sentTo === null) void ask(email.trim());
    else void verify();
  };

  return (
    <form className="siemail" onSubmit={submit}>
      <div className="sior">or with your email</div>
      {sentTo === null ? (
        <div className="sirow">
          <input
            type="email"
            aria-label="Email address"
            placeholder="you@example.com"
            autoComplete="email"
            required
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
          <button type="submit" className="btn outline" disabled={busy || !email.trim()}>
            Email me a code
          </button>
        </div>
      ) : (
        <>
          <div className="sisent">
            We sent a code to <b>{sentTo}</b>. Type it here. It works for 10 minutes.
          </div>
          <div className="sirow">
            <input
              className="sicode"
              aria-label="Code"
              placeholder="123456"
              inputMode="numeric"
              autoComplete="one-time-code"
              maxLength={7}
              autoFocus
              value={code}
              onChange={(e) => setCode(e.target.value)}
            />
            <button type="submit" className="btn" disabled={busy || code.replace(/\s/g, "").length !== 6}>
              Sign in
            </button>
          </div>
          <div className="silinks">
            <button type="button" className="silink" disabled={busy} onClick={() => void ask(sentTo)}>
              Send a new code
            </button>
            <button
              type="button"
              className="silink"
              disabled={busy}
              onClick={() => {
                setSentTo(null);
                setErr("");
              }}
            >
              Use another address
            </button>
          </div>
        </>
      )}
      {err ? <div className="sierr">{err}</div> : null}
    </form>
  );
}
