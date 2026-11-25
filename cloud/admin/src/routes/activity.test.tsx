import { cleanup, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";
import type { LogEntry } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const calls: string[] = [];

function entry(id: number, over: Partial<LogEntry>): LogEntry {
  return {
    id,
    at: NOW - id * 60,
    operator_id: 1001,
    operator_login: "octo-operator",
    action: "flag.grant",
    github_id: 1002,
    login: "octocat",
    detail: null,
    ...over,
  };
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

beforeAll(stubDialog);
beforeEach(() => {
  calls.length = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      calls.push(url);
      if (url === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
      if (url === "/api/log?before=40") return json({ entries: [entry(39, { action: "unblock" })], next: null });
      if (url === "/api/log") {
        return json({
          entries: [
            entry(50, { detail: { flag: "cloud" } }),
            entry(45, { action: "cloud.decline", github_id: 1003, login: "hubot" }),
            entry(41, { operator_id: null, operator_login: null, action: "notice.create", github_id: null, login: null }),
          ],
          next: 40,
        });
      }
      return json({ error: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Activity", () => {
  it("shows the operator, the action in words and the account", async () => {
    renderRoute("/activity");
    const grant = within((await screen.findByText("granted the flag cloud")).closest("li")!);
    expect(grant.getByText("octo-operator")).toBeTruthy();
    expect(grant.getByRole("link", { name: "octocat" }).getAttribute("href")).toBe("/users/1002");
    const decline = within(screen.getByText("declined cloud access").closest("li")!);
    expect(decline.getByRole("link", { name: "hubot" }).getAttribute("href")).toBe("/users/1003");
    const secret = within(screen.getByText("sent a notice").closest("li")!);
    expect(secret.getByText("the secret")).toBeTruthy();
    expect(secret.getByText("everyone")).toBeTruthy();
  });

  it("Load more asks with before", async () => {
    renderRoute("/activity");
    await userEvent.click(await screen.findByRole("button", { name: "Load more" }));
    expect(await screen.findByText("unblocked the account")).toBeTruthy();
    expect(calls).toContain("/api/log?before=40");
    expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
  });
});
