import { useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { Confirm } from "../components/Confirm";
import { Time } from "../components/Time";
import { age, githubUrl, platformName } from "../format";
import { signupsQuery, useDecideSignup, type SignupStatus } from "../queries";
import type { BetaPlatform, BetaSignup } from "../types";

interface BetaSearch {
  /// Pending when it is left out.
  status?: SignupStatus;
  /// Both when it is left out.
  platform?: BetaPlatform;
}

const TABS: { status: SignupStatus; label: string; empty: string }[] = [
  { status: "pending", label: "Pending", empty: "No sign-ups are pending." },
  { status: "admitted", label: "Admitted", empty: "No one has been admitted." },
  { status: "declined", label: "Declined", empty: "No sign-up has been declined." },
];

const PLATFORMS: { platform: BetaPlatform | undefined; label: string }[] = [
  { platform: undefined, label: "Both" },
  { platform: "ios", label: "iOS" },
  { platform: "android", label: "Android" },
];

/// Where the operator adds a tester, by platform.
const STORE: Record<BetaPlatform, string> = {
  ios: "App Store Connect (TestFlight)",
  android: "Play Console (the testing track's testers)",
};

export const Route = createFileRoute("/beta")({
  validateSearch: (search: Record<string, unknown>): BetaSearch => ({
    status: search.status === "admitted" || search.status === "declined" ? search.status : undefined,
    platform: search.platform === "ios" || search.platform === "android" ? search.platform : undefined,
  }),
  component: Betas,
});

function Betas() {
  const { status = "pending", platform } = Route.useSearch();
  const tab = TABS.find((t) => t.status === status)!;
  const signups = useQuery(signupsQuery({ status, platform }));
  const searchFor = (s: SignupStatus, p: BetaPlatform | undefined): BetaSearch => ({
    ...(s === "pending" ? {} : { status: s }),
    ...(p ? { platform: p } : {}),
  });
  return (
    <>
      <h1>Phone betas</h1>
      <p className="muted">
        Admitting invites no one. Add the email as a tester in the store first, then admit, so the app tells the person the invite is on
        its way.
      </p>
      <nav className="tabs" aria-label="Sign-up status">
        {TABS.map((t) => (
          <Link
            key={t.status}
            to="/beta"
            search={searchFor(t.status, platform)}
            className="tab"
            // The default tab has no search param, so match by hand.
            data-active={t.status === status || undefined}
            aria-current={t.status === status ? "page" : undefined}
          >
            {t.label}
          </Link>
        ))}
      </nav>
      <nav className="tabs" aria-label="Platform">
        {PLATFORMS.map((p) => (
          <Link
            key={p.label}
            to="/beta"
            search={searchFor(status, p.platform)}
            className="tab"
            data-active={p.platform === platform || undefined}
            aria-current={p.platform === platform ? "page" : undefined}
          >
            {p.label}
          </Link>
        ))}
      </nav>
      {signups.isPending ? (
        <p className="muted">Loading sign-ups…</p>
      ) : signups.isError ? (
        <p className="message" data-tone="error" role="alert">
          {signups.error.message}
        </p>
      ) : signups.data.length === 0 ? (
        <p className="muted">{tab.empty}</p>
      ) : (
        <>
          {platform && <CopyEmails emails={signups.data.map((s) => s.email)} />}
          <ul className="requests">
            {signups.data.map((s) => (
              <Signup key={`${s.github_id}-${s.platform}`} signup={s} />
            ))}
          </ul>
        </>
      )}
    </>
  );
}

/// The list's emails, one a line, to paste into the store's testers in one go.
function CopyEmails({ emails }: { emails: string[] }) {
  const [done, setDone] = useState<"copied" | "failed" | null>(null);
  return (
    <div className="toolbar">
      <button
        type="button"
        className="button"
        onClick={() => {
          navigator.clipboard.writeText(emails.join("\n")).then(
            () => setDone("copied"),
            () => setDone("failed"),
          );
        }}
      >
        {emails.length === 1 ? "Copy 1 email" : `Copy ${emails.length} emails`}
      </button>
      {done === "copied" && <span className="muted">Copied</span>}
      {done === "failed" && (
        <p className="message" data-tone="error" role="alert">
          The browser did not let the panel copy.
        </p>
      )}
    </div>
  );
}

function Signal({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="signal">
      <dt>{label}</dt>
      <dd>{value ?? <span className="muted">not known</span>}</dd>
    </div>
  );
}

function number(n: number | null): string | null {
  return n === null ? null : String(n);
}

function Signup({ signup: s }: { signup: BetaSignup }) {
  const decide = useDecideSignup();
  const { github } = s;
  const url = githubUrl(s.login);
  const name = s.login || `GitHub ID ${s.github_id}`;
  const which = platformName(s.platform);
  const act = (decision: "admit" | "decline") => decide.mutate({ githubId: s.github_id, platform: s.platform, decision });
  return (
    <li className="request" aria-label={`${s.login} ${which}`}>
      <div className="request-head">
        <img className="avatar" src={`https://avatars.githubusercontent.com/u/${s.github_id}?s=64`} alt="" />
        {url ? (
          <a className="request-login" href={url} rel="noreferrer">
            {s.login}
          </a>
        ) : (
          <span className="request-login">{name}</span>
        )}
        <span className="chip">{which}</span>
        {s.blocked && <span className="chip">blocked</span>}
        <Link className="request-account" to="/users/$githubId" params={{ githubId: String(s.github_id) }}>
          Account
        </Link>
      </div>
      <p className="mono">{s.email}</p>
      <dl className="signals">
        <Signal label="GitHub account age" value={github.age_days === null ? null : age(0, github.age_days * 86400)} />
        <Signal label="Public repos" value={number(github.public_repos)} />
        <Signal label="Followers" value={number(github.followers)} />
        <Signal label="Following" value={number(github.following)} />
      </dl>
      <p className="muted request-when">
        Signed up <Time at={s.requested_at} />
        {s.status !== "pending" && s.decided_at !== null && (
          <>
            {" "}
            · {s.status} {s.decided_by ? `by ${s.decided_by} ` : ""}
            <Time at={s.decided_at} />
          </>
        )}
      </p>
      <div className="request-actions">
        {s.status !== "admitted" && (
          <Confirm
            title={`Admit ${name} to the ${which} beta?`}
            body={
              <p>
                Add <span className="mono">{s.email}</span> as a tester in {STORE[s.platform]} first. Admitting only tells the app the invite is
                on its way.
              </p>
            }
            confirmLabel="Admit"
            disabled={decide.isPending}
            onConfirm={() => act("admit")}
          >
            Admit
          </Confirm>
        )}
        {s.status !== "declined" && (
          <Confirm
            title={s.status === "admitted" ? `Remove ${name} from the ${which} beta?` : `Decline ${name}?`}
            body={
              s.status === "admitted" ? (
                <p>
                  Remove <span className="mono">{s.email}</span> from the testers in {STORE[s.platform]} too. They can sign up again in 30
                  days.
                </p>
              ) : (
                <p>They can sign up again in 30 days.</p>
              )
            }
            confirmLabel={s.status === "admitted" ? "Remove" : "Decline"}
            danger
            disabled={decide.isPending}
            onConfirm={() => act("decline")}
          >
            {s.status === "admitted" ? "Remove" : "Decline"}
          </Confirm>
        )}
        {decide.error && (
          <p className="message" data-tone="error" role="alert">
            {decide.error.message}
          </p>
        )}
      </div>
    </li>
  );
}
