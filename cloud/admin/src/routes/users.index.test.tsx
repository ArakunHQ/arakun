import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";
import type { UserRow } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const calls: string[] = [];

function user(github_id: number, login: string, over: Partial<UserRow> = {}): UserRow {
  return {
    github_id,
    login,
    plan: "free",
    blocked_at: null,
    blocked_reason: null,
    created_at: NOW - 86400,
    last_seen_at: NOW - 3600,
    machines: 1,
    flags: [],
    ...over,
  };
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = String(input);
  calls.push(`${init?.method ?? "GET"} ${url}`);
  if (url === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
  const params = new URL(url, "http://x").searchParams;
  if (url.startsWith("/api/users?") || url === "/api/users") {
    if (params.get("q") === "octo") return json({ users: [user(1002, "octocat")], next: null });
    if (params.get("after") === "100_1003") return json({ users: [user(1004, "second-page")], next: null });
    return json({
      users: [user(1002, "octocat", { flags: ["cloud"], plan: "pro" }), user(1003, "", { blocked_at: NOW - 60 })],
      next: "100_1003",
    });
  }
  return json({ error: "not found" }, 404);
});

beforeAll(stubDialog);
beforeEach(() => {
  calls.length = 0;
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Users", () => {
  it("lists accounts with plan, flags and a link to each", async () => {
    renderRoute("/users");
    const octocat = within(await screen.findByRole("listitem", { name: "octocat" }));
    expect(octocat.getByRole("link", { name: "octocat" }).getAttribute("href")).toBe("/users/1002");
    expect(octocat.getByText("cloud")).toBeTruthy();
    expect(octocat.getByText("pro")).toBeTruthy();
    const unnamed = within(screen.getByRole("listitem", { name: "GitHub ID 1003" }));
    expect(unnamed.getByText("blocked")).toBeTruthy();
  });

  it("search writes q to the URL and asks the server with it", async () => {
    const { router } = renderRoute("/users");
    await screen.findByRole("listitem", { name: "octocat" });
    await userEvent.type(screen.getByRole("searchbox", { name: "Search users" }), "octo");
    await waitFor(() => expect(router.state.location.search).toEqual({ q: "octo" }));
    expect(calls).toContain("GET /api/users?q=octo");
    // Typed in one go, so one search and not one per letter.
    expect(calls.filter((c) => c.startsWith("GET /api/users?q=")).length).toBe(1);
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "GitHub ID 1003" })).toBeNull());
  });

  it("Next follows the cursor", async () => {
    const { router } = renderRoute("/users");
    await userEvent.click(await screen.findByRole("link", { name: "Next" }));
    await screen.findByRole("listitem", { name: "second-page" });
    expect(router.state.location.search).toEqual({ after: "100_1003" });
    expect(calls).toContain("GET /api/users?after=100_1003");
    expect(screen.queryByRole("link", { name: "Next" })).toBeNull();
    await userEvent.click(screen.getByRole("link", { name: "First page" }));
    await screen.findByRole("listitem", { name: "octocat" });
    expect(router.state.location.search).toEqual({});
  });

  it("changing q drops the cursor", async () => {
    const { router } = renderRoute("/users?after=100_1003");
    await screen.findByRole("listitem", { name: "second-page" });
    await userEvent.type(screen.getByRole("searchbox"), "octo");
    await waitFor(() => expect(router.state.location.search).toEqual({ q: "octo" }));
  });

  it("the box follows q when it changes from outside", async () => {
    const { router } = renderRoute("/users?q=octo");
    const box = (await screen.findByRole("searchbox")) as HTMLInputElement;
    expect(box.value).toBe("octo");
    await router.navigate({ to: "/users", search: {} });
    await waitFor(() => expect(box.value).toBe(""));
    // It must not jump back to the old search after the pause.
    await new Promise((r) => setTimeout(r, 400));
    expect(router.state.location.search).toEqual({});
  });

  it("offers to open a numeric ID", async () => {
    renderRoute("/users?q=1099");
    const link = await screen.findByRole("link", { name: "Open account 1099" });
    expect(link.getAttribute("href")).toBe("/users/1099");
    cleanup();
    renderRoute("/users?q=octo");
    await screen.findByRole("listitem", { name: "octocat" });
    expect(screen.queryByRole("link", { name: /Open account/ })).toBeNull();
  });

  it("says so when nothing matches", async () => {
    fetchMock.mockImplementationOnce(async () => json({ github_id: 1001, login: "octo-operator" }));
    fetchMock.mockImplementationOnce(async () => json({ users: [], next: null }));
    renderRoute("/users?q=zzz");
    expect(await screen.findByText("No account matches.")).toBeTruthy();
  });
});
