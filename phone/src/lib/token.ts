// What the computer gave this phone when it paired. It lives in localStorage
// and, over https, in a cookie too: an iOS home-screen app gets its own
// storage but inherits Safari's cookies, so it starts paired instead of
// asking for a new code. The page sends it only inside the sealed channel
// (sealed.ts), so it has to be readable here: an HttpOnly cookie would not do.
// With it goes the label of the computer's key, pinned at pairing: the token
// goes to no other key.

export const TOKEN_KEY = "arakun.token";

export function cookieToken(): string {
  const m = document.cookie.match(/(?:^|; )arakun\.token=([^;]*)/);
  return m ? decodeURIComponent(m[1]) : "";
}

export function keepToken(t: string): void {
  if (t) localStorage.setItem(TOKEN_KEY, t);
  else localStorage.removeItem(TOKEN_KEY);
  if (location.protocol !== "https:") return;
  const age = t ? 400 * 24 * 3600 : 0;
  document.cookie = `arakun.token=${encodeURIComponent(t)}; Path=/; Max-Age=${age}; Secure; SameSite=Strict`;
}

/** The token, or "". Copies it to whichever of the two places lacks it. */
export function loadToken(): string {
  const t = localStorage.getItem(TOKEN_KEY) || cookieToken();
  if (t && (!localStorage.getItem(TOKEN_KEY) || (location.protocol === "https:" && !cookieToken()))) keepToken(t);
  return t;
}

export function forgetToken(): void {
  keepToken("");
  keepLabel("");
}

export const LABEL_KEY = "arakun.key";

/** The label of the computer's key this phone trusts, or "" before it knows one. */
export function loadLabel(): string {
  return localStorage.getItem(LABEL_KEY) || "";
}

export function keepLabel(label: string): void {
  if (label) localStorage.setItem(LABEL_KEY, label);
  else localStorage.removeItem(LABEL_KEY);
}
