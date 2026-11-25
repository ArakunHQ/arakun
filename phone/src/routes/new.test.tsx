import { fireEvent, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { TabBar } from "../components/TabBar";
import type { OptionsMessage, Place } from "../lib/model";
import { fakeLink, renderScreen } from "../test/screen";
import { NewSession } from "./new";

const places: Place[] = [
  { path: "/w/web", project: "web", env: "web", agent: "claude" },
  { path: "/w/api", project: "api", env: "api", agent: "codex" },
  { path: "/w/api-fix", project: "api", env: "fix", branch: "fix", agent: "claude" },
];

const models: OptionsMessage = {
  type: "options",
  env: "/w/api",
  agent: "droid",
  settings: [
    {
      key: "modelId",
      label: "Model",
      current: "m1",
      options: [
        { id: "m1", name: "One" },
        { id: "m2", name: "Two" },
      ],
    },
  ],
};

beforeEach(() => {
  localStorage.clear();
  // As relay.js keeps a key as it is on the computer's own server.
  (globalThis as any).RELAY = { scoped: (key: string) => key };
});
afterEach(() => {
  delete (globalThis as any).RELAY;
});

const last = (link: { sent: unknown[] }) => link.sent[link.sent.length - 1];

describe("new session", () => {
  it("sends the start message for the project, agent and model picked, and keeps the place", async () => {
    const link = fakeLink({ places });
    await renderScreen(<NewSession />, "/new", link);
    fireEvent.change(screen.getByLabelText("Project"), { target: { value: "/w/api" } });
    expect(last(link)).toEqual({ type: "options", env: "/w/api", agent: "codex" });
    fireEvent.click(screen.getByRole("button", { name: "Droid" }));
    expect(last(link)).toEqual({ type: "options", env: "/w/api", agent: "droid" });
    link.set({ options: models });
    fireEvent.click(screen.getByRole("button", { name: "Two" }));
    fireEvent.change(screen.getByLabelText("First message"), { target: { value: "Fix the login" } });
    fireEvent.click(screen.getByRole("button", { name: "Start session" }));
    expect(last(link)).toEqual({ type: "start", env: "/w/api", agent: "droid", text: "Fix the login", settings: [["modelId", "m2"]] });
    expect(localStorage.getItem("arakun.place")).toBe("/w/api");
    expect((screen.getByRole("button", { name: "Starting…" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("starts on the place used last, with its agent", async () => {
    localStorage.setItem("arakun.place", "/w/api");
    await renderScreen(<NewSession />, "/new", fakeLink({ places }));
    expect((screen.getByLabelText("Project") as HTMLSelectElement).value).toBe("/w/api");
    expect(screen.getByRole("button", { name: "Codex" }).className).toContain("on");
  });

  it("can send again after the computer refused the start", async () => {
    const link = fakeLink({ places });
    await renderScreen(<NewSession />, "/new", link);
    fireEvent.change(screen.getByLabelText("First message"), { target: { value: "Go" } });
    fireEvent.click(screen.getByRole("button", { name: "Start session" }));
    link.emit({ type: "error", message: "No such place" });
    expect((screen.getByRole("button", { name: "Start session" }) as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("the + in the tab bar", () => {
  it("asks which project, then starts there from the quick sheet", async () => {
    const link = fakeLink({ places });
    await renderScreen(<TabBar />, "/inbox", link);
    fireEvent.click(screen.getByRole("button", { name: "New session" }));
    expect(screen.getByText("Which project?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    expect(last(link)).toEqual({ type: "options", env: "/w/api", agent: "codex" });
    fireEvent.change(screen.getByPlaceholderText("What should Codex do?"), { target: { value: "Add a test" } });
    fireEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(last(link)).toEqual({ type: "start", env: "/w/api", agent: "codex", text: "Add a test", settings: [] });
    expect(localStorage.getItem("arakun.place")).toBe("/w/api");
  });

  it("goes straight to the quick sheet with one project", async () => {
    await renderScreen(<TabBar />, "/inbox", fakeLink({ places: places.slice(0, 1) }));
    fireEvent.click(screen.getByRole("button", { name: "New session" }));
    expect(screen.queryByText("Which project?")).toBeNull();
    expect(screen.getByPlaceholderText("What should Claude Code do?")).toBeTruthy();
  });

  it("picks another branch of the project in the quick sheet", async () => {
    const link = fakeLink({ places });
    await renderScreen(<TabBar />, "/inbox", link);
    fireEvent.click(screen.getByRole("button", { name: "New session" }));
    fireEvent.click(screen.getByRole("button", { name: /^api/ }));
    fireEvent.click(screen.getByRole("button", { name: "api" }));
    fireEvent.click(screen.getByRole("button", { name: /^fix/ }));
    expect(last(link)).toEqual({ type: "options", env: "/w/api-fix", agent: "claude" });
  });

  it("takes what is written to the full form with More options", async () => {
    const { router } = await renderScreen(<TabBar />, "/inbox", fakeLink({ places: places.slice(0, 1) }));
    fireEvent.click(screen.getByRole("button", { name: "New session" }));
    fireEvent.change(screen.getByPlaceholderText("What should Claude Code do?"), { target: { value: "Half a thought" } });
    fireEvent.click(screen.getByRole("button", { name: "More options" }));
    await screen.findByText("/new");
    expect(router.state.location.pathname).toBe("/new");
    expect(router.state.location.state.startForm?.text).toBe("Half a thought");
  });

  it("closes the project tiles with the phone's back", async () => {
    const { router } = await renderScreen(<TabBar />, "/inbox", fakeLink({ places }));
    fireEvent.click(screen.getByRole("button", { name: "New session" }));
    expect(screen.getByText("Which project?")).toBeTruthy();
    router.history.back();
    await screen.findByRole("button", { name: "New session" });
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByText("Which project?")).toBeNull();
  });
});
