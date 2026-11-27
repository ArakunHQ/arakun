import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Access, CloudMachine, Usage } from "../lib/cloud";
import { CloudGroup } from "./CloudGroup";

let access: Access;
let machines: CloudMachine[];
let usage: Usage;
let sent: { url: string; method: string; body: unknown }[];
let answer: Response | null;

const cloud = (over: Partial<CloudMachine> = {}): CloudMachine => ({
  label: "abcdefghijklmnop",
  name: "Cloud",
  running: false,
  snapshot: true,
  last_active: null,
  task_ms: 0,
  stopped: null,
  ...over,
});

beforeEach(() => {
  sent = [];
  answer = null;
  access = { status: "none", requested_at: null, decided_at: null, retry_at: null };
  machines = [];
  usage = { month: "2026-11", used_ms: 0, allowance_ms: 20 * 3_600_000, left_ms: 20 * 3_600_000, task_cap_ms: 2 * 3_600_000, stopped: null, message: null };
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      if (method !== "GET") {
        sent.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
        if (answer) return answer;
        if (url === "/cloud/access") return Response.json({ ...access, status: "pending", requested_at: 2 }, { status: 201 });
        if (url === "/cloud/machines") return Response.json(cloud({ snapshot: false }), { status: 201 });
        return new Response(null, { status: 204 });
      }
      if (url === "/cloud/access") return Response.json(access);
      if (url === "/cloud/machines") return Response.json(machines);
      if (url === "/cloud/usage") return Response.json(usage);
      return new Response(null, { status: 404 });
    }),
  );
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const mount = (flagged: boolean) =>
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <CloudGroup flagged={flagged} />
    </QueryClientProvider>,
  );

describe("CloudGroup", () => {
  it("asks for access without the flag, with the reason typed", async () => {
    mount(false);
    const field = await screen.findByLabelText("What you would use it for");
    fireEvent.change(field, { target: { value: "  long refactors  " } });
    fireEvent.click(screen.getByText("Ask for access"));
    await screen.findByText(/You asked for access/);
    expect(sent).toEqual([{ url: "/cloud/access", method: "POST", body: { reason: "long refactors" } }]);
    expect(screen.queryByText("Ask for access")).toBeNull();
    expect(fetch).not.toHaveBeenCalledWith("/cloud/machines", expect.anything());
  });

  it("says when a declined account may ask again", async () => {
    access = { status: "declined", requested_at: 1, decided_at: 2, retry_at: Date.now() / 1000 + 86_400 - 5 };
    mount(false);
    await screen.findByText("Not this time. You can ask again in 1 day.");
    expect(screen.queryByText("Ask for access")).toBeNull();
  });

  it("makes a cloud environment and shows the hours left", async () => {
    mount(true);
    await screen.findByText("20 hours of 20 hours left this month");
    fireEvent.click(screen.getByText("Make one"));
    await screen.findByText(/Starting/);
    expect(sent).toEqual([{ url: "/cloud/machines", method: "POST", body: {} }]);
  });

  it("wakes an environment stopped at the task cap, and says why it stopped", async () => {
    machines = [cloud({ stopped: "task_cap" })];
    mount(true);
    await screen.findByText("Stopped after 2 hours, the most one task may run. Wake it to go on.");
    fireEvent.click(screen.getByText("Wake"));
    await waitFor(() => expect(sent).toEqual([{ url: "/cloud/machines/abcdefghijklmnop/wake", method: "POST", body: undefined }]));
  });

  it("offers no wake while the month's hours are used, and shows the service's reason", async () => {
    const message = "This account used its 20 hours of cloud time for this month.";
    machines = [cloud({ stopped: "allowance_used" })];
    usage = { ...usage, used_ms: usage.allowance_ms, left_ms: 0, stopped: "allowance_used", message };
    mount(true);
    expect((await screen.findAllByText(message)).length).toBeGreaterThan(0);
    expect(screen.queryByText("Wake")).toBeNull();
    expect(screen.getByText("0 minutes of 20 hours left this month")).toBeTruthy();
  });

  it("shows the service's reason when a stop fails", async () => {
    machines = [cloud({ running: true })];
    answer = Response.json({ error: "no such cloud machine" }, { status: 404 });
    mount(true);
    fireEvent.click(await screen.findByText("Stop"));
    await screen.findByText("no such cloud machine");
  });

  it("shows nothing where cloud environments are not set up", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ error: "cloud machines are not set up here" }, { status: 503 })));
    const { container } = mount(true);
    await new Promise((r) => setTimeout(r, 20));
    expect(container.textContent).toBe("");
  });
});
