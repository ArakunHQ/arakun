// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The arithmetic of cloud machine metering: months, limits, and what the owner
// reads. The lifecycle that uses it is in cloud_machine.test.ts.
import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { addAwake, budgetOf, byMonth, limitOf, limitText, monthOf, msLeft, nextMonthStart, STOP_ALL_KEY, stoppedAll, type Budget } from "../src/cloud_meter";
import { CHECK_MS, nextCheck } from "../src/cloud_machine";
import { signIn } from "./helpers";

const HOUR = 3_600_000;

function budget(over: Partial<Budget> = {}): Budget {
  return { stoppedAll: null, held: null, month: "2026-11", allowanceMs: 20 * HOUR, usedMs: 0, taskCapMs: 2 * HOUR, ...over };
}

describe("months", () => {
  it("are UTC", () => {
    expect(monthOf(Date.UTC(2026, 10, 30, 23, 59))).toBe("2026-11");
    expect(monthOf(Date.UTC(2026, 11, 1))).toBe("2026-12");
    expect(nextMonthStart(Date.UTC(2026, 11, 31, 12))).toBe(Date.UTC(2027, 0, 1));
  });

  it("split time awake across midnight on the 1st", () => {
    const from = Date.UTC(2026, 10, 30, 23, 30);
    const to = Date.UTC(2026, 11, 1, 0, 15);
    expect(byMonth(from, to)).toEqual([
      ["2026-11", 30 * 60_000],
      ["2026-12", 15 * 60_000],
    ]);
    expect(byMonth(to, to)).toEqual([]);
    expect(byMonth(to, from)).toEqual([]);
  });
});

describe("limits", () => {
  it("come in order: the operator, then the allowance, then the task cap", () => {
    expect(limitOf(budget(), 0)).toBeNull();
    expect(limitOf(budget(), 2 * HOUR)).toBe("task_cap");
    expect(limitOf(budget({ usedMs: 20 * HOUR }), 2 * HOUR)).toBe("allowance_used");
    expect(limitOf(budget({ usedMs: 20 * HOUR, held: { reason: null } }), 0)).toBe("held");
    expect(limitOf(budget({ held: { reason: null }, stoppedAll: "" }), 0)).toBe("stopped_all");
  });

  it("leave the time to the nearer of the allowance and the cap", () => {
    expect(msLeft(budget(), 0)).toBe(2 * HOUR);
    expect(msLeft(budget({ usedMs: 19.5 * HOUR }), 0)).toBe(HOUR / 2);
    expect(msLeft(budget({ usedMs: 21 * HOUR }), 0)).toBe(0);
  });

  it("bring the alarm forward for a limit before the next check, but not to a spin", () => {
    expect(nextCheck(1000, 10 * HOUR)).toBe(1000 + CHECK_MS);
    expect(nextCheck(1000, 60_000)).toBe(1000 + 60_000);
    expect(nextCheck(1000, 0)).toBe(2000);
  });

  it("say why in a sentence", () => {
    expect(limitText("task_cap", budget())).toBe("The cloud environment stopped after 2 hours, the most one task may run. Wake it to go on.");
    expect(limitText("allowance_used", budget())).toBe(
      "This account used its 20 hours of cloud time for this month. Cloud environments can run again on 2026-12-01.",
    );
    expect(limitText("held", budget({ held: { reason: "abuse report 12" } }))).toBe("The operator stopped this account's cloud environments: abuse report 12");
    expect(limitText("stopped_all", budget({ stoppedAll: "" }))).toBe("Cloud environments are stopped for now.");
    expect(limitText("task_cap", budget({ taskCapMs: 90 * 60_000 }))).toMatch(/after 1\.5 hours,/);
    expect(limitText("task_cap", budget({ taskCapMs: 30 * 60_000 }))).toMatch(/after 30 minutes,/);
  });
});

describe("the switch for everyone", () => {
  it("reads KV: any text but false stops them", async () => {
    expect(await stoppedAll(env.CONFIG)).toBeNull();
    await env.CONFIG.put(STOP_ALL_KEY, "true");
    expect(await stoppedAll(env.CONFIG)).toBe("");
    await env.CONFIG.put(STOP_ALL_KEY, "a provider outage");
    expect(await stoppedAll(env.CONFIG)).toBe("a provider outage");
    await env.CONFIG.put(STOP_ALL_KEY, "false");
    expect(await stoppedAll(env.CONFIG)).toBeNull();
  });
});

describe("an account's budget", () => {
  it("adds up awake time per month, under the plan's hours from KV", async () => {
    const me = await signIn(71_000, "meter71000");
    const t = Date.UTC(2026, 10, 30, 23, 0);
    await addAwake(env, me.account_id, t, t + 30 * 60_000);
    await addAwake(env, me.account_id, t + 30 * 60_000, t + 90 * 60_000);
    const nov = await budgetOf(env, me.account_id, "free", t);
    expect(nov).toMatchObject({ month: "2026-11", usedMs: HOUR, allowanceMs: 20 * HOUR, taskCapMs: 2 * HOUR, held: null, stoppedAll: null });
    expect((await budgetOf(env, me.account_id, "free", t + 2 * HOUR)).usedMs).toBe(HOUR / 2);

    await env.CONFIG.put("plans", JSON.stringify({ free: { cloudMinutes: 90, cloudTaskMinutes: 30 } }));
    expect(await budgetOf(env, me.account_id, "free", t)).toMatchObject({ allowanceMs: 90 * 60_000, taskCapMs: 30 * 60_000 });
  });
});
