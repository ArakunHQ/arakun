import { useParams } from "@tanstack/react-router";
import { useContext, useEffect } from "react";
import { LinkContext } from "../lib/link";
import { openSession } from "../lib/start";

// A placeholder until the session screen is ported from app.js. It opens the
// session already, so a notification tap is checked against this computer.
export function SessionScreen() {
  const { session } = useParams({ from: "/s/$session" });
  const link = useContext(LinkContext);
  useEffect(() => {
    if (link) openSession(link, session);
  }, [link, session]);
  return (
    <div className="screen">
      <div className="statusbar" />
      <div className="hdr">
        <h1>Session</h1>
      </div>
    </div>
  );
}
