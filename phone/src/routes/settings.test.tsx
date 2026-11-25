import { screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fakeLink, renderScreen } from "../test/screen";
import { Settings } from "./settings";

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
});
