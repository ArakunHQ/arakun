import { act, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setActiveLink } from "../lib/api";
import type { Session } from "../lib/model";
import { fakeLink, renderScreen } from "../test/screen";
import { SessionScreen } from "./session";

// Through the relay: the computer's own picture links are fetched apart.
vi.mock("../lib/start", async (load) => ({ ...(await load<typeof import("../lib/start")>()), mode: () => "account" }));

const session = (over: Partial<Session> = {}): Session => ({
  id: "a",
  title: "t",
  agent: "claude",
  status: "idle",
  count: 1,
  from: 0,
  entries: [{ type: "agent", text: "look ![shot](/image/x.png)" }],
  permission: null,
  question: null,
  place: "p",
  ...over,
});

const tick = () => act(() => new Promise((r) => setTimeout(r, 0)));

beforeEach(() => {
  (globalThis as any).RELAY = { scoped: (key: string) => key };
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: vi.fn(() => "blob:pic1"), revokeObjectURL: vi.fn() }));
});
afterEach(() => {
  setActiveLink(null);
  delete (globalThis as any).RELAY;
  vi.unstubAllGlobals();
});

describe("pictures through the relay", () => {
  it("shows a picture once its blob arrives, in the same row, fetched once", async () => {
    const link = fakeLink({ session: session() });
    let answer!: (r: Response) => void;
    (link.fetch as ReturnType<typeof vi.fn>).mockReturnValue(new Promise<Response>((r) => (answer = r)));
    setActiveLink(link);
    const { container, unmount } = await renderScreen(<SessionScreen />, "/s/a", link);
    await tick();
    const row = container.querySelector('[data-abs="0"]');
    const img = () => container.querySelector(".entry.agent img.pic") as HTMLImageElement;
    expect(img().hasAttribute("src")).toBe(false);
    // A tap while it is on its way waits for the same fetch.
    act(() => img().click());
    await act(async () => answer({ ok: true, blob: async () => new Blob(["x"]) } as Response));
    await tick();
    expect(img().getAttribute("src")).toBe("blob:pic1");
    expect(container.querySelector('[data-abs="0"]')).toBe(row);
    expect(link.fetch).toHaveBeenCalledTimes(1);
    expect(link.fetch).toHaveBeenCalledWith("/image/x.png", undefined);
    expect(screen.getAllByAltText("shot")).toHaveLength(2);
    expect(document.querySelector("#picview img")?.getAttribute("src")).toBe("blob:pic1");
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:pic1");
  });

  it("tries again after a fetch that failed", async () => {
    const link = fakeLink({ session: session() });
    (link.fetch as ReturnType<typeof vi.fn>)
      .mockRejectedValueOnce(new Error("not connected"))
      .mockResolvedValueOnce({ ok: true, blob: async () => new Blob(["x"]) } as Response);
    setActiveLink(link);
    const { container } = await renderScreen(<SessionScreen />, "/s/a", link);
    await tick();
    const img = () => container.querySelector(".entry.agent img.pic") as HTMLImageElement;
    expect(img().hasAttribute("src")).toBe(false);
    // A tap asks again.
    act(() => img().click());
    await tick();
    expect(img().getAttribute("src")).toBe("blob:pic1");
    expect(link.fetch).toHaveBeenCalledTimes(2);
  });

  it("lets go of a blob that arrives after the screen went", async () => {
    const link = fakeLink({ session: session() });
    let answer!: (r: Response) => void;
    (link.fetch as ReturnType<typeof vi.fn>).mockReturnValue(new Promise<Response>((r) => (answer = r)));
    setActiveLink(link);
    const { unmount } = await renderScreen(<SessionScreen />, "/s/a", link);
    await tick();
    unmount();
    await act(async () => answer({ ok: true, blob: async () => new Blob(["x"]) } as Response));
    await tick();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:pic1");
  });
});
