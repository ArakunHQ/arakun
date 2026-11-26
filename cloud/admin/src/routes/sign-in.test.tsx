import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderRoute, stubDialog } from "../test-utils";

let answer: () => Response;
const sent: { url: string; body: unknown }[] = [];
const assign = vi.fn();

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
  const url = String(input);
  if (url === "/api/me") return json({ error: "not signed in" }, 401);
  sent.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
  return answer();
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
    renderRoute("/?sign_in=no_key");
    expect((await screen.findByRole("alert")).textContent).toContain("no authenticator app set up");
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
});
