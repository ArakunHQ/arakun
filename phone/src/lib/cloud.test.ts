import { describe, expect, it } from "vitest";
import { type Access, accessLine, canAsk, type CloudMachine, hours, machineLine, type Usage, usageLine } from "./cloud";

const access = (status: Access["status"], retry_at: number | null = null): Access => ({ status, requested_at: 1, decided_at: null, retry_at });
const machine = (over: Partial<CloudMachine> = {}): CloudMachine => ({
  label: "abcdefghijklmnop",
  name: "Cloud",
  running: false,
  snapshot: true,
  last_active: null,
  task_ms: 0,
  stopped: null,
  ...over,
});
const usage = (over: Partial<Usage> = {}): Usage => ({
  month: "2026-11",
  used_ms: 5.5 * 3_600_000,
  allowance_ms: 20 * 3_600_000,
  left_ms: 14.5 * 3_600_000,
  task_cap_ms: 2 * 3_600_000,
  stopped: null,
  message: null,
  ...over,
});

describe("cloud", () => {
  it("says times as the service does", () => {
    expect(hours(60_000)).toBe("1 minute");
    expect(hours(45 * 60_000)).toBe("45 minutes");
    expect(hours(3_600_000)).toBe("1 hour");
    expect(hours(14.54 * 3_600_000)).toBe("14.5 hours");
    expect(usageLine(usage())).toBe("14.5 hours of 20 hours left this month");
  });

  it("lets an account ask until it has, or after a decline has cooled down", () => {
    expect(canAsk(access("none"))).toBe(true);
    expect(canAsk(access("pending"))).toBe(false);
    expect(canAsk(access("granted"))).toBe(false);
    expect(canAsk(access("declined", 200), 100)).toBe(false);
    expect(canAsk(access("declined", 100), 100)).toBe(true);
    expect(accessLine(access("declined", 100 + 3 * 86_400 - 5), 100)).toContain("3 days");
    expect(accessLine(access("pending"))).toContain("by hand");
  });

  it("says why an environment stopped", () => {
    expect(machineLine(machine({ running: true }), null)).toBe("Awake");
    expect(machineLine(machine(), null)).toContain("Asleep");
    expect(machineLine(machine({ snapshot: false }), null)).toContain("Starting");
    expect(machineLine(machine({ stopped: "task_cap" }), usage())).toBe("Stopped after 2 hours, the most one task may run. Wake it to go on.");
    const used = usage({ stopped: "allowance_used", message: "This account used its 20 hours of cloud time for this month." });
    expect(machineLine(machine({ stopped: "allowance_used" }), used)).toBe(used.message);
    expect(machineLine(machine({ stopped: "held" }), null)).toContain("Stopped");
  });
});
