import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as start from "../lib/start";
import { HIDE_BETA, type Signups } from "../lib/beta";
import { BetaCard } from "./BetaSignup";

const none = { status: "none", email: null, retry_at: null } as const;
let signups: Signups;
let posted: { url: string; body: unknown }[];
let flags: string[];

beforeEach(() => {
  localStorage.clear();
  posted = [];
  flags = ["mobile_beta"];
  signups = { email: "me@github.com", ios: { ...none }, android: { ...none } };
  vi.spyOn(start, "mode").mockReturnValue("account");
  vi.stubGlobal("navigator", { ...navigator, userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", maxTouchPoints: 5 });
  (globalThis as Record<string, unknown>).RELAY = { boot: async () => ({ screen: "list", me: { login: "me", flags } }) };
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === "POST") {
        const body = JSON.parse(String(init.body));
        posted.push({ url, body });
        return Response.json({ status: "pending", email: body.email, retry_at: null }, { status: 201 });
      }
      return Response.json(signups);
    }),
  );
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  delete (globalThis as Record<string, unknown>).RELAY;
});

const mount = () =>
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <BetaCard />
    </QueryClientProvider>,
  );

describe("BetaCard", () => {
  it("shows only to accounts with the flag", async () => {
    flags = [];
    mount();
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByText("Try the Arakun app")).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("signs this phone's store up with the email typed, for the operator to admit", async () => {
    mount();
    const field = await screen.findByLabelText("Email for the invite");
    expect((field as HTMLInputElement).value).toBe("me@github.com");
    expect(screen.getByText(/Apple ID/)).toBeTruthy();
    fireEvent.change(field, { target: { value: "me@icloud.com" } });
    fireEvent.click(screen.getByText("Join the beta"));
    await waitFor(() => expect(posted).toEqual([{ url: "/beta/ios", body: { email: "me@icloud.com" } }]));
    // Signed up: the card has nothing more to ask.
    await waitFor(() => expect(screen.queryByLabelText("Email for the invite")).toBeNull());
  });

  it("stays away once closed, and when already signed up", async () => {
    const { unmount } = mount();
    fireEvent.click(await screen.findByLabelText("Not now"));
    expect(localStorage.getItem(HIDE_BETA)).toBe("1");
    expect(screen.queryByText("Try the Arakun app")).toBeNull();
    unmount();

    localStorage.clear();
    signups.ios = { status: "pending", email: "me@icloud.com", retry_at: null };
    mount();
    await waitFor(() => expect(fetch).toHaveBeenCalled());
    expect(screen.queryByText("Try the Arakun app")).toBeNull();
  });
});
