import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError } from "../api";
import { ActivityList } from "../components/ActivityList";
import { Confirm } from "../components/Confirm";
import { NoticeForm } from "../components/NoticeForm";
import { Block, Failure, Flags, Plan, SignOut } from "../components/UserControls";
import { Time } from "../components/Time";
import { githubUrl, parseGithubId } from "../format";
import {
  plansQuery,
  useAddFlag,
  useBlock,
  useCloseSessions,
  useRemoveFlag,
  useSetPlan,
  useUnblock,
  userQuery,
} from "../queries";
import type { NoticeRow, UserDetail } from "../types";

export const Route = createFileRoute("/users/$githubId")({
  component: UserRoute,
});

function UserRoute() {
  const { githubId } = Route.useParams();
  const id = parseGithubId(githubId);
  if (id === null) {
    return (
      <>
        <h1>User</h1>
        <p className="message" data-tone="error" role="alert">
          This is not a GitHub ID.
        </p>
      </>
    );
  }
  // Keyed, so what was typed for one account never carries over to the next.
  return <User key={id} id={id} />;
}

function Name({ id, login }: { id: number; login: string }) {
  const url = githubUrl(login);
  // An account set up before its first sign-in has no login.
  return url ? (
    <a href={url} rel="noreferrer">
      {login}
    </a>
  ) : (
    <>GitHub ID {id}</>
  );
}

function User({ id }: { id: number }) {
  const user = useQuery(userQuery(id));
  if (user.isPending) return <p className="muted">Loading the account…</p>;
  if (user.isError) {
    if (user.error instanceof ApiError && user.error.status === 404) return <Unknown id={id} />;
    return (
      <>
        <h1>User</h1>
        <Failure error={user.error} />
      </>
    );
  }
  return <Known id={id} detail={user.data} />;
}

function Unknown({ id }: { id: number }) {
  return (
    <>
      <h1>GitHub ID {id}</h1>
      <p className="muted">This account has not signed in yet.</p>
      <p className="muted">What you set here is kept for when it does.</p>
      <Flags id={id} flags={[]} />
      <Plan id={id} plan={null} />
      <Block id={id} blockedAt={null} reason={null} />
    </>
  );
}

function Known({ id, detail }: { id: number; detail: UserDetail }) {
  const { user, sessions, machines, cloud_request: request, notices } = detail;
  return (
    <>
      <div className="page-head">
        <img className="avatar" src={`https://avatars.githubusercontent.com/u/${id}?s=64`} alt="" />
        <h1>
          <Name id={id} login={user.login} />
        </h1>
        {user.blocked_at !== null && (
          <span className="chip" data-tone="error">
            blocked
          </span>
        )}
      </div>
      <dl className="signals">
        <div className="signal">
          <dt>GitHub ID</dt>
          <dd>{id}</dd>
        </div>
        <div className="signal">
          <dt>Email</dt>
          <dd>{user.email ?? <span className="muted">not known</span>}</dd>
        </div>
        <div className="signal">
          <dt>First signed in</dt>
          <dd>
            <Time at={user.created_at} />
          </dd>
        </div>
        <div className="signal">
          <dt>Last seen</dt>
          <dd>
            <Time at={user.last_seen_at} />
          </dd>
        </div>
      </dl>
      <Flags id={id} flags={user.flags} />
      <Plan id={id} plan={user.plan} />
      <Block id={id} blockedAt={user.blocked_at} reason={user.blocked_reason} />

      <section className="section">
        <h2>Signed-in devices</h2>
        {sessions.length === 0 ? <p className="muted">No device is signed in.</p> : (
          <ul className="rows">
            {sessions.map((s) => (
              <li key={s.id} className="row">
                <span className="request-login">{s.name}</span>
                <span className="chip">{s.kind}</span>
                {s.machine_label && <span className="chip">{s.machine_label}</span>}
                <span className="muted row-meta">
                  used <Time at={s.last_used_at} /> · ends <Time at={s.expires_at} />
                </span>
              </li>
            ))}
          </ul>
        )}
        <SignOut id={id} />
      </section>

      <section className="section">
        <h2>Machines</h2>
        {machines.length === 0 ? <p className="muted">No machine is registered.</p> : (
          <ul className="rows">
            {machines.map((m) => (
              <li key={m.label} className="row">
                <span className="request-login">{m.name}</span>
                <span className="chip">{m.kind}</span>
                <span className="muted row-meta">
                  {m.label} · seen <Time at={m.last_seen_at} />
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {request && (
        <section className="section">
          <h2>Cloud access request</h2>
          <p>
            {request.status}, asked <Time at={request.requested_at} />
            {request.decided_by ? ` · by ${request.decided_by}` : ""}
          </p>
          {request.reason ? <p className="reason">{request.reason}</p> : <p className="muted">No reason given</p>}
        </section>
      )}

      <section className="section">
        <h2>Notices</h2>
        {notices.length === 0 ? <p className="muted">No notice for this account.</p> : (
          <ul className="rows">
            {notices.map((n) => (
              <Notice key={n.id} notice={n} />
            ))}
          </ul>
        )}
        <NoticeForm githubId={id} />
      </section>

      <section className="section">
        <h2>Activity</h2>
        <ActivityList githubId={id} />
      </section>
    </>
  );
}

function Notice({ notice: n }: { notice: NoticeRow }) {
  return (
    <li className="row">
      <span className="reason">{n.text}</span>
      {n.url !== null && n.url.startsWith("https://") && (
        <a href={n.url} rel="noreferrer">
          {n.url}
        </a>
      )}
      <span className="muted row-meta">
        sent <Time at={n.created_at} />
        {n.expires_at !== null && (
          <>
            {" "}· ends <Time at={n.expires_at} />
          </>
        )}{" "}
        · read by {n.reads}
      </span>
    </li>
  );
}
