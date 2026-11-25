// Which of the three layouts the window is wide enough for. The breakpoints
// are the mockups': a phone under 768 px, a tablet under 1200 px, a desktop above.

import { useSyncExternalStore } from "react";

export type Width = "phone" | "tablet" | "desktop";

const TABLET = "(min-width: 768px)";
const DESKTOP = "(min-width: 1200px)";

// Without matchMedia (an old test setup) the page is a phone, as it was.
const media = () => (typeof matchMedia === "function" ? matchMedia : null);

export function currentWidth(): Width {
  const mm = media();
  if (!mm) return "phone";
  if (mm(DESKTOP).matches) return "desktop";
  return mm(TABLET).matches ? "tablet" : "phone";
}

function subscribe(fn: () => void): () => void {
  const mm = media();
  if (!mm) return () => {};
  const lists = [mm(TABLET), mm(DESKTOP)];
  for (const l of lists) l.addEventListener("change", fn);
  return () => {
    for (const l of lists) l.removeEventListener("change", fn);
  };
}

export function useWidth(): Width {
  return useSyncExternalStore(subscribe, currentWidth);
}
