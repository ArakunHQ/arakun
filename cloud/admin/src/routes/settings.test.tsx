// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";

const KEY = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
let finish: () => Response;
const sent: { url: string; body: unknown }[] = [];

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = String(input);
  if (url === "/api/me") return json({ github_id: 1001, login: "octo-operator" });
  sent.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
  if (url === "/auth/setup/start") return json({ key: KEY, uri: `otpauth://totp/x?secret=${KEY}` });
  if (url === "/auth/setup") return finish();
  return json({ error: "not found" }, 404);
});

beforeAll(stubDialog);

beforeEach(() => {
  sent.length = 0;
  finish = () => new Response(null, { status: 204 });
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

async function openSetup() {
  renderRoute("/settings");
  await userEvent.click(await screen.findByRole("button", { name: "Replace authenticator app" }));
  await screen.findByRole("img", { name: "QR code for your authenticator app" });
}

describe("Settings", () => {
  it("replaces the authenticator app with codes from the current and the new one", async () => {
    await openSetup();
    const button = screen.getByRole("button", { name: "Replace authenticator app" });
    await userEvent.type(screen.getByRole("textbox", { name: "Code from the new app" }), "654321");
    // Not without the current app's code.
    expect((button as HTMLButtonElement).disabled).toBe(true);
    await userEvent.type(screen.getByRole("textbox", { name: "Code from your current app" }), "123456");
    await userEvent.click(button);
    expect((await screen.findByRole("status")).textContent).toContain("The old one no longer works");
    expect(sent).toEqual([
      { url: "/auth/setup/start", body: null },
      { url: "/auth/setup", body: { code: "654321", current_code: "123456" } },
    ]);
  });

  it("says when the current app's code is wrong, and keeps the key", async () => {
    finish = () => json({ error: "wrong current code" }, 401);
    await openSetup();
    await userEvent.type(screen.getByRole("textbox", { name: "Code from your current app" }), "000000");
    await userEvent.type(screen.getByRole("textbox", { name: "Code from the new app" }), "654321");
    await userEvent.click(screen.getByRole("button", { name: "Replace authenticator app" }));
    expect((await screen.findByRole("alert")).textContent).toContain("current app is not right");
    expect(screen.getByRole("img", { name: "QR code for your authenticator app" })).toBeTruthy();
    expect(sent.filter((s) => s.url === "/auth/setup/start")).toHaveLength(1);
  });
});
