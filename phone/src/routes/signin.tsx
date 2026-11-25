import { useSaid } from "../lib/account";

export function SignIn() {
  const said = useSaid();
  return (
    <div className="screen">
      <div className="pair">
        <div className="big">🦝</div>
        <h1>Sign in to reach your computers</h1>
        <p>Sign in with the GitHub account you use in Arakun on your computer.</p>
        <a className="btn" href="/auth/github/start">
          Sign in with GitHub
        </a>
        <div className="err">{said}</div>
        <div className="hint">{location.host} · end-to-end encrypted</div>
      </div>
    </div>
  );
}
