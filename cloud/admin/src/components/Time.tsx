// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { exact, relative } from "../format";

/// A Unix time as "3 days ago", with the exact time on hover.
export function Time({ at }: { at: number }) {
  return (
    <time dateTime={new Date(at * 1000).toISOString()} title={exact(at)}>
      {relative(at, Math.floor(Date.now() / 1000))}
    </time>
  );
}
