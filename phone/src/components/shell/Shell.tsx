// The frame around the screens, by the width of the window. A phone shows one
// screen at a time with the tab bar under the top-level ones. A tablet has the
// rail and a desktop the sidebar, with the screen in the pane next to it. The
// pane stays where it is in the tree at both widths, so going from one session
// to the next, or turning a tablet, does not mount the frame again.

import { Outlet, useNavigate, useRouterState } from "@tanstack/react-router";
import { useContext, useEffect, useState } from "react";
import { useMe } from "../../lib/account";
import { LinkContext, useLink } from "../../lib/link";
import { connecting, connectingHint, needsYou } from "../../lib/sessions";
import { mode } from "../../lib/start";
import { useWidth } from "../../lib/useWidth";
import { NoticeBanners } from "../NoticeBanners";
import { SetupCards } from "../SetupCards";
import { StatusBar } from "../StatusBar";
import { TAB_PATHS, TabBar } from "../TabBar";
import { Rail } from "./Rail";
import { Sidebar } from "./Sidebar";
import "./Shell.css";

// Screens with no computer behind them yet: the whole window, at every width.
const ALONE = ["/signin", "/pair", "/moved", "/offline", "/fatal"];
// Where the page is on its way in: the inbox comes next.
const ON_THE_WAY = [...ALONE, "/", "/computers", "/inbox"];

/**
 * A desktop's pane with no session open: the first session that needs you,
 * else what the inbox would say over its list, which is the sidebar here.
 */
function Home() {
  const navigate = useNavigate();
  const first = useLink((s) => s.inbox.find(needsYou)?.id);
  const any = useLink((s) => s.inbox.length > 0);
  const connected = useLink((s) => s.connected);
  const host = useLink((s) => s.host);
  const pcOffline = useLink((s) => s.pcOffline);
  const me = useMe(mode() === "account");
  const notices = me.data && me.data.screen !== "signin" ? me.data.me?.notices || [] : [];

  useEffect(() => {
    if (first) void navigate({ to: "/s/$session", params: { session: first }, replace: true });
  }, [first, navigate]);

  let say = "Pick a session on the left.";
  let hint = "";
  if (!any) {
    say = connected ? "No sessions yet" : connecting({ pcOffline, host });
    hint = connected ? "Start one with New session, or on the computer." : connectingHint();
  }
  return (
    <div className="screen">
      <StatusBar />
      <div className="home">
        <NoticeBanners notices={notices} />
        <SetupCards />
        <div className="empty">
          <img className="mascot" src="mascot.svg" alt="" />
          <div>{say}</div>
          {hint ? <div className="hint">{hint}</div> : null}
        </div>
      </div>
    </div>
  );
}

export function Shell() {
  const width = useWidth();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const link = useContext(LinkContext);
  // A desktop opens what needs you only on the way in. Once the page has been
  // anywhere else, the Inbox tab and Back show the inbox's list: else they
  // would land on that session again, and its notices would never show.
  const [arrived, setArrived] = useState(false);
  useEffect(() => {
    if (!ON_THE_WAY.includes(path)) setArrived(true);
  }, [path]);
  // No computer yet (sign-in, pairing, the account's list before a pick):
  // nothing to lay out around the screen.
  if (!link || ALONE.includes(path)) return <Outlet />;

  if (width === "phone") {
    if (!TAB_PATHS.includes(path)) return <Outlet />;
    return (
      <div className="shell phone">
        <div className="pane">
          <Outlet />
        </div>
        <TabBar />
      </div>
    );
  }
  return (
    <div className={`shell ${width}`}>
      {width === "desktop" ? <Sidebar /> : <Rail />}
      <main className="pane">{width === "desktop" && path === "/inbox" && !arrived ? <Home /> : <Outlet />}</main>
    </div>
  );
}
