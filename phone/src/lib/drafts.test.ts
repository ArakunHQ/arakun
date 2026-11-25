import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DRAFT_KEY, keepDraft, kept, loadDraft, PLACE_KEY } from "./drafts";

beforeEach(() => {
  localStorage.clear();
  // As relay.js scopes a key to a computer.
  (globalThis as any).RELAY = {
    scoped: (key: string, label: string | null) => (!label ? key : key.endsWith(":") ? `${key.slice(0, -1)}@${label}:` : `${key}@${label}`),
  };
});
afterEach(() => {
  delete (globalThis as any).RELAY;
});

describe("drafts", () => {
  it("uses the keys the old page used", () => {
    expect(DRAFT_KEY).toBe("arakun.draft:");
    expect(PLACE_KEY).toBe("arakun.place");
    expect(kept(PLACE_KEY)).toBe("arakun.place");
  });

  it("keeps a draft per session and forgets an empty one", () => {
    keepDraft("a", "one");
    keepDraft("b", "two");
    expect(localStorage.getItem("arakun.draft:a")).toBe("one");
    expect(loadDraft("a")).toBe("one");
    expect(loadDraft("b")).toBe("two");
    keepDraft("a", "");
    expect(localStorage.getItem("arakun.draft:a")).toBeNull();
    expect(loadDraft("a")).toBe("");
  });
});
