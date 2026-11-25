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
