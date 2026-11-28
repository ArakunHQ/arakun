// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";
import type { MachineRow } from "../types";

const NOW = Math.floor(Date.now() / 1000);

function machine(label: string, account_id: number, login: string, name: string): MachineRow {
  return { label, account_id, github_id: null, login, name, kind: "app", created_at: NOW - 86400, last_seen_at: NOW - 60 };
}

const rows = [
  machine("lbl-aaa", 1002, "octocat", "Studio Mac"),
  machine("lbl-bbb", 1003, "", "Build box"),
  machine("lbl-ccc", 1004, "hubot", "Laptop"),
];

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

beforeAll(stubDialog);
beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url === "/api/me") return json({ email: "ops@example.com" });
      if (url === "/api/machines") return json(rows);
      return json({ error: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const names = () => screen.queryAllByRole("listitem").map((li) => li.getAttribute("aria-label"));

describe("Machines", () => {
  it("lists machines with a link to the user", async () => {
    renderRoute("/machines");
    const first = within(await screen.findByRole("listitem", { name: "lbl-aaa" }));
    expect(first.getByRole("link", { name: "octocat" }).getAttribute("href")).toBe("/users/1002");
    const unnamed = within(screen.getByRole("listitem", { name: "lbl-bbb" }));
    expect(unnamed.getByRole("link", { name: "Account 1003" }).getAttribute("href")).toBe("/users/1003");
  });

  it("filters machines by login, name and label", async () => {
    const { router } = renderRoute("/machines");
    await screen.findByRole("listitem", { name: "lbl-aaa" });
    const box = screen.getByRole("searchbox", { name: "Filter machines" });
    await userEvent.type(box, "HUB");
    await waitFor(() => expect(router.state.location.search).toEqual({ q: "HUB" }));
    expect(names()).toEqual(["lbl-ccc"]);
    await userEvent.clear(box);
    await userEvent.type(box, "build");
    await waitFor(() => expect(names()).toEqual(["lbl-bbb"]));
    await userEvent.clear(box);
    await userEvent.type(box, "lbl-aaa");
    await waitFor(() => expect(names()).toEqual(["lbl-aaa"]));
  });

  it("the box follows q when it changes from outside, and says when nothing matches", async () => {
    const { router } = renderRoute("/machines?q=zzz");
    expect(await screen.findByText("No machine matches.")).toBeTruthy();
    const box = screen.getByRole("searchbox") as HTMLInputElement;
    expect(box.value).toBe("zzz");
    await router.navigate({ to: "/machines", search: {} });
    await waitFor(() => expect(box.value).toBe(""));
    expect(names()).toHaveLength(3);
  });
});
