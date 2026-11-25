import { createFileRoute } from "@tanstack/react-router";

interface SignInSearch {
  sign_in?: "denied" | "failed";
  signed_out?: 1;
}

export const Route = createFileRoute("/sign-in")({
  validateSearch: (search: Record<string, unknown>): SignInSearch => ({
    sign_in: search.sign_in === "denied" || search.sign_in === "failed" ? search.sign_in : undefined,
    // The router reads ?signed_out=1 as the number 1.
    signed_out: search.signed_out === 1 || search.signed_out === "1" ? 1 : undefined,
  }),
  component: SignIn,
});

const MESSAGES = {
  denied: "This GitHub account is not an operator.",
  failed: "Sign-in failed. Try again.",
  signedOut: "You were signed out. The last change was not made.",
};

function SignIn() {
  const { sign_in, signed_out } = Route.useSearch();
  const message = sign_in ? MESSAGES[sign_in] : signed_out ? MESSAGES.signedOut : null;
  return (
    <main className="sign-in">
      <div className="sign-in-panel">
        <p className="wordmark">
          Arakun <span className="wordmark-sub">admin</span>
        </p>
        <h1>Sign in</h1>
        <p className="muted">The operator panel of Arakun's cloud. Only the GitHub accounts on the operator list can sign in.</p>
        {message && (
          <p className="message" data-tone={sign_in ? "error" : "attention"} role="alert">
            {message}
          </p>
        )}
        {/* A full page load: the Worker answers this path, not the router. */}
        <a className="button primary large" href="/auth/github/start">
          Sign in with GitHub
        </a>
      </div>
    </main>
  );
}
