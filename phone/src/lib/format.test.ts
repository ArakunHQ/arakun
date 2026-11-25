import { describe, expect, it } from "vitest";
import { ago, clock, counts, fmtElapsed, statusLabel } from "./format";

// Expected strings come from running the functions of the same name in
// public/app.js, which this file replaces.
describe("ago", () => {
  const now = 1_700_000_000;
  it("says now under a minute", () => {
    expect(ago(now - 30, now)).toBe("now");
    expect(ago(now - 59, now)).toBe("now");
    expect(ago(now + 5, now)).toBe("now");
  });
  it("counts minutes, hours and days", () => {
    expect(ago(now - 60, now)).toBe("1m");
    expect(ago(now - 120, now)).toBe("2m");
    expect(ago(now - 3599, now)).toBe("59m");
    expect(ago(now - 3600, now)).toBe("1h");
    expect(ago(now - 86399, now)).toBe("23h");
    expect(ago(now - 86400, now)).toBe("1d");
    expect(ago(now - 172800, now)).toBe("2d");
  });
  it("is empty for a missing time", () => {
    expect(ago(0, now)).toBe("");
  });
  it("defaults now to the clock", () => {
    expect(ago(Date.now() / 1000 - 5)).toBe("now");
  });
});

describe("clock", () => {
  it("shows hours and minutes", () => {
    expect(clock(1_700_000_000)).toMatch(/^\d{1,2}:\d{2}/);
  });
});

describe("fmtElapsed", () => {
  it("is at least a second", () => {
    expect(fmtElapsed(0)).toBe("1s");
    expect(fmtElapsed(400)).toBe("1s");
  });
  it("rounds seconds, then adds minutes", () => {
    expect(fmtElapsed(28_000)).toBe("28s");
    expect(fmtElapsed(59_499)).toBe("59s");
    expect(fmtElapsed(59_500)).toBe("1m");
    expect(fmtElapsed(65_000)).toBe("1m 5s");
    expect(fmtElapsed(252_000)).toBe("4m 12s");
    expect(fmtElapsed(3_600_000)).toBe("60m");
  });
});

describe("statusLabel", () => {
  it("names each status", () => {
    expect(statusLabel("working")).toBe("Working");
    expect(statusLabel("needs_input")).toBe("Needs you");
    expect(statusLabel("error")).toBe("Error");
    expect(statusLabel("idle")).toBe("Done");
  });
  it("passes an unknown status through", () => {
    expect(statusLabel("weird" as never)).toBe("weird");
  });
});

describe("counts", () => {
  it("is empty with nothing changed", () => {
    expect(counts(0, 0)).toBe("");
  });
  it("shows what is there", () => {
    expect(counts(3, 0)).toBe("+3");
    expect(counts(0, 2)).toBe("−2");
    expect(counts(3, 2)).toBe("+3 −2");
  });
});
