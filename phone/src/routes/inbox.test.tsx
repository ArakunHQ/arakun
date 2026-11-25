import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { needsYou } from "../lib/sessions";
import { fakeLink, renderScreen } from "../test/screen";
import { Inbox } from "./inbox";

const now = Math.floor(Date.now() / 1000);
const row = (id: string, status: string, over = {}) => ({ id, title: `t-${id}`, agent: "claude", status, project: "p", updated: now - 60, ...over });

describe("inbox", () => {
  it("puts what needs you first", async () => {
    const link = fakeLink({
      inbox: [row("a", "working"), row("b", "idle"), row("c", "needs_input", { asks: { kind: "permission", text: "rm x" } }), row("d", "error")] as never,
    });
    const { container } = await renderScreen(<Inbox />, "/inbox", link);
    const titles = [...container.querySelectorAll(".srow .t")].map((e) => e.textContent);
    expect(titles).toEqual(["t-c", "t-d", "t-a", "t-b"]);
    expect(container.querySelector(".group.needs h2")?.textContent).toBe("Needs you");
    // One word with the sidebar's group.
    expect(container.querySelector(".group.done h2")?.textContent).toBe("Done");
    expect(screen.getByText("Run: rm x")).toBeTruthy();
    expect(screen.getByText("Stopped with an error")).toBeTruthy();
  });


  it("opens a session through the link", async () => {
    const link = fakeLink({ inbox: [row("a", "working")] as never });
    const { router } = await renderScreen(<Inbox />, "/inbox", link);
    (await screen.findByText("t-a")).click();
    expect(link.sent).toEqual([{ type: "open", session: "a" }]);
    await new Promise((r) => setTimeout(r, 0));
    expect(router.state.location.pathname).toBe("/s/a");
  });
});

describe("needsYou", () => {
  it("is true for a question, a permission and an error", () => {
    expect(needsYou({ status: "needs_input" })).toBe(true);
    expect(needsYou({ status: "error" })).toBe(true);
    expect(needsYou({ status: "idle" })).toBe(false);
    expect(needsYou({ status: "working" })).toBe(false);
  });
});
