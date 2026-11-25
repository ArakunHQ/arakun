// A short line at the bottom of the screen that goes by itself, as `toast()`
// in the old app.js. Anything can call `toast`; the root shows it.

import { useEffect, useState, useSyncExternalStore } from "react";

const SHOW_MS = 2200;

let current = { text: "", at: 0 };
const listeners = new Set<() => void>();

export function toast(text: string): void {
  current = { text, at: Date.now() };
  for (const fn of listeners) fn();
}

/** The last line said, for tests. */
export function lastToast(): string {
  return current.text;
}

function subscribe(fn: () => void) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function Toaster() {
  const said = useSyncExternalStore(subscribe, () => current);
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!said.text) return;
    setShown(true);
    const t = setTimeout(() => setShown(false), SHOW_MS);
    return () => clearTimeout(t);
  }, [said]);
  return (
    <div className={`toast${shown ? " show" : ""}`} role="status">
      {said.text}
    </div>
  );
}
