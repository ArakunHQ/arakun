import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";
import type { CloudRequest } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const DAY = 86400;

function request(over: Partial<CloudRequest> & { account_id: number; login: string }): CloudRequest {
  return {
    github_id: 583231,
    status: "pending",
    reason: "I want to build on a cloud machine",
    requested_at: NOW - 2 * DAY,
    decided_at: null,
    decided_by: null,
    retry_at: null,
    has_flag: false,
    blocked: false,
    github: {
      login: over.login,
      created_at: NOW - 400 * DAY,
      age_days: 400,
      public_repos: 12,
      followers: 34,
      following: 5,
    },
    ...over,
  };
}

let requests: CloudRequest[];
const calls: string[] = [];
let hold: Promise<void> | null;
let failGrant: string | null;
let failDecline: string | null;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = String(input);
  const method = init?.method ?? "GET";
  calls.push(`${method} ${url}`);
  if (url === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
  const list = /^\/api\/cloud\/requests\?status=(\w+)$/.exec(url);
  if (list) return json(requests.filter((r) => r.status === list[1]));
  const act = /^\/api\/cloud\/requests\/(\d+)\/(grant|decline)$/.exec(url);
  if (act && method === "PUT") {
    if (hold) await hold;
    const failure = act[2] === "grant" ? failGrant : failDecline;
    if (failure) return json({ error: failure }, 409);
    const r = requests.find((q) => q.account_id === Number(act[1]))!;
    r.status = act[2] === "grant" ? "granted" : "declined";
    return json(r);
  }
  return json({ error: "not found" }, 404);
});

beforeAll(stubDialog);

beforeEach(() => {
  calls.length = 0;
  hold = null;
  failGrant = null;
  failDecline = null;
  fetchMock.mockClear();
  vi.stubGlobal("fetch", fetchMock);
  requests = [
    request({ account_id: 1002, login: "octocat" }),
    request({
      account_id: 1003,
      login: "newcomer",
      reason: null,
      github: { login: "newcomer", created_at: null, age_days: null, public_repos: null, followers: null, following: null },
    }),
    request({
      account_id: 1004,
      login: "declined-one",
      status: "declined",
      decided_at: NOW - DAY,
      decided_by: "octo-operator",
      retry_at: NOW + 29 * DAY,
    }),
  ];
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function row(login: string): HTMLElement {
  return screen.getByRole("listitem", { name: login });
}

describe("Cloud access", () => {
  it("shows pending requests with the GitHub signals and the reason", async () => {
    renderRoute("/");
    const octocat = within(await screen.findByRole("listitem", { name: "octocat" }));
    const link = octocat.getByRole("link", { name: "octocat" });
    expect(link.getAttribute("href")).toBe("https://github.com/octocat");
    expect(link.getAttribute("rel")).toContain("noreferrer");
    expect(octocat.getByText("13 months")).toBeTruthy();
    expect(octocat.getByText("12")).toBeTruthy();
    expect(octocat.getByText("34")).toBeTruthy();
    expect(octocat.getByText("5")).toBeTruthy();
    expect(octocat.getByText("I want to build on a cloud machine")).toBeTruthy();
    expect(octocat.getByRole("link", { name: "Account" }).getAttribute("href")).toBe("/users/1002");
    expect(octocat.getByRole("button", { name: "Grant" })).toBeTruthy();
    expect(screen.queryByText("declined-one")).toBeNull();
  });

  it('shows "not known" for a signal GitHub did not give', async () => {
    renderRoute("/");
    const newcomer = within(await screen.findByRole("listitem", { name: "newcomer" }));
    expect(newcomer.getAllByText("not known")).toHaveLength(4);
    expect(newcomer.getByText("No reason given")).toBeTruthy();
  });

  it("grant calls PUT /api/cloud/requests/<id>/grant and the row leaves the list", async () => {
    renderRoute("/");
    await userEvent.click(within(await screen.findByRole("listitem", { name: "octocat" })).getByRole("button", { name: "Grant" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "octocat" })).toBeNull());
    expect(calls).toContain("PUT /api/cloud/requests/1002/grant");
    expect(screen.getByRole("listitem", { name: "newcomer" })).toBeTruthy();
  });

  it("the button is disabled while the call runs", async () => {
    let release!: () => void;
    hold = new Promise((resolve) => (release = resolve));
    renderRoute("/");
    const octocat = within(await screen.findByRole("listitem", { name: "octocat" }));
    await userEvent.click(octocat.getByRole("button", { name: "Grant" }));
    await waitFor(() => expect((octocat.getByRole("button", { name: "Grant" }) as HTMLButtonElement).disabled).toBe(true));
    expect((octocat.getByRole("button", { name: "Decline" }) as HTMLButtonElement).disabled).toBe(true);
    release();
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "octocat" })).toBeNull());
  });

  it("decline asks to confirm first, and Cancel calls nothing", async () => {
    renderRoute("/");
    const octocat = within(await screen.findByRole("listitem", { name: "octocat" }));
    await userEvent.click(octocat.getByRole("button", { name: "Decline" }));
    const dialog = within(screen.getByRole("dialog", { name: "Decline octocat?" }));
    await userEvent.click(dialog.getByRole("button", { name: "Cancel" }));
    expect(calls.some((c) => c.startsWith("PUT"))).toBe(false);

    await userEvent.click(octocat.getByRole("button", { name: "Decline" }));
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Decline" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "octocat" })).toBeNull());
    expect(calls).toContain("PUT /api/cloud/requests/1002/decline");
  });

  it("the Declined tab offers Grant and shows who declined and when", async () => {
    renderRoute("/?status=declined");
    const declined = within(await screen.findByRole("listitem", { name: "declined-one" }));
    expect(declined.getByText(/declined by octo-operator/)).toBeTruthy();
    expect(declined.getByText("1 day ago")).toBeTruthy();
    expect(declined.getByRole("button", { name: "Grant" })).toBeTruthy();
    expect(declined.queryByRole("button", { name: "Decline" })).toBeNull();
    expect(calls).toContain("GET /api/cloud/requests?status=declined");
  });

  it("Grant on a declined row calls PUT /api/cloud/requests/<id>/grant", async () => {
    renderRoute("/?status=declined");
    await userEvent.click(within(await screen.findByRole("listitem", { name: "declined-one" })).getByRole("button", { name: "Grant" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "declined-one" })).toBeNull());
    expect(calls).toContain("PUT /api/cloud/requests/1004/grant");
  });

  it("shows no GitHub link for a login that is empty or not valid", async () => {
    requests = [request({ account_id: 1007, login: "" }), request({ account_id: 1008, login: "a/b" })];
    renderRoute("/");
    const first = within(await screen.findByRole("listitem", { name: "" }));
    expect(first.getByText("Account 1007")).toBeTruthy();
    expect(first.queryByRole("link", { name: /GitHub ID/ })).toBeNull();
    const second = within(screen.getByRole("listitem", { name: "a/b" }));
    expect(second.getByText("Account 1008")).toBeTruthy();
    expect(document.querySelector('a[href^="https://github.com"]')).toBeNull();
  });

  it("a failed grant shows the server's text", async () => {
    failGrant = "the request is already granted";
    renderRoute("/");
    await userEvent.click(within(await screen.findByRole("listitem", { name: "octocat" })).getByRole("button", { name: "Grant" }));
    const alert = await within(row("octocat")).findByRole("alert");
    expect(alert.textContent).toBe("the request is already granted");
    expect(within(row("newcomer")).queryByRole("alert")).toBeNull();
  });

  it("shows the error of the call that failed last", async () => {
    failGrant = "the grant failed";
    failDecline = "the decline failed";
    renderRoute("/");
    const octocat = within(await screen.findByRole("listitem", { name: "octocat" }));
    await userEvent.click(octocat.getByRole("button", { name: "Grant" }));
    expect((await octocat.findByRole("alert")).textContent).toBe("the grant failed");
    await userEvent.click(octocat.getByRole("button", { name: "Decline" }));
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Decline" }));
    await waitFor(() => expect(octocat.getByRole("alert").textContent).toBe("the decline failed"));
    await userEvent.click(octocat.getByRole("button", { name: "Grant" }));
    await waitFor(() => expect(octocat.getByRole("alert").textContent).toBe("the grant failed"));
  });

  it("says so when nothing is pending", async () => {
    requests = [];
    renderRoute("/");
    expect(await screen.findByText("No requests are pending.")).toBeTruthy();
  });

  it("switches tabs through the URL", async () => {
    const { router } = renderRoute("/");
    await screen.findByRole("listitem", { name: "octocat" });
    await userEvent.click(screen.getByRole("link", { name: "Declined" }));
    await screen.findByRole("listitem", { name: "declined-one" });
    expect(router.state.location.search).toEqual({ status: "declined" });
  });
});
