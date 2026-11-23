// What KV says about how much a label may do, and whether the relay is open at
// all. Reading KV instead of baking numbers in lets the limits move without a
// deploy. Reads are few: two per socket opened, one a minute per object for
// the plan. No edge cache on them, so a change lands at once.
//
// Keys: `plans` → `{ free: Plan, … }`; `disabled` → any text (shown to whoever
// asks why); `blocked` → JSON array of labels.

export interface Plan {
  /// Phones connected at once.
  phones: number;
  /// Frames relayed per second.
  burst: number;
  /// Frames relayed per UTC day.
  daily: number;
  /// Bytes in one frame's payload.
  frame: number;
}

export const FREE: Plan = { phones: 3, burst: 50, daily: 20_000, frame: 256 * 1024 };

/// The plan under a name, over the free plan's defaults for anything KV left out.
export async function planFor(kv: KVNamespace, name = "free"): Promise<Plan> {
  const plans = await kv.get<Record<string, Partial<Plan>>>("plans", "json").catch(() => null);
  const given = plans?.[name] ?? plans?.["free"] ?? {};
  return {
    phones: positive(given.phones, FREE.phones),
    burst: positive(given.burst, FREE.burst),
    daily: positive(given.daily, FREE.daily),
    frame: positive(given.frame, FREE.frame),
  };
}

/// Why the relay is closed to everyone, or null while it is open.
export async function disabledReason(kv: KVNamespace): Promise<string | null> {
  const text = await kv.get("disabled").catch(() => null);
  if (text === null || text === "" || text === "false") return null;
  return text === "true" ? "relay disabled" : text;
}

export async function isBlocked(kv: KVNamespace, label: string): Promise<boolean> {
  const list = await kv.get<unknown>("blocked", "json").catch(() => null);
  return Array.isArray(list) && list.includes(label);
}

function positive(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) && v > 0 ? Math.floor(v) : fallback;
}
