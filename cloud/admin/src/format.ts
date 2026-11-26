// Times on the wire are Unix seconds; these turn them into words.

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const MONTH = 30 * DAY;
const YEAR = 365 * DAY;

function count(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? "" : "s"}`;
}

/// How long ago `seconds` was at `now`, both Unix seconds: "5 days", "3 years".
/// Days up to two months and months up to two years, so a GitHub account's
/// age reads as the reviewer would say it.
export function age(seconds: number, now: number): string {
  const d = Math.max(0, now - seconds);
  if (d < MINUTE) return "under a minute";
  if (d < HOUR) return count(Math.floor(d / MINUTE), "minute");
  if (d < DAY) return count(Math.floor(d / HOUR), "hour");
  if (d < 60 * DAY) return count(Math.floor(d / DAY), "day");
  if (d < 2 * YEAR) return count(Math.floor(d / MONTH), "month");
  return count(Math.floor(d / YEAR), "year");
}

/// "2 days ago", "in 2 days" or "just now".
export function relative(seconds: number, now: number): string {
  if (Math.abs(seconds - now) < MINUTE) return "just now";
  return seconds < now ? `${age(seconds, now)} ago` : `in ${age(now, seconds)}`;
}

/// The exact local time, for a title.
export function exact(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" });
}

/// GitHub's own rule for a login, as the cloud Worker checks it.
const GITHUB_LOGIN = /^[A-Za-z0-9_-]{1,39}$/;

/// The account's page on GitHub, or null for a login that is empty (an
/// account set up before its first sign-in) or not one GitHub allows. A link
/// is never built from text that does not pass.
/// The avatar of the GitHub user linked to an account, or null without one.
export function avatarUrl(githubId: number | null, size: number): string | null {
  return githubId === null ? null : `https://avatars.githubusercontent.com/u/${githubId}?s=${size}`;
}

export function githubUrl(login: string): string | null {
  return GITHUB_LOGIN.test(login) ? `https://github.com/${login}` : null;
}

/// The cloud Worker's rule for an account ID or a GitHub ID (`NUMERIC_ID` in
/// cloud/src/util.ts).
const NUMERIC_ID = /^[1-9][0-9]{0,15}$/;

/// The number in `text` if it is an account ID or a GitHub ID the Worker
/// accepts, else null.
export function parseId(text: string): number | null {
  if (!NUMERIC_ID.test(text)) return null;
  const id = Number(text);
  return Number.isSafeInteger(id) ? id : null;
}

/// A beta platform as people write it. Anything else is shown as it came.
export function platformName(platform: unknown): string {
  return platform === "ios" ? "iOS" : platform === "android" ? "Android" : String(platform);
}
