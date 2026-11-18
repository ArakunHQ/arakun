import { describe, expect, it } from "vitest";
import { replyClose } from "../src/relay";

function fake() {
  const calls: [number, string][] = [];
  return { calls, ws: { close: (code: number, reason: string) => void calls.push([code, reason]) } };
}

describe("replyClose", () => {
  it("echoes codes close() accepts", () => {
    for (const code of [1000, 3000, 4409, 4999]) {
      const f = fake();
      replyClose(f.ws, code, "why");
      expect(f.calls).toEqual([[code, "why"]]);
    }
  });

  it("answers codes that can't be sent with 1000", () => {
    for (const code of [1001, 1005, 1006, 1011, 1015, 2999, 5000]) {
      const f = fake();
      replyClose(f.ws, code, "");
      expect(f.calls).toEqual([[1000, ""]]);
    }
  });

  it("ignores a socket that is already closed", () => {
    expect(() => replyClose({ close: () => { throw new Error("closed"); } }, 1000, "")).not.toThrow();
  });
});
