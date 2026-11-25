import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "@testing-library/react";
import { fakeLink, renderScreen } from "../test/screen";
import { Usage } from "./usage";

const asks = (link: { sent: unknown[] }) => link.sent.filter((m) => (m as { type: string }).type === "usage").length;

describe("usage", () => {
  // Only the intervals are fake, so the router still loads on real timeouts.
  afterEach(() => vi.useRealTimers());

  it("asks again every minute while shown and connected", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    const link = fakeLink();
    const { unmount } = await renderScreen(<Usage />, "/usage", link);
    expect(asks(link)).toBe(1);
    act(() => vi.advanceTimersByTime(60_000));
    expect(asks(link)).toBe(2);

    link.set({ connected: false });
    act(() => vi.advanceTimersByTime(120_000));
    expect(asks(link)).toBe(2);

    link.set({ connected: true });
    expect(asks(link)).toBe(3);
    act(() => vi.advanceTimersByTime(60_000));
    expect(asks(link)).toBe(4);

    unmount();
    act(() => vi.advanceTimersByTime(120_000));
    expect(asks(link)).toBe(4);
  });
});
