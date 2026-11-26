import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";

let answer: (url: string) => Response;
const sent: { url: string; body: unknown }[] = [];
const assign = vi.fn();

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = String(input);
  if (url === "/api/me") return json({ error: "not signed in" }, 401);
  sent.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
  return answer(url);
});

beforeAll(stubDialog);

beforeEach(() => {
  sent.length = 0;
  answer = () => new Response(null, { status: 204 });
  assign.mockClear();
  vi.stubGlobal("fetch", fetchMock);
  vi.stubGlobal("location", { ...window.location, assign });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Sign in", () => {
  it("says why GitHub's sign-in ended without the code step", async () => {
    renderRoute("/?sign_in=reset");
    expect((await screen.findByRole("alert")).textContent).toContain("Ask a maintainer to reset it");
    expect(screen.getByRole("link", { name: "Sign in with GitHub" }).getAttribute("href")).toBe("/auth/github/start");
  });

  it("sends the code and loads the panel when it is right", async () => {
    renderRoute("/sign-in?step=code");
    const input = await screen.findByRole("textbox", { name: "Code" });
    const button = screen.getByRole("button", { name: "Continue" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    await userEvent.type(input, "123 456");
    await userEvent.click(button);
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/"));
    expect(sent).toEqual([{ url: "/auth/code", body: { code: "123 456" } }]);
  });

  it("asks again after a wrong code", async () => {
    answer = () => json({ error: "wrong code" }, 401);
    renderRoute("/sign-in?step=code");
    await userEvent.type(await screen.findByRole("textbox", { name: "Code" }), "000000");
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect((await screen.findByRole("alert")).textContent).toContain("That code is not right");
    expect((screen.getByRole("textbox", { name: "Code" }) as HTMLInputElement).value).toBe("");
    expect(assign).not.toHaveBeenCalled();
  });

  it("goes back to GitHub when the code step expired", async () => {
    answer = () => json({ error: "sign-in expired" }, 401);
    renderRoute("/sign-in?step=code");
    await userEvent.type(await screen.findByRole("textbox", { name: "Code" }), "000000");
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect((await screen.findByRole("alert")).textContent).toContain("took too long");
    expect(screen.queryByRole("textbox", { name: "Code" })).toBeNull();
    expect(screen.getByRole("link", { name: "Sign in with GitHub" })).toBeTruthy();
  });

  it("goes to the setup when the key was reset since GitHub's answer", async () => {
    answer = (url) => (url === "/auth/code" ? json({ error: "no key" }, 409) : json({ key: "A".repeat(32), uri: "otpauth://totp/x?secret=A" }));
    renderRoute("/sign-in?step=code");
    await userEvent.type(await screen.findByRole("textbox", { name: "Code" }), "000000");
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("heading", { name: "Set up your authenticator app" })).toBeTruthy();
  });
});

describe("Setup at the first sign-in", () => {
  const KEY = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
  const URI = `otpauth://totp/Arakun%20admin%3Atwo?secret=${KEY}&issuer=Arakun%20admin`;

  it("shows the key as a QR code and text, then stores it with the app's code", async () => {
    answer = (url) => (url === "/auth/setup/start" ? json({ key: KEY, uri: URI }) : new Response(null, { status: 204 }));
    renderRoute("/sign-in?step=setup");
    expect(await screen.findByRole("img", { name: "QR code for your authenticator app" })).toBeTruthy();
    expect(screen.getByText("GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Open in app" }).getAttribute("href")).toBe(URI);
    // No field for a current code: there is none yet.
    expect(screen.queryByRole("textbox", { name: "Code from your current app" })).toBeNull();
    await userEvent.type(screen.getByRole("textbox", { name: "Code from the app" }), "123456");
    await userEvent.click(screen.getByRole("button", { name: "Finish setup" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/"));
    expect(sent).toEqual([
      { url: "/auth/setup/start", body: null },
      { url: "/auth/setup", body: { code: "123456" } },
    ]);
  });

  it("asks for the code instead when a key exists", async () => {
    answer = () => json({ error: "a key exists" }, 409);
    renderRoute("/sign-in?step=setup");
    expect(await screen.findByRole("heading", { name: "Enter your code" })).toBeTruthy();
  });

  it("goes back to GitHub when the sign-in expired", async () => {
    answer = () => json({ error: "sign-in expired" }, 401);
    renderRoute("/sign-in?step=setup");
    expect(await screen.findByRole("link", { name: "Sign in with GitHub" })).toBeTruthy();
  });
});
