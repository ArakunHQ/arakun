// What the computer gave this phone when it paired: the token, and the label
// of the key that signs the computer's handshake, which every connection is
// checked against. Both live in localStorage and, over https, in a cookie too:
// an iOS home-screen app gets its own storage but inherits Safari's cookies,
// so it starts paired instead of asking for a new code. The token goes to the
// computer only inside the sealed channel (sealed.ts), so the page has to be
// able to read it: an HttpOnly cookie would not do.

export const TOKEN_KEY = "arakun.token";
export const PIN_KEY = "arakun.key";

function cookie(name: string): string {
  const m = document.cookie.match(new RegExp(`(?:^|; )${name.replace(".", "\\.")}=([^;]*)`));
  return m ? decodeURIComponent(m[1]) : "";
}

function keep(name: string, value: string): void {
  if (value) localStorage.setItem(name, value);
  else localStorage.removeItem(name);
  if (location.protocol !== "https:") return;
  const age = value ? 400 * 24 * 3600 : 0;
  document.cookie = `${name}=${encodeURIComponent(value)}; Path=/; Max-Age=${age}; Secure; SameSite=Strict`;
}

/** The value, or "". Copies it to whichever of the two places lacks it. */
function load(name: string): string {
  const v = localStorage.getItem(name) || cookie(name);
  if (v && (!localStorage.getItem(name) || (location.protocol === "https:" && !cookie(name)))) keep(name, v);
  return v;
}

export function cookieToken(): string {
  return cookie(TOKEN_KEY);
}

export function keepToken(t: string): void {
  keep(TOKEN_KEY, t);
}

export function loadToken(): string {
  return load(TOKEN_KEY);
}

/** The label of the computer's key, or "" before the first sealed connection. */
export function loadPin(): string {
  return load(PIN_KEY);
}

export function keepPin(label: string): void {
  keep(PIN_KEY, label);
}

/** Forgets the pairing: the token, and the key it was made with. */
export function forgetToken(): void {
  keep(TOKEN_KEY, "");
  keep(PIN_KEY, "");
}
