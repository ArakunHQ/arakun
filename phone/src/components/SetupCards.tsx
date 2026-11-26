// The cards that get the app on the home screen and notifications on.

import { useReducer, useSyncExternalStore } from "react";
import { Icon } from "../lib/icons";
import { BetaCard } from "./BetaSignup";
import { useLink } from "../lib/link";
import { notifyState, subscribePush } from "../lib/push";
import { toast } from "../lib/toast";
import "./SetupCards.css";

export const HIDE_INSTALL = "arakun.hideInstall";
export const HIDE_NOTIFY = "arakun.hideNotify";

// The browser's own install offer, held back until the person asks for it.
interface InstallOffer extends Event {
  prompt(): Promise<unknown>;
}
let installOffer: InstallOffer | null = null;
const offerListeners = new Set<() => void>();
function setOffer(o: InstallOffer | null) {
  installOffer = o;
  for (const fn of offerListeners) fn();
}
function subscribeOffer(fn: () => void) {
  offerListeners.add(fn);
  return () => offerListeners.delete(fn);
}

// Listening from the start: the browser offers once, perhaps before the inbox is on screen.
window.addEventListener("beforeinstallprompt", (ev) => {
  ev.preventDefault();
  setOffer(ev as InstallOffer);
});
window.addEventListener("appinstalled", () => {
  setOffer(null);
  toast("Arakun is on your home screen");
});

/** Whether the page runs from the home screen already. */
function standalone(): boolean {
  return (typeof matchMedia === "function" && matchMedia("(display-mode: standalone)").matches) || (navigator as { standalone?: boolean }).standalone === true;
}

/**
 * A Safari tab on an iPhone or iPad: no install offer ever comes, and push
 * exists only once the page is on the home screen, so the card has to say so.
 * (Mac Safari has navigator.standalone too, but no touchscreen.)
 */
export function appleTab(): boolean {
  return "standalone" in navigator && navigator.standalone === false && navigator.maxTouchPoints > 0;
}

/** Asks for the permission and subscribes; says how it went. */
export async function turnOnNotifications(pushKey: string): Promise<void> {
  const got = await Notification.requestPermission();
  if (got !== "granted") return;
  try {
    await subscribePush(pushKey);
    toast("You will be told when the agent needs you");
  } catch {
    toast("Could not turn notifications on");
  }
}

function Card({ id, icon, title, text, action, onGo, onSkip }: { id: string; icon: "phone" | "bell"; title: string; text: string; action: string; onGo: () => void; onSkip: () => void }) {
  return (
    <div className="setup" data-setup={id}>
      <span className="sicon">
        <Icon name={icon} />
      </span>
      <span className="body">
        <div className="t">{title}</div>
        <div className="s">{text}</div>
      </span>
      {action ? (
        <button className="btn outline small" onClick={onGo}>
          {action}
        </button>
      ) : null}
      <button className="x" aria-label="Not now" onClick={onSkip}>
        <Icon name="close" />
      </button>
    </div>
  );
}

export function SetupCards() {
  const offer = useSyncExternalStore(subscribeOffer, () => installOffer);
  const pushKey = useLink((s) => s.pushKey);
  // What a tap changes lives outside React (storage, the permission): draw again.
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  const skip = (key: string) => () => {
    localStorage.setItem(key, "1");
    redraw();
  };
  const showInstall = !standalone() && !localStorage.getItem(HIDE_INSTALL) && (offer || appleTab());
  const showNotify = notifyState() === "off" && !localStorage.getItem(HIDE_NOTIFY);
  return (
    <>
      {showInstall ? (
        <Card
          id="install"
          icon="phone"
          title="Add Arakun to your home screen"
          text={offer ? "Opens full screen, like an app." : "Tap Share, then “Add to Home Screen”. Notifications only work from there."}
          action={offer ? "Add" : ""}
          onGo={async () => {
            if (!offer) return;
            setOffer(null);
            await offer.prompt();
            redraw();
          }}
          onSkip={skip(HIDE_INSTALL)}
        />
      ) : null}
      {showNotify ? (
        <Card
          id="notify"
          icon="bell"
          title="Know when the agent needs you"
          text="A notification for permissions, questions and finished turns."
          action="Turn on"
          onGo={async () => {
            await turnOnNotifications(pushKey);
            redraw();
          }}
          onSkip={skip(HIDE_NOTIFY)}
        />
      ) : null}
      <BetaCard />
    </>
  );
}
