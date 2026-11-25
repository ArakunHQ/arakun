import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
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
      github_id: 1002,
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
    notices: [{ id: "n1", github_id: 1002, login: "octocat", text: "Hello there", url: "https://example.com/x", created_at: NOW - 30, expires_at: null, reads: 2 }],
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
  if (one && method === "GET") return detail ? json(detail) : json({ error: "no such user" }, 404);
  if (/^\/api\/users\/\d+\/flags\/\w+$/.test(url)) return json({ github_id: 1002, flags: ["cloud"] });
  if (/^\/api\/users\/\d+\/sessions$/.test(url)) return json({ github_id: 1002, closed: 2 });
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
    expect(await screen.findByText("This is not a GitHub ID.")).toBeTruthy();
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
    await userEvent.click(within(screen.getByRole("dialog", { name: "Block GitHub ID 1002?" })).getByRole("button", { name: "Block" }));
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

  it("shows the forms for an account that never signed in", async () => {
    detail = null;
    renderRoute("/users/1005");
    expect(await screen.findByText("This account has not signed in yet.")).toBeTruthy();
    expect(screen.getByRole("textbox", { name: "New flag" })).toBeTruthy();
    expect(screen.getByRole("combobox", { name: "Plan" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Block" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Send notice" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Sign out everywhere" })).toBeNull();
    expect(screen.queryByText("Activity")).toBeNull();
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
    expect(bodies["POST /api/notices"]).toEqual({ github_id: 1002, text: "Welcome", url: "https://example.com/a" });
    expect(await screen.findByText("Notice sent.")).toBeTruthy();
  });

  it("lists the account's activity", async () => {
    log = [{ id: 7, at: NOW - 5, operator_id: null, operator_login: null, action: "flag.grant", github_id: 1002, login: "octocat", detail: { flag: "cloud" } }];
    renderRoute("/users/1002");
    expect(await screen.findByText("granted the flag cloud")).toBeTruthy();
    expect(calls).toContain("GET /api/log?github_id=1002");
  });
});
