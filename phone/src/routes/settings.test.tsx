import { fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setActiveLink } from "../lib/api";
import { fakeLink, renderScreen } from "../test/screen";
import { Settings } from "./settings";

vi.mock("../lib/push", async (load) => ({ ...(await load<typeof import("../lib/push")>()), notifyState: () => "on" }));

beforeEach(() => {
  (globalThis as any).RELAY = { scoped: (key: string) => key };
});
afterEach(() => {
  delete (globalThis as any).RELAY;
});

describe("settings", () => {
  // The page runs in a desktop's browser too: it is a device, not a phone.
  it("calls the page's own end this device", async () => {
    await renderScreen(<Settings />, "/settings", fakeLink({ host: "MacBook Pro" }));
    expect(screen.getAllByText("This device").length).toBeGreaterThan(0);
    expect(screen.getByText("Unpair this device")).toBeTruthy();
    expect(screen.queryByText(/this phone/i)).toBeNull();
  });

  it("shows the defaults when the computer sent no numbers", async () => {
    await renderScreen(<Settings />, "/settings", fakeLink({ me: { notices: { done: true, input: true, error: true } } as never }));
    expect(screen.getByText("Only when I'm away for")).toBeTruthy();
    expect(screen.getByRole("button", { name: "2 min" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByText("Turns shorter than this stay quiet")).toBeTruthy();
    expect(screen.getByRole("button", { name: "30 s" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("shows what the computer chose, and posts only the number picked", async () => {
    const link = fakeLink({ me: { notices: { done: true, input: true, error: true, away_after_secs: 0, min_turn_secs: 60 } } as never });
    (link.fetch as ReturnType<typeof vi.fn>).mockResolvedValue(
      new Response(JSON.stringify({ notifying: true, notices: { done: true, input: true, error: true, away_after_secs: 300, min_turn_secs: 60 } })),
    );
    setActiveLink(link);
    await renderScreen(<Settings />, "/settings", link);
    expect(screen.getByRole("button", { name: "Always" }).getAttribute("aria-pressed")).toBe("true");
    expect(within(screen.getByRole("group", { name: "Turns shorter than this stay quiet" })).getByRole("button", { name: "1 min", pressed: true })).toBeTruthy();
    const away = screen.getByRole("group", { name: "Only when I'm away for" });
    fireEvent.click(within(away).getByRole("button", { name: "5 min" }));
    const [path, init] = (link.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(path).toBe("/push");
    expect(JSON.parse(init.body)).toEqual({ notices: { away_after_secs: 300 } });
    setActiveLink(null);
  });
});
