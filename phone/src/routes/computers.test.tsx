import { QueryClient } from "@tanstack/react-query";
import { screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeLink, renderScreen } from "../test/screen";
import { Computers } from "./computers";

afterEach(() => {
  delete (globalThis as any).RELAY;
});

describe("computers", () => {
  it("says the account host is unreachable at once, without retrying", async () => {
    const boot = vi.fn(() => Promise.reject(new Error("down")));
    (globalThis as any).RELAY = { boot, unreachable: () => "Could not reach h.", sentence: () => "" };
    // The page's own client, with Query's default retries: meQuery turns them off.
    await renderScreen(<Computers />, "/computers", fakeLink(), undefined, new QueryClient());
    expect(await screen.findByText("Could not reach h.", undefined, { timeout: 500 })).toBeTruthy();
    expect(boot).toHaveBeenCalledTimes(1);
  });
});
