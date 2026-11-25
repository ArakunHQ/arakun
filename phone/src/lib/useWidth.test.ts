import { afterEach, describe, expect, it, vi } from "vitest";
import { currentWidth } from "./useWidth";

const wide = (px: number) =>
  vi.stubGlobal("matchMedia", (q: string) => ({ matches: px >= Number(/(\d+)px/.exec(q)?.[1]), addEventListener() {}, removeEventListener() {} }));

afterEach(() => vi.unstubAllGlobals());

describe("the width", () => {
  it("is a phone under 768 px, a tablet under 1200 px, a desktop above", () => {
    for (const [px, width] of [[390, "phone"], [767, "phone"], [768, "tablet"], [1199, "tablet"], [1200, "desktop"], [1440, "desktop"]] as const) {
      wide(px);
      expect(currentWidth()).toBe(width);
    }
  });
});
