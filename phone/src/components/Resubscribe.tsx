import { useContext, useEffect, useRef } from "react";
import { LinkContext, useLink } from "../lib/link";
import { notifyState, subscribePush } from "../lib/push";

function Watch() {
  const me = useLink((s) => s.me);
  const pushKey = useLink((s) => s.pushKey);
  // The `me` already acted on: a second run for it (StrictMode) does nothing.
  const done = useRef<unknown>(null);
  useEffect(() => {
    if (!me || me.notifying || !pushKey || done.current === me) return;
    if (notifyState() !== "on") return;
    done.current = me;
    // Nothing to tell the person: the cards and switches stay as they are.
    subscribePush(pushKey).catch(() => {});
  }, [me, pushKey]);
  return null;
}

/**
 * Where the permission is given but the computer does not know this phone
 * (after a new pairing), subscribes again, so notices do not stay silent until
 * the person toggles them. Mount once, above the screens.
 */
export function Resubscribe() {
  return useContext(LinkContext) ? <Watch /> : null;
}
