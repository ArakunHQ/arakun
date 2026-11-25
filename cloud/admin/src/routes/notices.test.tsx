import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";
import type { NoticeRow } from "../types";

const NOW = Math.floor(Date.now() / 1000);
const calls: { method: string; url: string; body?: unknown }[] = [];

function notice(id: string, text: string, over: Partial<NoticeRow> = {}): NoticeRow {
  return { id, github_id: null, login: null, text, url: null, created_at: NOW - 3600, expires_at: null, reads: 0, ...over };
}

let rows: NoticeRow[] = [];

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

beforeAll(stubDialog);
beforeEach(() => {
  calls.length = 0;
  rows = [
    notice("n1", "Maintenance on Friday", { reads: 7, expires_at: NOW + 86400 }),
    notice("n2", "Your plan changed", { github_id: 1002, login: "octocat", url: "https://example.com/plan" }),
    notice("n3", "Old news", { github_id: 1003, login: "", url: "javascript:alert(1)", expires_at: NOW - 3600, reads: 2 }),
  ];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      if (url === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
      calls.push({ method, url, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url === "/api/notices" && method === "GET") return json(rows);
      if (url === "/api/notices" && method === "POST") return json({ id: "n9" }, 201);
      if (url === "/api/notices/n1" && method === "DELETE") {
        rows = rows.filter((n) => n.id !== "n1");
        return new Response(null, { status: 204 });
      }
      return json({ error: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const posts = () => calls.filter((c) => c.method === "POST");

describe("Notices", () => {
  it("lists notices with read counts", async () => {
    renderRoute("/notices");
    const all = within(await screen.findByRole("listitem", { name: "Maintenance on Friday" }));
    expect(all.getByText("Everyone")).toBeTruthy();
    expect(all.getByText(/read by 7/)).toBeTruthy();
    expect(all.queryByText("ended")).toBeNull();
    const one = within(screen.getByRole("listitem", { name: "Your plan changed" }));
    expect(one.getByRole("link", { name: "octocat" }).getAttribute("href")).toBe("/users/1002");
    const link = one.getByRole("link", { name: "https://example.com/plan" });
    expect(link.getAttribute("rel")).toBe("noreferrer");
    // A link that is not https is not a link; an ended notice stays listed.
    const old = within(screen.getByRole("listitem", { name: "Old news" }));
    expect(old.getByRole("link", { name: "GitHub ID 1003" }).getAttribute("href")).toBe("/users/1003");
    expect(old.getByText("ended")).toBeTruthy();
    expect(old.queryByText(/javascript/)).toBeNull();
  });

  it("asks before sending to everyone, not before sending to one account", async () => {
    renderRoute("/notices");
    await screen.findByRole("listitem", { name: "Old news" });
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Hello all");
    await userEvent.click(screen.getByRole("button", { name: "Send notice" }));
    expect(posts()).toEqual([]);
    const dialog = screen.getByRole("dialog", { name: "Send this notice to everyone?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Send to everyone" }));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0]?.body).toEqual({ text: "Hello all" });

    await userEvent.click(screen.getByRole("radio", { name: "One account" }));
    await userEvent.type(screen.getByRole("textbox", { name: "GitHub ID" }), "1002");
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Just you");
    await userEvent.click(screen.getByRole("button", { name: "Send notice" }));
    await waitFor(() => expect(posts()).toHaveLength(2));
    expect(posts()[1]?.body).toEqual({ github_id: 1002, text: "Just you" });
  });

  it("refuses a link that is not https before calling", async () => {
    renderRoute("/notices");
    await screen.findByRole("listitem", { name: "Old news" });
    await userEvent.type(screen.getByRole("textbox", { name: "Text" }), "Hi");
    await userEvent.type(screen.getByRole("textbox", { name: "Link (optional)" }), "http://example.com");
    expect((screen.getByRole("button", { name: "Send notice" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("The link must be an https:// address.")).toBeTruthy();
    expect(posts()).toEqual([]);
  });

  it("takes a notice back after a confirm", async () => {
    renderRoute("/notices");
    const row = within(await screen.findByRole("listitem", { name: "Maintenance on Friday" }));
    await userEvent.click(row.getByRole("button", { name: "Take back" }));
    const dialog = within(row.getByRole("dialog", { name: "Take back this notice?" }));
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    await userEvent.click(dialog.getByRole("button", { name: "Take back" }));
    await waitFor(() => expect(calls).toContainEqual({ method: "DELETE", url: "/api/notices/n1", body: undefined }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "Maintenance on Friday" })).toBeNull());
  });
});
