import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { type ReactNode, useReducer, useState } from "react";
import { turnOnNotifications, appleTab } from "../components/SetupCards";
import { StatusBar } from "../components/StatusBar";
import { TopBar } from "../components/shell/TopBar";
import { meQuery, signedOut, useMachines, useSignedInSessions, useSignOut } from "../lib/account";
import { activeLink, api, setActiveLink } from "../lib/api";
import { ago, clock } from "../lib/format";
import { type Machine, RELAY } from "../lib/globals";
import { Icon, type IconName } from "../lib/icons";
import { useLink } from "../lib/link";
import { deviceName } from "../lib/pair";
import { useWidth } from "../lib/useWidth";
import { notifyState } from "../lib/push";
import { mode, setPairMessage } from "../lib/start";
import { toast } from "../lib/toast";
import { forgetToken } from "../lib/token";

type Notices = { done: boolean; input: boolean; error: boolean };
const ALL_ON: Notices = { done: true, input: true, error: true };

/** How this page reaches the computer, from the address it was opened at. */
function wayIn(): string {
  if (mode() === "account") return "Connected through the relay, end-to-end encrypted";
  if (/\.ts\.net$/.test(location.hostname)) return "Connected through Tailscale";
  if (location.protocol === "https:") return "Connected over HTTPS";
  return "Connected on the local network";
}

function pairedOn(unix: number): string {
  if (!unix) return "";
  return new Date(unix * 1000).toLocaleDateString(document.documentElement.lang || "en", { month: "short", day: "numeric" });
}

const esc = (s: unknown) => String(s ?? "").replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function Row({ icon, title, sub, right, danger, onClick }: { icon?: IconName; title: ReactNode; sub?: ReactNode; right?: ReactNode; danger?: boolean; onClick?: () => void }) {
  const inner = (
    <>
      {icon ? (
        <span className="ricon">
          <Icon name={icon} size={20} />
        </span>
      ) : null}
      <span className="rbody">
        <span className="rt">{title}</span>
        {sub ? <span className="rs">{sub}</span> : null}
      </span>
      {right}
    </>
  );
  const cls = `prow${danger ? " danger" : ""}`;
  return onClick ? (
    <button className={cls} onClick={onClick}>
      {inner}
    </button>
  ) : (
    <div className={cls}>{inner}</div>
  );
}

function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section className="group">
      <div className="glabel">{label}</div>
      <div className="gbox">{children}</div>
    </section>
  );
}

/** A switch; the whole row is its target, not only the knob. */
function Toggle({ icon, title, on, onFlip }: { icon: IconName; title: string; on: boolean; onFlip: (on: boolean) => void }) {
  return (
    <div className="prow" onClick={() => onFlip(!on)}>
      <span className="ricon">
        <Icon name={icon} size={20} />
      </span>
      <span className="rbody">
        <span className="rt">{title}</span>
      </span>
      <button
        className={`switch${on ? " on" : ""}`}
        role="switch"
        aria-checked={on}
        aria-label={title}
        onClick={(e) => {
          e.stopPropagation();
          onFlip(!on);
        }}
      >
        <span className="knob" />
      </button>
    </div>
  );
}

/** The account's signed-in devices, this phone marked. */
function SignedInDevices() {
  const qc = useQueryClient();
  const q = useSignedInSessions();
  const said = (text: string) => (
    <div className="prow">
      <span className="rbody">
        <span className="rs">{text}</span>
      </span>
    </div>
  );
  const got = q.data;
  const list = got && "list" in got ? (got.list as { id: string; name: string }[]) : null;
  let body: ReactNode;
  if (q.isError) body = said("Could not ask your account.");
  else if (!list) body = said("Asking your account…");
  else {
    // relay.js builds the rows and escapes them; the sign-out buttons are found by their id.
    const html = RELAY.signedInRows(list, esc, ago).join("");
    body = (
      <div
        dangerouslySetInnerHTML={{ __html: html }}
        onClick={async (e) => {
          const id = (e.target as HTMLElement).closest<HTMLElement>("[data-signout]")?.dataset.signout;
          const device = list.find((x) => x.id === id);
          if (!device) return;
          if (!confirm(`Sign out ${device.name}? It needs to sign in again to reach your computers.`)) return;
          let done: Awaited<ReturnType<typeof RELAY.signOutSession>>;
          try {
            done = await RELAY.signOutSession(device.id);
          } catch {
            done = { ok: false };
          }
          if ("signedOut" in done) return signedOut(qc);
          if (!done.ok) toast(`Could not sign out ${device.name}`);
          void qc.invalidateQueries({ queryKey: ["sessions"] });
        }}
      />
    );
  }
  return <Group label="Signed-in devices">{body}</Group>;
}

export function Settings() {
  const navigate = useNavigate();
  const width = useWidth();
  const me = useLink((s) => s.me);
  const host = useLink((s) => s.host);
  const connected = useLink((s) => s.connected);
  const pcOffline = useLink((s) => s.pcOffline);
  const pushKey = useLink((s) => s.pushKey);
  const account = mode() === "account";
  const boot = useQuery({ ...meQuery, enabled: account });
  const bootData = boot.data && boot.data.screen !== "signin" ? boot.data : null;
  const machines = useMachines(account && !!bootData);
  const computers: Machine[] = machines.data && "list" in machines.data ? machines.data.list : bootData?.machines || [];
  const signOut = useSignOut();
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  // What the person just flipped, until the computer's own answer replaces it.
  const [flipped, setFlipped] = useState<Notices | null>(null);
  const notices: Notices = flipped ?? { ...ALL_ON, ...(me?.notices || {}) };

  // One kind of notice on or off; the computer keeps the choice.
  const setNotice = async (key: keyof Notices, on: boolean) => {
    if (!me) return;
    const before = notices;
    const next = { ...before, [key]: on };
    setFlipped(next);
    try {
      const res = await api("/push", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ notices: next }),
      });
      if (!res.ok) throw new Error(String(res.status));
      setFlipped({ ...next, ...((await res.json()).notices || {}) });
    } catch {
      setFlipped(before);
      toast("Could not save that");
    }
  };

  // This device lets go of the computer it paired with on the network: its
  // token dies there, and here. On the account host it signs out instead.
  const unpair = async () => {
    const name = host || location.hostname;
    if (!confirm(`Unpair this device from ${name}?\n\nYou will need a new code from Arakun to pair it again.`)) return;
    try {
      await api("/unpair", { method: "POST" });
    } catch {
      // The computer may be away; the token is dropped here regardless.
    }
    if ("serviceWorker" in navigator && window.isSecureContext) {
      try {
        const reg = await navigator.serviceWorker.ready;
        const sub = await reg.pushManager.getSubscription();
        if (sub) await sub.unsubscribe();
      } catch {
        // Nothing to undo.
      }
    }
    const link = activeLink();
    setActiveLink(null);
    link?.close();
    forgetToken();
    setPairMessage("");
    void navigate({ to: "/pair" });
  };

  const askSignOut = () => {
    if (signOut.isPending) return;
    const login = bootData?.me?.login;
    if (confirm(`Sign out of ${login || "GitHub"} in this browser?`)) signOut.mutate();
  };

  const ns = notifyState();
  let notify: ReactNode[];
  if (ns === "on") {
    notify = [
      <Toggle key="done" icon="check" title="Turn finished" on={notices.done} onFlip={(on) => setNotice("done", on)} />,
      <Toggle key="input" icon="help" title="Needs permission or answer" on={notices.input} onFlip={(on) => setNotice("input", on)} />,
      <Toggle key="error" icon="alert" title="Something went wrong" on={notices.error} onFlip={(on) => setNotice("error", on)} />,
    ];
  } else if (ns === "off") {
    notify = [
      <Row
        key="n"
        icon="bell"
        title="Notifications"
        sub="For permissions, questions, finished turns and errors."
        right={
          <button
            className="btn outline small"
            onClick={async () => {
              await turnOnNotifications(pushKey);
              redraw();
            }}
          >
            Turn on
          </button>
        }
      />,
    ];
  } else if (ns === "denied") {
    notify = [<Row key="n" icon="bell" title="Notifications" sub="Blocked by the browser. Allow them for this site in its settings to turn them back on." />];
  } else if (appleTab()) {
    notify = [<Row key="n" icon="bell" title="Notifications" sub="Add Arakun to your home screen first: tap Share, then “Add to Home Screen”." />];
  } else {
    notify = [<Row key="n" icon="bell" title="Notifications" sub="They need the HTTPS address (Tailscale). The local network cannot deliver them." />];
  }
  // One origin keeps one push subscription: say which computer it is for.
  const note = account && ns !== "unsupported" && ns !== "denied" ? RELAY.pushNote(computers, host) : "";
  if (note) notify.unshift(<Row key="from" icon="monitor" title="From one computer" sub={note} />);

  const name = String(me?.name || deviceName());
  return (
    <div className="screen">
      <StatusBar />
      <TopBar />
      <div className="hdr">
        <div className="grow">
          <h1>Settings</h1>
        </div>
      </div>
      <div className="settings">
        <Group label="Desktop">
          <Row icon="monitor" title={host || location.hostname} sub={connected ? wayIn() : pcOffline ? `Away since ${clock(pcOffline)}` : "Reconnecting…"} />
        </Group>
        {/* A phone has no Usage tab: it is here. */}
        {width === "phone" ? (
          <Group label="Agents">
            <Row icon="gauge" title="Usage" onClick={() => void navigate({ to: "/usage" })} />
          </Group>
        ) : null}
        <Group label="Notifications">{notify}</Group>
        {account ? <SignedInDevices /> : null}
        <Group label="This device">
          {account ? (
            <>
              <Row icon="phone" title={name} sub={bootData?.me?.login ? `Signed in as ${bootData.me.login}` : ""} />
              <Row icon="unlink" title="Sign out" danger onClick={askSignOut} />
            </>
          ) : (
            <>
              <Row icon="phone" title={name} sub={me?.paired_at ? `Paired ${pairedOn(Number(me.paired_at))} · last seen now` : ""} />
              <Row icon="unlink" title="Unpair this device" danger onClick={unpair} />
            </>
          )}
        </Group>
      </div>
    </div>
  );
}
