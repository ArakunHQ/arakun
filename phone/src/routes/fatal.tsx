import { useRouter } from "@tanstack/react-router";
import { useEffect } from "react";
import { RELAY } from "../lib/globals";
import { nextFatalWait, retryStart } from "../lib/start";
import "./pair.css";

// The account host failed before saying which way in this is. Never the
// pairing screen, which has nothing to pair with here. The page asks again by
// itself, a little later each time.
export function Fatal() {
  const router = useRouter();
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    const later = () => {
      timer = setTimeout(async () => {
        retryStart();
        await router.invalidate();
        if (alive) later();
      }, nextFatalWait());
    };
    later();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [router]);
  return (
    <div className="screen">
      <div className="pair">
        <img className="mascot" src="mascot.svg" alt="" />
        <h1>Cannot connect</h1>
        <p>{RELAY.unreachable()}</p>
        {/* Starting over checks everything again; nothing is trusted on the way. */}
        <button className="btn" onClick={() => location.reload()}>
          Try again
        </button>
        <div className="hint">{location.hostname}</div>
      </div>
    </div>
  );
}
