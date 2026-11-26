// The service worker: what lets the phone add the page to its home screen as an
// app, and what hears push messages while the page is closed. It caches
// nothing on purpose: the page comes from the computer right next to it, and
// a stale copy would be worse than no page while that computer is off.
"use strict";

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (ev) => ev.waitUntil(self.clients.claim()));

// A notice from the computer: { kind, session, title, body, env?, secs? }. The computer
// already skips phones looking at that session; this covers the moment in
// between, when the page is open on it but the computer has not heard yet.
self.addEventListener("push", (ev) => {
  let msg;
  try {
    msg = ev.data.json();
  } catch (e) {
    return;
  }
  ev.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((wins) => {
      const looking = wins.some((w) => w.visibilityState === "visible" && showing(w.url) === String(msg.session));
      if (looking) return;
      return self.registration.showNotification(msg.title || "Arakun", {
        body: withSubtitle(subtitle(msg.env, msg.secs), msg.body || ""),
        icon: "icon-192.png",
        badge: "icon-192.png",
        // One notice per session on the shade: a newer one replaces the older.
        tag: `arakun-${msg.session}`,
        renotify: true,
        data: { session: msg.session },
      });
    })
  );
});

// "app", or "app \u00b7 4m 12s" for a turn of a minute or more and "app \u00b7 42s"
// for a shorter one. Empty without an environment. The page has no subtitle
// line, so it goes first in the body.
function subtitle(env, secs) {
  if (typeof env !== "string" || !env) return "";
  if (typeof secs !== "number" || !isFinite(secs) || secs < 0) return env;
  const s = Math.floor(secs);
  return s >= 60 ? `${env} \u00b7 ${Math.floor(s / 60)}m ${s % 60}s` : `${env} \u00b7 ${s}s`;
}

function withSubtitle(sub, body) {
  return sub ? (body ? `${sub}\n${body}` : sub) : body;
}

// The session a window's address shows: `#/s/<id>`, or `#s=<id>` from before
// the router. Whole ids only: `abc` is not `abcd`. A sheet open over the
// session adds `?sheet=…`, and the session is still on screen.
function showing(url) {
  const m = /#(?:\/s\/|s=)([^/?#]+)(?:\?[^#]*)?$/.exec(url);
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]);
  } catch (e) {
    return null;
  }
}

// Tapping the notice opens that session: in the page already open, or in a new one.
self.addEventListener("notificationclick", (ev) => {
  ev.notification.close();
  const session = ev.notification.data && ev.notification.data.session;
  const target = session ? `/#s=${encodeURIComponent(session)}` : "/";
  ev.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((wins) => {
      const win = wins[0];
      if (win) {
        win.postMessage({ open: session });
        return win.focus();
      }
      return self.clients.openWindow(target);
    })
  );
});
