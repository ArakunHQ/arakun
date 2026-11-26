// Which sessions have their notices off. The computer keeps the list and
// sends it in `/me`; the link holds that answer, and a toggle puts the
// computer's reply back in it.

import { type Link, useLink } from "./link";
import { toast } from "./toast";

/** Ids of the muted sessions; none for a computer that does not know muting. */
export function useMuted(): string[] {
  return useLink((s) => s.me?.muted) ?? NONE;
}
const NONE: string[] = [];

/** Whether the computer knows muting: an older one sends no `muted` in `/me`. */
export function useCanMute(): boolean {
  return useLink((s) => Array.isArray(s.me?.muted));
}

/** Mutes or unmutes one session on the computer. Says so when it could not. */
export async function setMuted(link: Link, session: string, muted: boolean): Promise<void> {
  try {
    const res = await link.fetch("/mute", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session, muted }),
    });
    if (!res.ok) throw new Error(String(res.status));
    const body = (await res.json()) as { muted?: string[] };
    if (Array.isArray(body.muted)) link.setMe({ muted: body.muted });
  } catch {
    toast("Could not save that");
  }
}
