// How the Worker's GitHub callback ends when it does not reach the code step:
// it sends the browser to /?sign_in=<outcome>, and the sign-in screen says why.

export type SignInOutcome = "denied" | "failed" | "no_key";

export function isOutcome(value: unknown): value is SignInOutcome {
  return value === "denied" || value === "failed" || value === "no_key";
}
