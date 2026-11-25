// iOS keeps the page its full height under the keyboard and slides it up, so
// the header and the newest lines leave the screen: the page takes the height
// that stays visible instead, as `--vh` on <html>. A pinch zoom shrinks that
// height too, and is left alone.

import { useEffect } from "react";

let fit = () => {};

/** Measures the visible height again after each of `delays` ms, as the keyboard comes or goes. */
export function refit(...delays: number[]): void {
  for (const ms of delays) setTimeout(() => fit(), ms);
}

function typing(): boolean {
  const el = document.activeElement as HTMLElement | null;
  if (!el) return false;
  return el.tagName === "TEXTAREA" || (el.tagName === "INPUT" && (el as HTMLInputElement).type !== "checkbox") || el.isContentEditable;
}

/** Keeps `--vh` on <html> at the height the keyboard leaves. A no-op without `visualViewport`. */
export function useViewportFit(): void {
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const root = document.documentElement;
    // The tallest the page has been with no keyboard, for this width: iOS does
    // not always say when the keyboard is down again (after sending, say), and
    // a height kept from the keyboard would leave a gap under the composer.
    let full = 0;
    let width = 0;
    const measure = () => {
      if (Math.abs(vv.scale - 1) > 0.01) return;
      if (Math.round(vv.width) !== width) {
        width = Math.round(vv.width);
        full = 0;
      }
      let h = Math.round(vv.height);
      if (typing()) {
        // The keyboard takes more than a toolbar's worth of the window.
        root.classList.toggle("kb", window.innerHeight - vv.height > 120);
      } else {
        // No field has the focus, so no keyboard: the whole height there is.
        full = Math.max(full, h);
        h = full;
        root.classList.remove("kb");
      }
      root.style.setProperty("--vh", `${h}px`);
      if (vv.offsetTop > 0) window.scrollTo(0, 0);
    };
    // While the keyboard goes down, measure again until it is gone.
    const out = () => refit(0, 120, 300, 600);
    const into = () => refit(0, 300);
    vv.addEventListener("resize", measure);
    vv.addEventListener("scroll", measure);
    document.addEventListener("focusout", out);
    document.addEventListener("focusin", into);
    fit = measure;
    measure();
    return () => {
      vv.removeEventListener("resize", measure);
      vv.removeEventListener("scroll", measure);
      document.removeEventListener("focusout", out);
      document.removeEventListener("focusin", into);
      if (fit === measure) fit = () => {};
    };
  }, []);
}
