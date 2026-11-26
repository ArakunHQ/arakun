// Signing up for the phone app betas from this page: a card above the inbox,
// which goes away for good once closed, and a group in Settings. Both ask for
// the email the store invites, since the operator admits each sign-up by hand
// in the admin dashboard and adds that address.

import { useQuery } from "@tanstack/react-query";
import { useEffect, useReducer, useState } from "react";
import { meQuery } from "../lib/account";
import { ACCOUNT, BETA_FLAG, betaLine, canJoin, HIDE_BETA, NAME, type Platform, PLATFORMS, type Signups, thisPlatform, useBeta, useJoinBeta } from "../lib/beta";
import { Icon } from "../lib/icons";
import { mode } from "../lib/start";
import { toast } from "../lib/toast";
import "./SetupCards.css";

/** The betas, once signed in on the account host with the flag; null anywhere else. */
function useSignups(): Signups | null {
  const account = mode() === "account";
  const boot = useQuery({ ...meQuery, enabled: account });
  const flags = boot.data && boot.data.screen !== "signin" ? boot.data.me?.flags : undefined;
  const flagged = account && Array.isArray(flags) && flags.includes(BETA_FLAG);
  return useBeta(flagged).data ?? null;
}

/** Whose address to ask for: this phone's store, or both on a computer's browser. */
function asks(platforms: Platform[]): string {
  if (platforms.length === 1) return `your ${ACCOUNT[platforms[0]!]}`;
  return `your ${ACCOUNT.ios} for iOS, your ${ACCOUNT.android} for Android`;
}

/** The email field and a Join button per platform still open. */
export function BetaForm({ signups, platforms }: { signups: Signups; platforms: Platform[] }) {
  const [email, setEmail] = useState(signups.email ?? "");
  const join = useJoinBeta();
  // The account's email arrives after the first draw on a cold start.
  useEffect(() => {
    if (!email && signups.email) setEmail(signups.email);
    // Only when the account's email changes: what the person typed stays.
  }, [signups.email]);
  const go = (platform: Platform) =>
    join.mutate(
      { platform, email: email.trim() },
      {
        onSuccess: (s) => toast(`Signed up. The invite will go to ${s.email}`),
      },
    );
  return (
    <form
      className="beta-form"
      autoComplete="on"
      onSubmit={(e) => {
        e.preventDefault();
        if (platforms.length === 1) go(platforms[0]!);
      }}
    >
      <input type="email" inputMode="email" autoComplete="email" placeholder="you@example.com" aria-label="Email for the invite" value={email} disabled={join.isPending} onChange={(e) => setEmail(e.target.value)} />
      <div className="beta-buttons">
        {platforms.map((p) => (
          <button key={p} type="button" className="btn outline small" disabled={join.isPending} onClick={() => go(p)}>
            {join.isPending && join.variables?.platform === p ? "Signing up…" : platforms.length === 1 ? "Join the beta" : `Join on ${NAME[p]}`}
          </button>
        ))}
      </div>
      {join.isError ? <div className="beta-err">{join.error.message}</div> : null}
    </form>
  );
}

/** The card above the inbox: for this phone's store, until signed up or closed. */
export function BetaCard() {
  const signups = useSignups();
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  if (!signups || localStorage.getItem(HIDE_BETA)) return null;
  const mine = thisPlatform();
  const platforms = (mine ? [mine] : PLATFORMS).filter((p) => canJoin(signups[p]));
  // Signed up for this phone's store, or for either from a computer: nothing to ask.
  if (platforms.length === 0 || (!mine && PLATFORMS.some((p) => !canJoin(signups[p])))) return null;
  const what = mine ? `Arakun for ${NAME[mine]}` : "Arakun for iOS and Android";
  return (
    <div className="setup beta" data-setup="beta">
      <span className="sicon">
        <Icon name="phone" />
      </span>
      <span className="body">
        <div className="t">Try the Arakun app</div>
        <div className="s">
          {what} is in beta. The invite comes by email: {asks(platforms)}.
        </div>
        <BetaForm signups={signups} platforms={platforms} />
      </span>
      <button
        className="x"
        aria-label="Not now"
        onClick={() => {
          localStorage.setItem(HIDE_BETA, "1");
          redraw();
        }}
      >
        <Icon name="close" />
      </button>
    </div>
  );
}

/** The group in Settings: where each beta stands, and the form while one is open. */
export function BetaGroup() {
  const signups = useSignups();
  if (!signups) return null;
  const open = PLATFORMS.filter((p) => canJoin(signups[p]));
  return (
    <section className="group">
      <div className="glabel">App beta</div>
      <div className="gbox">
        {PLATFORMS.map((p) => (
          <div key={p} className="prow">
            <span className="ricon">
              <Icon name="phone" size={20} />
            </span>
            <span className="rbody">
              <span className="rt">{NAME[p]}</span>
              <span className="rs">{betaLine(p, signups[p])}</span>
            </span>
          </div>
        ))}
        {open.length ? (
          <div className="prow beta-row">
            <BetaForm signups={signups} platforms={open} />
          </div>
        ) : null}
      </div>
    </section>
  );
}
