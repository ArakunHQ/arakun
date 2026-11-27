import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EmailSignIn } from "./signin";

const calls: { url: string; body: unknown }[] = [];
let answers: Response[] = [];
const replace = vi.fn();

beforeEach(() => {
  calls.length = 0;
  answers = [];
  vi.stubGlobal("fetch", async (url: string, init: RequestInit) => {
    calls.push({ url, body: JSON.parse(String(init.body)) });
    return answers.shift() ?? new Response(null, { status: 500 });
  });
  vi.stubGlobal("location", { ...window.location, host: "app.arakun.dev", replace });
  replace.mockClear();
});
afterEach(() => vi.unstubAllGlobals());

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

async function askFor(address: string) {
  fireEvent.change(screen.getByRole("textbox", { name: "Email address" }), { target: { value: address } });
  fireEvent.click(screen.getByRole("button", { name: "Email me a code" }));
  await screen.findByText(/We sent a code to/);
}

describe("email sign-in on the page", () => {
  it("asks for a code, then signs in with it and starts the page again", async () => {
    answers = [json({ sent: true, expires_in: 600 }), new Response(null, { status: 204 })];
    render(<EmailSignIn />);
    await askFor(" ada@example.com ");
    expect(calls[0]).toEqual({ url: "/auth/email", body: { email: "ada@example.com" } });
    expect(screen.getByText("ada@example.com")).toBeTruthy();

    const signIn = screen.getByRole("button", { name: "Sign in" }) as HTMLButtonElement;
    expect(signIn.disabled).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Code" }), { target: { value: "123 456" } });
    expect(signIn.disabled).toBe(false);
    fireEvent.click(signIn);
    await waitFor(() => expect(replace).toHaveBeenCalledWith("/"));
    expect(calls[1]).toEqual({ url: "/auth/email/web", body: { email: "ada@example.com", code: "123 456" } });
  });

  it("shows what the account host said went wrong, and stays", async () => {
    answers = [json({ error: "that address is from a throwaway mail service; use one you keep" }, 400)];
    render(<EmailSignIn />);
    fireEvent.change(screen.getByRole("textbox", { name: "Email address" }), { target: { value: "x@mailinator.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Email me a code" }));
    expect(await screen.findByText("That address is from a throwaway mail service; use one you keep.")).toBeTruthy();
    expect(screen.queryByText(/We sent a code/)).toBeNull();
  });

  it("says a wrong code is wrong, sends a new one, and goes back to the address", async () => {
    answers = [json({ sent: true }), json({ error: "the code is wrong or has run out; ask for a new one" }, 401), json({ sent: true })];
    render(<EmailSignIn />);
    await askFor("ada@example.com");
    fireEvent.change(screen.getByRole("textbox", { name: "Code" }), { target: { value: "000000" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText("The code is wrong or has run out; ask for a new one.")).toBeTruthy();
    expect(replace).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Send a new code" }));
    await waitFor(() => expect(calls).toHaveLength(3));
    expect(calls[2]).toEqual({ url: "/auth/email", body: { email: "ada@example.com" } });
    await waitFor(() => expect(screen.queryByText(/The code is wrong/)).toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "Use another address" }));
    expect(screen.getByRole("textbox", { name: "Email address" })).toBeTruthy();
  });

  it("says when the account host cannot be reached", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new TypeError("offline");
    });
    render(<EmailSignIn />);
    fireEvent.change(screen.getByRole("textbox", { name: "Email address" }), { target: { value: "ada@example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Email me a code" }));
    expect(await screen.findByText("Could not reach app.arakun.dev. Try again.")).toBeTruthy();
  });
});
