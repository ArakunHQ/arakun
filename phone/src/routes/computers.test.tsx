import { QueryClient } from "@tanstack/react-query";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeLink, renderScreen } from "../test/screen";
import { Computers } from "./computers";

afterEach(() => {
  delete (globalThis as any).RELAY;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
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

  it("deletes the cloud environment, not only its place on the account, when its row is removed", async () => {
    const machines = [
      { label: "m1", name: "MacBook Pro", pub: "A", online: true },
      { label: "abcdefghijklmnop", name: "Cloud", pub: "C", kind: "server", last_seen_at: 1 },
    ];
    const forgetMachine = vi.fn(async () => ({ ok: true }));
    (globalThis as any).RELAY = {
      boot: async () => ({ screen: "ok", me: { login: "me", flags: ["cloud"], notices: [] }, machines }),
      machines: async () => ({ list: machines }),
      forgetMachine,
      removeQuestion: (name: string) => `Remove ${name}?`,
      sentence: () => "",
    };
    const sent: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        const method = init?.method ?? "GET";
        if (method !== "GET") {
          sent.push(`${method} ${url}`);
          return new Response(null, { status: 204 });
        }
        if (url === "/cloud/machines") return Response.json([{ label: "abcdefghijklmnop", name: "Cloud", running: false, snapshot: true, last_active: null, task_ms: 0, stopped: null }]);
        if (url === "/cloud/usage") return Response.json({ month: "2026-11", used_ms: 0, allowance_ms: 1, left_ms: 1, task_cap_ms: 1, stopped: null, message: null });
        return new Response(null, { status: 404 });
      }),
    );
    const asked = vi.spyOn(window, "confirm").mockReturnValue(true);
    await renderScreen(<Computers />, "/computers", fakeLink());
    // The row in the list, then the one in the Cloud group: the first is the server's.
    await screen.findByText("Cloud environment");
    fireEvent.click(screen.getAllByLabelText("Remove Cloud")[0]!);
    await waitFor(() => expect(sent).toEqual(["DELETE /cloud/machines/abcdefghijklmnop"]));
    expect(asked).toHaveBeenCalledWith("Remove Cloud? Its files and sessions go with it.");
    expect(forgetMachine).not.toHaveBeenCalled();
  });
});
