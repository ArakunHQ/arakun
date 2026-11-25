import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useViewportFit } from "./viewport";

class FakeViewport extends EventTarget {
  width = 390;
  height = 800;
  scale = 1;
  offsetTop = 0;
}

let vv: FakeViewport;
const root = document.documentElement;

beforeEach(() => {
  vv = new FakeViewport();
  vi.stubGlobal("visualViewport", vv);
  root.style.removeProperty("--vh");
  root.classList.remove("kb");
});
afterEach(() => {
  vi.unstubAllGlobals();
  document.body.replaceChildren();
});

describe("useViewportFit", () => {
  it("takes the visible height while a field has the focus", () => {
    const { unmount } = renderHook(() => useViewportFit());
    expect(root.style.getPropertyValue("--vh")).toBe("800px");
    const field = document.createElement("textarea");
    document.body.appendChild(field);
    field.focus();
    vv.height = 400;
    vv.dispatchEvent(new Event("resize"));
    expect(root.style.getPropertyValue("--vh")).toBe("400px");
    unmount();
  });

  it("leaves a pinch zoom alone", () => {
    const { unmount } = renderHook(() => useViewportFit());
    const field = document.createElement("textarea");
    document.body.appendChild(field);
    field.focus();
    vv.scale = 2;
    vv.height = 400;
    vv.dispatchEvent(new Event("resize"));
    expect(root.style.getPropertyValue("--vh")).toBe("800px");
    unmount();
  });

  it("keeps the full height with no field focused", () => {
    const { unmount } = renderHook(() => useViewportFit());
    vv.height = 400;
    vv.dispatchEvent(new Event("resize"));
    expect(root.style.getPropertyValue("--vh")).toBe("800px");
    unmount();
  });
});
