// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { userQuery } from "../queries";
import { renderRoute, stubDialog } from "../test-utils";
import type { LogEntry, UserDetail } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const calls: string[] = [];
const bodies: Record<string, unknown> = {};
let detail: UserDetail | null;
let failure: { match: RegExp; error: string } | null;
let log: LogEntry[];

function makeDetail(): UserDetail {
  return {
    user: {
      account_id: 1002,
      github_id: 583231,
      login: "octocat",
      plan: "free",
      blocked_at: null,
      blocked_reason: null,
      created_at: NOW - 86400,
      last_seen_at: NOW - 3600,
      machines: 1,
      flags: ["cloud"],
      email: "octo@example.com",
    },
    sessions: [{ id: "s1", name: "Laptop", kind: "app", created_at: NOW - 100, last_used_at: NOW - 50, expires_at: NOW + 1000, machine_label: null }],
    machines: [{ label: "m-1", name: "Studio", kind: "server", created_at: NOW - 100, last_seen_at: NOW - 10 }],
    cloud_request: null,
    notices: [{ id: "n1", account_id: 1002, login: "octocat", text: "Hello there", url: "https://example.com/x", created_at: NOW - 30, expires_at: null, reads: 2 }],
  };
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = String(input);
  const method = init?.method ?? "GET";
  calls.push(`${method} ${url}`);
  if (init?.body) bodies[`${method} ${url}`] = JSON.parse(String(init.body));
  if (url === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
  if (failure && method !== "GET" && failure.match.test(url)) return json({ error: failure.error }, 409);
  if (url === "/api/plans") return json({ plans: ["free", "pro"] });
  if (url.startsWith("/api/log")) return json({ entries: log, next: null });
  const one = /^\/api\/users\/(\d+)$/.exec(url);
  if (one && method === "GET") return detail && one[1] !== "1005" ? json(detail) : json({ error: "no such account" }, 404);
  if (url === "/api/github/1005" && method === "PUT") return json({ account_id: 1002, github_id: 1005, created: true });
  if (/^\/api\/users\/\d+\/flags\/\w+$/.test(url)) return json({ account_id: 1002, flags: ["cloud"] });
  if (/^\/api\/users\/\d+\/sessions$/.test(url)) return json({ account_id: 1002, closed: 2 });
  if (url === "/api/notices" && method === "POST") return json({ id: "n2" }, 201);
  if (method !== "GET") return new Response(null, { status: 204 });
  return json({ error: "not found" }, 404);
});

beforeAll(stubDialog);
beforeEach(() => {
  calls.length = 0;
  for (const k of Object.keys(bodies)) delete bodies[k];
  detail = makeDetail();
  failure = null;
  log = [];
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const writes = () => calls.filter((c) => !c.startsWith("GET"));

describe("one user", () => {
  it("shows the account in full", async () => {
    renderRoute("/users/1002");
    expect(await screen.findByRole("heading", { name: "octocat" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "octocat" }).getAttribute("href")).toBe("https://github.com/octocat");
    expect(screen.getByText("octo@example.com")).toBeTruthy();
    expect(screen.getByText("Laptop")).toBeTruthy();
    expect(screen.getByText("Studio")).toBeTruthy();
    expect(screen.getByText("Hello there")).toBeTruthy();
    const link = screen.getByRole("link", { name: "https://example.com/x" });
    expect(link.getAttribute("rel")).toContain("noreferrer");
  });

  it("calls nothing for an ID that is not one", async () => {
    renderRoute("/users/0123");
    expect(await screen.findByText("This is not an account ID.")).toBeTruthy();
    expect(calls.filter((c) => c.includes("/users/"))).toEqual([]);
  });

  it("adds a flag, and refuses \"Bad Flag\" without calling", async () => {
    renderRoute("/users/1002");
    const field = await screen.findByRole("textbox", { name: "New flag" });
    await userEvent.type(field, "Bad Flag");
    await userEvent.click(screen.getByRole("button", { name: "Add flag" }));
    expect(screen.getByText(/A flag starts with a lower case letter/)).toBeTruthy();
    expect(writes()).toEqual([]);

    await userEvent.clear(field);
    await userEvent.type(field, "beta_users");
    await userEvent.click(screen.getByRole("button", { name: "Add flag" }));
    await waitFor(() => expect(calls).toContain("PUT /api/users/1002/flags/beta_users"));
    await waitFor(() => expect((field as HTMLInputElement).value).toBe(""));
  });

  it("removing a flag, blocking and signing out ask to confirm", async () => {
    renderRoute("/users/1002");
    const chip = within(await screen.findByRole("listitem", { name: "cloud" }));
    await userEvent.click(chip.getByRole("button", { name: "Remove" }));
    await userEvent.click(within(screen.getByRole("dialog", { name: "Take away the flag cloud?" })).getByRole("button", { name: "Cancel" }));
    expect(writes()).toEqual([]);
    await userEvent.click(chip.getByRole("button", { name: "Remove" }));
    await userEvent.click(within(screen.getByRole("dialog", { name: "Take away the flag cloud?" })).getByRole("button", { name: "Take away" }));
    await waitFor(() => expect(calls).toContain("DELETE /api/users/1002/flags/cloud"));

    await userEvent.type(screen.getByRole("textbox", { name: "Reason" }), "spam");
    await userEvent.click(screen.getByRole("button", { name: "Block" }));
    expect(writes()).toEqual(["DELETE /api/users/1002/flags/cloud"]);
    const blockDialog = within(screen.getByRole("dialog", { name: "Block account 1002?" }));
    // A block ends the relay sockets, not the sign-in; the text must not say it does.
    expect(blockDialog.getByText(/disconnected now and cannot connect until it is unblocked\. It stays signed in: use Sign out everywhere/)).toBeTruthy();
    await userEvent.click(blockDialog.getByRole("button", { name: "Block" }));
    await waitFor(() => expect(calls).toContain("PUT /api/users/1002/block"));
    expect(bodies["PUT /api/users/1002/block"]).toEqual({ reason: "spam" });

    await userEvent.click(screen.getByRole("button", { name: "Sign out everywhere" }));
    expect(calls).not.toContain("DELETE /api/users/1002/sessions");
    await userEvent.click(within(screen.getByRole("dialog", { name: "Sign out everywhere?" })).getByRole("button", { name: "Sign out" }));
    await waitFor(() => expect(calls).toContain("DELETE /api/users/1002/sessions"));
    expect(await screen.findByText("Signed out 2 devices.")).toBeTruthy();
  });

  it("unblocks a blocked account", async () => {
    detail!.user.blocked_at = NOW - 60;
    detail!.user.blocked_reason = "spam";
    renderRoute("/users/1002");
    await userEvent.click(await screen.findByRole("button", { name: "Unblock" }));
    await waitFor(() => expect(calls).toContain("DELETE /api/users/1002/block"));
  });

  it("sets the plan", async () => {
    renderRoute("/users/1002");
    const select = await screen.findByRole("combobox", { name: "Plan" });
    await waitFor(() => expect(within(select).getByRole("option", { name: "pro" })).toBeTruthy());
    expect((screen.getByRole("button", { name: "Set plan" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.selectOptions(select, "pro");
    await userEvent.click(screen.getByRole("button", { name: "Set plan" }));
    await waitFor(() => expect(calls).toContain("PUT /api/users/1002/plan"));
    expect(bodies["PUT /api/users/1002/plan"]).toEqual({ plan: "pro" });
  });

  it("offers to set up the account of a GitHub ID no account has, and opens it", async () => {
    const { router } = renderRoute("/users/1005");
    expect(await screen.findByText("No account has this ID.")).toBeTruthy();
    // Nothing to set on an account that does not exist.
    expect(screen.queryByRole("textbox", { name: "New flag" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Block" })).toBeNull();
    expect(writes()).toEqual([]);
    await userEvent.click(screen.getByRole("button", { name: "Set up the account of GitHub ID 1005" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/users/1002"));
    expect(writes()).toEqual(["PUT /api/github/1005"]);
    expect(await screen.findByRole("textbox", { name: "New flag" })).toBeTruthy();
  });

  it("starts a fresh screen when the operator goes to another account", async () => {
    const { router, queryClient } = renderRoute("/users/1002");
    await queryClient.prefetchQuery(userQuery(1003));
    await userEvent.type(await screen.findByRole("textbox", { name: "Reason" }), "spam");
    await userEvent.type(screen.getByRole("textbox", { name: "New flag" }), "beta");
    // Account B is already cached, so nothing unmounts by waiting.
    await router.navigate({ to: "/users/$accountId", params: { accountId: "1003" } });
    await waitFor(() => expect((screen.getByRole("textbox", { name: "Reason" }) as HTMLInputElement).value).toBe(""));
    expect((screen.getByRole("textbox", { name: "New flag" }) as HTMLInputElement).value).toBe("");
  });

  it("shows the server's error under the form that failed", async () => {
    failure = { match: /flags/, error: "too many flags" };
    renderRoute("/users/1002");
    await userEvent.type(await screen.findByRole("textbox", { name: "New flag" }), "beta");
    await userEvent.click(screen.getByRole("button", { name: "Add flag" }));
    const flags = screen.getByRole("heading", { name: "Flags" }).closest("section")!;
    expect((await within(flags).findByRole("alert")).textContent).toBe("too many flags");
    const plan = screen.getByRole("heading", { name: "Plan" }).closest("section")!;
    expect(within(plan).queryByRole("alert")).toBeNull();
  });

  it("sends a notice for this account only", async () => {
    renderRoute("/users/1002");
    await userEvent.type(await screen.findByRole("textbox", { name: "Text" }), "Welcome");
    await userEvent.type(screen.getByRole("textbox", { name: "Link (optional)" }), "https://example.com/a");
    await userEvent.click(screen.getByRole("button", { name: "Send notice" }));
    await waitFor(() => expect(calls).toContain("POST /api/notices"));
    expect(bodies["POST /api/notices"]).toEqual({ account_id: 1002, text: "Welcome", url: "https://example.com/a" });
    expect(await screen.findByText("Notice sent.")).toBeTruthy();
  });

  it("lists the account's activity", async () => {
    log = [{ id: 7, at: NOW - 5, operator_id: null, operator_login: null, action: "flag.grant", account_id: 1002, login: "octocat", detail: { flag: "cloud" } }];
    renderRoute("/users/1002");
    expect(await screen.findByText("granted the flag cloud")).toBeTruthy();
    expect(calls).toContain("GET /api/log?account_id=1002");
  });
});
