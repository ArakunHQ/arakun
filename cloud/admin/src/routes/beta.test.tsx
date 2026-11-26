import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";
import type { BetaSignup } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const DAY = 86400;

function signup(over: Partial<BetaSignup> & { account_id: number; login: string }): BetaSignup {
  return {
    github_id: 583231,
    platform: "ios",
    email: `${over.login}@example.com`,
    status: "pending",
    requested_at: NOW - 2 * DAY,
    decided_at: null,
    decided_by: null,
    retry_at: null,
    blocked: false,
    github: { login: over.login, created_at: NOW - 400 * DAY, age_days: 400, public_repos: 12, followers: 34, following: 5 },
    ...over,
  };
}

let signups: BetaSignup[];
const calls: string[] = [];
let fail: string | null;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = new URL(String(input), "https://admin.test");
  const method = init?.method ?? "GET";
  calls.push(`${method} ${url.pathname}${url.search}`);
  if (url.pathname === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
  if (url.pathname === "/api/beta/signups") {
    const status = url.searchParams.get("status");
    const platform = url.searchParams.get("platform");
    return json(signups.filter((s) => s.status === status && (platform === null || s.platform === platform)));
  }
  const act = /^\/api\/beta\/signups\/(\d+)\/(ios|android)\/(admit|decline)$/.exec(url.pathname);
  if (act && method === "PUT") {
    if (fail) return json({ error: fail }, 404);
    const s = signups.find((q) => q.account_id === Number(act[1]) && q.platform === act[2])!;
    s.status = act[3] === "admit" ? "admitted" : "declined";
    return json(s);
  }
  return json({ error: "not found" }, 404);
});

beforeAll(stubDialog);

beforeEach(() => {
  calls.length = 0;
  fail = null;
  fetchMock.mockClear();
  vi.stubGlobal("fetch", fetchMock);
  signups = [
    signup({ account_id: 1002, login: "octocat" }),
    signup({ account_id: 1002, login: "octocat", platform: "android", email: "octocat@gmail.com" }),
    signup({ account_id: 1003, login: "inside", status: "admitted", decided_at: NOW - DAY, decided_by: "octo-operator" }),
  ];
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Phone betas", () => {
  it("lists pending sign-ups for both platforms with the email to invite", async () => {
    renderRoute("/beta");
    const ios = within(await screen.findByRole("listitem", { name: "octocat iOS" }));
    expect(ios.getByText("octocat@example.com", { selector: "p" })).toBeTruthy();
    expect(ios.getByText("13 months")).toBeTruthy();
    expect(ios.getByRole("link", { name: "Account" }).getAttribute("href")).toBe("/users/1002");
    const android = within(screen.getByRole("listitem", { name: "octocat Android" }));
    expect(android.getByText("octocat@gmail.com", { selector: "p" })).toBeTruthy();
    expect(screen.queryByRole("listitem", { name: "inside iOS" })).toBeNull();
    expect(calls).toContain("GET /api/beta/signups?status=pending");
    // Copying is offered for one platform at a time: one store's list.
    expect(screen.queryByRole("button", { name: /^Copy/ })).toBeNull();
  });

  it("admits after the operator confirms they added the email in the store", async () => {
    renderRoute("/beta");
    const ios = within(await screen.findByRole("listitem", { name: "octocat iOS" }));
    await userEvent.click(ios.getByRole("button", { name: "Admit" }));
    const dialog = within(screen.getByRole("dialog", { name: "Admit octocat to the iOS beta?" }));
    expect(dialog.getByText(/App Store Connect/)).toBeTruthy();
    await userEvent.click(dialog.getByRole("button", { name: "Cancel" }));
    expect(calls.some((c) => c.startsWith("PUT"))).toBe(false);

    await userEvent.click(ios.getByRole("button", { name: "Admit" }));
    await userEvent.click(within(screen.getByRole("dialog", { name: /Admit octocat/ })).getByRole("button", { name: "Admit" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "octocat iOS" })).toBeNull());
    expect(calls).toContain("PUT /api/beta/signups/1002/ios/admit");
    expect(screen.getByRole("listitem", { name: "octocat Android" })).toBeTruthy();
  });

  it("declines a pending sign-up", async () => {
    renderRoute("/beta");
    const android = within(await screen.findByRole("listitem", { name: "octocat Android" }));
    await userEvent.click(android.getByRole("button", { name: "Decline" }));
    await userEvent.click(within(screen.getByRole("dialog", { name: "Decline octocat?" })).getByRole("button", { name: "Decline" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "octocat Android" })).toBeNull());
    expect(calls).toContain("PUT /api/beta/signups/1002/android/decline");
  });

  it("removes an admitted tester and says to remove them in the store too", async () => {
    renderRoute("/beta?status=admitted");
    const inside = within(await screen.findByRole("listitem", { name: "inside iOS" }));
    expect(inside.getByText(/admitted by octo-operator/)).toBeTruthy();
    expect(inside.queryByRole("button", { name: "Admit" })).toBeNull();
    await userEvent.click(inside.getByRole("button", { name: "Remove" }));
    const dialog = within(screen.getByRole("dialog", { name: "Remove inside from the iOS beta?" }));
    expect(dialog.getByText(/from the testers in App Store Connect/)).toBeTruthy();
    await userEvent.click(dialog.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "inside iOS" })).toBeNull());
    expect(calls).toContain("PUT /api/beta/signups/1003/ios/decline");
  });

  it("filters by platform through the URL, keeping the status, and copies that list's emails", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const { router } = renderRoute("/beta");
    await screen.findByRole("listitem", { name: "octocat iOS" });
    await userEvent.click(screen.getByRole("link", { name: "Android" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "octocat iOS" })).toBeNull());
    expect(router.state.location.search).toEqual({ platform: "android" });
    expect(calls).toContain("GET /api/beta/signups?platform=android&status=pending");

    await userEvent.click(screen.getByRole("button", { name: "Copy 1 email" }));
    expect(writeText).toHaveBeenCalledWith("octocat@gmail.com");
    expect(await screen.findByText("Copied")).toBeTruthy();

    await userEvent.click(screen.getByRole("link", { name: "Admitted" }));
    await waitFor(() => expect(router.state.location.search).toEqual({ status: "admitted", platform: "android" }));
  });

  it("shows the server's text when a decision fails", async () => {
    fail = "no sign-up from this account for this platform";
    renderRoute("/beta");
    const ios = within(await screen.findByRole("listitem", { name: "octocat iOS" }));
    await userEvent.click(ios.getByRole("button", { name: "Admit" }));
    await userEvent.click(within(screen.getByRole("dialog", { name: /Admit octocat/ })).getByRole("button", { name: "Admit" }));
    expect((await ios.findByRole("alert")).textContent).toBe("no sign-up from this account for this platform");
  });

  it("says so when nothing is pending", async () => {
    signups = [];
    renderRoute("/beta");
    expect(await screen.findByText("No sign-ups are pending.")).toBeTruthy();
  });
});
