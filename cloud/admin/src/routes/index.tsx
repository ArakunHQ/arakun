// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { Confirm } from "../components/Confirm";
import { Time } from "../components/Time";
import { age, githubUrl } from "../format";
import { requestsQuery, useDecline, useGrant, type RequestStatus } from "../queries";
import type { CloudRequest } from "../types";
import { Avatar } from "../components/Avatar";

interface AccessSearch {
  /// Pending when it is left out.
  status?: RequestStatus;
}

const TABS: { status: RequestStatus; label: string; empty: string }[] = [
  { status: "pending", label: "Pending", empty: "No requests are pending." },
  { status: "granted", label: "Granted", empty: "No request has been granted." },
  { status: "declined", label: "Declined", empty: "No request has been declined." },
];

export const Route = createFileRoute("/")({
  validateSearch: (search: Record<string, unknown>): AccessSearch => ({
    status: search.status === "granted" || search.status === "declined" ? search.status : undefined,
  }),
  component: CloudAccess,
});

function CloudAccess() {
  const status = Route.useSearch({ select: (s) => s.status ?? "pending" });
  const tab = TABS.find((t) => t.status === status)!;
  const requests = useQuery(requestsQuery(status));
  return (
    <>
      <h1>Cloud access</h1>
      <nav className="tabs" aria-label="Request status">
        {TABS.map((t) => (
          <Link
            key={t.status}
            to="/"
            search={t.status === "pending" ? {} : { status: t.status }}
            className="tab"
            // The default tab has no search param, so match by hand.
            data-active={t.status === status || undefined}
            aria-current={t.status === status ? "page" : undefined}
          >
            {t.label}
          </Link>
        ))}
      </nav>
      {requests.isPending ? (
        <p className="muted">Loading requests…</p>
      ) : requests.isError ? (
        <p className="message" data-tone="error" role="alert">
          {requests.error.message}
        </p>
      ) : requests.data.length === 0 ? (
        <p className="muted">{tab.empty}</p>
      ) : (
        <ul className="requests">
          {requests.data.map((r) => (
            <Request key={r.account_id} request={r} />
          ))}
        </ul>
      )}
    </>
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

function Request({ request: r }: { request: CloudRequest }) {
  const grant = useGrant();
  const decline = useDecline();
  const busy = grant.isPending || decline.isPending;
  const error = grant.error ?? decline.error;
  const { github } = r;
  const url = githubUrl(r.login);
  return (
    <li className="request" aria-label={r.login}>
      <div className="request-head">
        <Avatar githubId={r.github_id} size={64} />
        {url ? (
          <a className="request-login" href={url} rel="noreferrer">
            {r.login}
          </a>
        ) : (
          // Set up by an operator before its first sign-in: no login yet.
          <span className="request-login">Account {r.account_id}</span>
        )}
        {r.blocked && <span className="chip">blocked</span>}
        {r.has_flag && <span className="chip">has cloud access</span>}
        <Link className="request-account" to="/users/$accountId" params={{ accountId: String(r.account_id) }}>
          Account
        </Link>
      </div>
      <dl className="signals">
        <Signal label="GitHub account age" value={github.age_days === null ? null : age(0, github.age_days * 86400)} />
        <Signal label="Public repos" value={number(github.public_repos)} />
        <Signal label="Followers" value={number(github.followers)} />
        <Signal label="Following" value={number(github.following)} />
      </dl>
      {r.reason === null || r.reason === "" ? <p className="muted">No reason given</p> : <p className="reason">{r.reason}</p>}
      <p className="muted request-when">
        Asked <Time at={r.requested_at} />
        {r.status !== "pending" && r.decided_at !== null && (
          <>
            {" "}
            · {r.status} {r.decided_by ? `by ${r.decided_by} ` : ""}
            <Time at={r.decided_at} />
          </>
        )}
      </p>
      {r.status !== "granted" && (
        <div className="request-actions">
          <button
            type="button"
            className="button primary"
            disabled={busy}
            onClick={() => {
              // One error at a time: the one of the call that ran last.
              decline.reset();
              grant.mutate(r.account_id);
            }}
          >
            Grant
          </button>
          {r.status === "pending" && (
            <Confirm
              title={`Decline ${r.login}?`}
              body={<p>They can ask again in 30 days.</p>}
              confirmLabel="Decline"
              danger
              disabled={busy}
              onConfirm={() => {
                grant.reset();
                decline.mutate(r.account_id);
              }}
            >
              Decline
            </Confirm>
          )}
          {error && (
            <p className="message" data-tone="error" role="alert">
              {error.message}
            </p>
          )}
        </div>
      )}
    </li>
  );
}
