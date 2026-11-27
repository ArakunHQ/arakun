// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { useEffect, useRef, useState } from "react";

/// The text of a search box that keeps `q` in the URL. It waits for a pause
/// in typing before it calls `commit`, and follows `q` when something else
/// (a link, the back button) changes it.
export function useSearchBox(q: string | undefined, commit: (value: string | undefined) => void) {
  const [typed, setTyped] = useState(q ?? "");
  // The last q this box wrote to the URL: any other change came from outside.
  const written = useRef(q);
  useEffect(() => {
    if (q !== written.current) {
      written.current = q;
      setTyped(q ?? "");
    }
  }, [q]);
  useEffect(() => {
    const value = typed.trim() || undefined;
    if (value === q) return;
    const timer = setTimeout(() => {
      written.current = value;
      commit(value);
    }, 300);
    return () => clearTimeout(timer);
  }, [typed, q, commit]);
  return [typed, setTyped] as const;
}
