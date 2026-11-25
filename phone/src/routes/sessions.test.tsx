import { fireEvent, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { fakeLink, renderScreen } from "../test/screen";
import { Sessions } from "./sessions";

const row = (id: string, title: string, project: string, over = {}) => ({ id, title, agent: "claude", status: "idle", project, updated: 1, ...over });
const inbox = [row("1", "Fix login", "web"), row("2", "Add tests", "web"), row("3", "Port the page", "phone")] as never;

const titles = (c: HTMLElement) => [...c.querySelectorAll(".srow .t")].map((e) => e.textContent);

describe("sessions", () => {
  it("lists every session under its project", async () => {
    const { container } = await renderScreen(<Sessions />, "/sessions", fakeLink({ inbox }));
    expect(titles(container)).toEqual(["Fix login", "Add tests", "Port the page"]);
    expect(screen.getByText("3 sessions · 2 projects")).toBeTruthy();
  });

  it("filters by title and by project in the search box", async () => {
    const { container } = await renderScreen(<Sessions />, "/sessions", fakeLink({ inbox }));
    const box = screen.getByPlaceholderText("Search sessions…");
    fireEvent.change(box, { target: { value: "login" } });
    expect(titles(container)).toEqual(["Fix login"]);
    fireEvent.change(box, { target: { value: "PHONE" } });
    expect(titles(container)).toEqual(["Port the page"]);
    fireEvent.change(box, { target: { value: "zzz" } });
    expect(screen.getByText("Nothing matches “zzz”")).toBeTruthy();
  });

  it("narrows to one project with its chip", async () => {
    const { container } = await renderScreen(<Sessions />, "/sessions", fakeLink({ inbox }));
    fireEvent.click(screen.getByRole("button", { name: "phone" }));
    expect(titles(container)).toEqual(["Port the page"]);
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    expect(titles(container)).toHaveLength(3);
  });
});
