// The computer this page talks to: its name, its kind and whether it answers.
// On the account host it opens a menu with the account's computers, to switch
// or remove one, and Sign out. On the computer's own server there is nothing
// to switch to, so it is a plain label.

import { useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { pick, pickedMachine, useMachines, useMe, useRemoveMachine, useSignOut } from "../../lib/account";
import { ago } from "../../lib/format";
import { type Machine, RELAY } from "../../lib/globals";
import { Icon } from "../../lib/icons";
import { useLink } from "../../lib/link";
import { mode } from "../../lib/start";
import "./ComputerPicker.css";

/** Where the picker sits: the phone's top bar, the desktop sidebar or the tablet rail. */
export type PickerLook = "chip" | "card" | "icon";

const kindOf = (m: Machine | null) => (m?.kind === "server" ? "Server" : "Arakun app");

// What the menu says about when a computer was there.
function seen(m: Machine): string {
  if (m.online) return "online";
  const at = Number(m.last_seen_at) || 0;
  if (!at) return "offline";
  const when = ago(at);
  return `last seen ${when}${when === "now" ? "" : " ago"}`;
}

/** Sign out of the account, asked first. */
export function useAskSignOut(): () => void {
  const signOut = useSignOut();
  const me = useMe(mode() === "account");
  const login = me.data?.me?.login;
  return () => {
    if (signOut.isPending) return;
    if (confirm(`Sign out of ${login || "GitHub"} in this browser?`)) signOut.mutate();
  };
}

function Menu({ look, onClose }: { look: PickerLook; onClose: () => void }) {
  const navigate = useNavigate();
  const machines = useMachines(true);
  const remove = useRemoveMachine();
  const askSignOut = useAskSignOut();
  const list = machines.data && "list" in machines.data ? machines.data.list : [];
  const current = pickedMachine()?.label;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [onClose]);

  const choose = (m: Machine) => {
    onClose();
    if (m.label === current) return;
    pick(m);
    void navigate({ to: "/inbox" });
  };
  const askRemove = (m: Machine) => {
    if (confirm(RELAY.removeQuestion(m.name))) remove.mutate(m);
  };
  return (
    <>
      <div className="pkscrim" onClick={onClose} />
      <div className={`pkmenu pk-${look}`} role="menu" aria-label="Computers">
        {list.map((m) => (
          // The pick and Remove side by side: a button cannot hold another.
          <div className="pkrow" key={m.label}>
            <button type="button" role="menuitem" className="pkpick" onClick={() => choose(m)}>
              <Icon name="monitor" />
              <span className="pkbody">
                <span className="pkname">{m.name}</span>
                <span className="pksub">
                  {kindOf(m)} · {seen(m)}
                </span>
              </span>
              {m.label === current ? <Icon name="check" /> : null}
            </button>
            <button type="button" role="menuitem" className="pkremove" aria-label={`Remove ${m.name}`} onClick={() => askRemove(m)}>
              Remove
            </button>
          </div>
        ))}
        {machines.isPending ? <div className="pkhint">Loading…</div> : null}
        <div className="pksep" />
        <button type="button" role="menuitem" className="pkout" onClick={askSignOut}>
          Sign out
        </button>
      </div>
    </>
  );
}

export function ComputerPicker({ look }: { look: PickerLook }) {
  const connected = useLink((s) => s.connected);
  const host = useLink((s) => s.host);
  const pcOffline = useLink((s) => s.pcOffline);
  const [open, setOpen] = useState(false);
  const account = mode() === "account";
  const machine = account ? pickedMachine() : null;
  const name = machine?.name || host || location.hostname;
  const state = connected ? "online" : pcOffline ? "away" : "reconnecting";
  const dot = <span className={`dot ${connected ? "ok" : "bad"}`} />;

  let inner;
  if (look === "icon") {
    inner = (
      <>
        <Icon name="monitor" size={20} />
        {dot}
      </>
    );
  } else {
    inner = (
      <>
        {look === "card" ? <Icon name="monitor" size={20} /> : dot}
        <span className="pkbody">
          <span className="pkname">{name}</span>
          {look === "card" ? (
            <span className="pksub">
              {account ? kindOf(machine) : "This computer"} · {state}
            </span>
          ) : null}
        </span>
        {look === "card" ? dot : null}
        {account ? <Icon name="unfold-more" size={14} /> : null}
      </>
    );
  }
  // The rail's picker has no words of its own: say them.
  const label = look === "icon" ? `${name}, ${state}` : undefined;
  if (!account) {
    return (
      <div className={`picker pk-${look}`} title={label} aria-label={label} role={label ? "img" : undefined}>
        {inner}
      </div>
    );
  }
  return (
    <div className={`pickwrap pk-${look}`}>
      <button type="button" className={`picker pk-${look}`} aria-label={label} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}>
        {inner}
      </button>
      {open ? <Menu look={look} onClose={() => setOpen(false)} /> : null}
    </div>
  );
}
