import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ActivityList } from "./ActivityList";
import type { LogEntry } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const calls: string[] = [];
let empty = false;

/// The list alone, in a router only so its links have somewhere to point.
function renderList() {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <ActivityList /> }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

function entry(id: number, over: Partial<LogEntry>): LogEntry {
  return {
    id,
    at: NOW - id,
    operator_id: 1001,
    operator_login: "octo-operator",
    action: "unblock",
    github_id: 1002,
    login: "octocat",
    detail: null,
    ...over,
  };
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body));
}

const ALL: LogEntry[] = [
  entry(10, { action: "flag.grant", detail: { flag: "cloud" } }),
  entry(9, { action: "flag.revoke", detail: { flag: "cloud" } }),
  entry(8, { action: "sessions.close", detail: { closed: 3 } }),
  entry(7, { action: "block", detail: { reason: "spam" } }),
  entry(6, { action: "block", detail: { reason: null } }),
  entry(5, { action: "unblock" }),
  entry(4, { action: "plan.set", detail: { plan: "pro" } }),
  entry(3, { action: "notice.create", github_id: null, login: null, operator_login: null, operator_id: null }),
  entry(2, { action: "notice.delete" }),
  entry(1, { action: "cloud.grant" }),
];

beforeEach(() => {
  calls.length = 0;
  empty = false;
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      calls.push(url);
      if (url === "/api/log" && empty) return json({ entries: [], next: null });
      if (url === "/api/log") return json({ entries: ALL, next: 5 });
      if (url === "/api/log?before=5") {
        return json({
          entries: [entry(0, { action: "cloud.decline", github_id: 1003, login: "" }), entry(-1, { action: "future.thing" as LogEntry["action"] })],
          next: null,
        });
      }
      return json({ error: "not found" });
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ActivityList", () => {
  it("words every action, and names the secret and everyone", async () => {
    renderList();
    for (const text of [
      "granted the flag cloud",
      "took away the flag cloud",
      "signed out 3 devices",
      "blocked the account: spam",
      "blocked the account",
      "unblocked the account",
      "set the plan to pro",
      "sent a notice",
      "took back a notice",
      "granted cloud access",
    ]) {
      expect(await screen.findByText(text)).toBeTruthy();
    }
    expect(screen.getByText("the secret")).toBeTruthy();
    expect(screen.getByText("everyone")).toBeTruthy();
    expect(screen.getAllByRole("link", { name: "octocat" })[0]!.getAttribute("href")).toBe("/users/1002");

    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByText("declined cloud access")).toBeTruthy();
    expect(screen.getByText("future.thing")).toBeTruthy();
    expect(screen.getByText("GitHub ID 1003")).toBeTruthy();
    // The first page stays while the next one is added.
    expect(screen.getByText("granted the flag cloud")).toBeTruthy();
    await waitFor(() => expect(screen.queryByRole("button", { name: "Load more" })).toBeNull());
    expect(calls).toContain("/api/log?before=5");
  });

  it("says so when the log is empty", async () => {
    empty = true;
    renderList();
    expect(await screen.findByText("Nothing has been changed yet.")).toBeTruthy();
  });
});
