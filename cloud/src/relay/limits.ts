// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// What KV says about how much an account and its machines may do, and whether
// the relay is open at all. Reading KV instead of baking numbers in lets the
// limits move without a deploy. Reads are few: one or two per socket opened,
// one a minute per object for the plan. No edge cache on them, so a change
// lands at once. Which plan an account is on is `users.plan` in D1.
//
// `free` and `internal` are built in, so they work before KV names them; KV
// still overrides any of their fields.
//
// Keys: `plans` → `{ free: Plan, … }`; `disabled` → any text (shown to whoever
// asks why).

export interface Plan {
  /// Machines an account may register.
  machines: number;
  /// Phones connected at once.
  phones: number;
  /// Frames relayed per second.
  burst: number;
  /// Frames relayed per UTC day.
  daily: number;
  /// Bytes in one frame's payload.
  frame: number;
  /// Minutes the account's cloud machines may be awake in a UTC month
  /// (cloud_meter.ts). Minutes, not hours, so KV can give a part of an hour.
  cloudMinutes: number;
  /// Minutes one task may run: one stretch awake, from a wake to the sleep.
  cloudTaskMinutes: number;
}

/// A frame costs the relay 1/20 of a request, so a full day here is about
/// 5,000 requests: less than a phone left open used to cost in alarms. The
/// cloud hours are the #561 rehearsal's (20 a month, 2 a task) until the
/// allowance is decided.
export const FREE: Plan = { machines: 10, phones: 5, burst: 100, daily: 100_000, frame: 256 * 1024, cloudMinutes: 20 * 60, cloudTaskMinutes: 2 * 60 };

/// For the team's own accounts, put on it from the admin panel. High enough
/// not to get in the way, but still a ceiling on a runaway client.
export const INTERNAL: Plan = {
  machines: 50,
  phones: 10,
  burst: 500,
  daily: 1_000_000,
  frame: 1024 * 1024,
  cloudMinutes: 200 * 60,
  cloudTaskMinutes: 8 * 60,
};

/// The plans that exist without KV, by name.
export const BUILT_IN: Readonly<Record<string, Plan>> = { free: FREE, internal: INTERNAL };

/// The plan under a name: KV's fields over the built-in plan of that name.
/// A name that is neither in KV nor built in is the free plan.
export async function planFor(kv: KVNamespace, name = "free"): Promise<Plan> {
  const plans = await kv.get<Record<string, Partial<Plan>>>("plans", "json").catch(() => null);
  const kvPlans = plans !== null && typeof plans === "object" ? plans : {};
  // Own keys only: a name like `constructor` must not find Object's.
  const key = Object.hasOwn(BUILT_IN, name) || Object.hasOwn(kvPlans, name) ? name : "free";
  const base = Object.hasOwn(BUILT_IN, key) ? BUILT_IN[key]! : FREE;
  const given = (Object.hasOwn(kvPlans, key) ? kvPlans[key] : null) ?? {};
  return {
    machines: positive(given.machines, base.machines),
    phones: positive(given.phones, base.phones),
    burst: positive(given.burst, base.burst),
    daily: positive(given.daily, base.daily),
    frame: positive(given.frame, base.frame),
    cloudMinutes: positive(given.cloudMinutes, base.cloudMinutes),
    cloudTaskMinutes: positive(given.cloudTaskMinutes, base.cloudTaskMinutes),
  };
}

/// Why the relay is closed to everyone, or null while it is open.
export async function disabledReason(kv: KVNamespace): Promise<string | null> {
  const text = await kv.get("disabled").catch(() => null);
  if (text === null || text === "" || text === "false") return null;
  return text === "true" ? "relay disabled" : text;
}

function positive(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) && v > 0 ? Math.floor(v) : fallback;
}
