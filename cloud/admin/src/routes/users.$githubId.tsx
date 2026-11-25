import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError } from "../api";
import { ActivityList } from "../components/ActivityList";
import { Confirm } from "../components/Confirm";
import { NoticeForm } from "../components/NoticeForm";
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

const FLAG = /^[a-z][a-z0-9_]{0,31}$/;
const MAX_REASON = 500;

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
  return <User id={id} />;
}

function Failure({ error }: { error: Error | null }) {
  return error ? (
    <p className="message" data-tone="error" role="alert">
      {error.message}
    </p>
  ) : null;
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

function Flags({ id, flags }: { id: number; flags: string[] }) {
  const add = useAddFlag();
  const remove = useRemoveFlag();
  const [flag, setFlag] = useState("");
  const [invalid, setInvalid] = useState(false);
  const busy = add.isPending || remove.isPending;
  // One error at a time: the one of the call that ran last.
  const error = add.error ?? remove.error;
  return (
    <section className="section">
      <h2>Flags</h2>
      {flags.length === 0 ? (
        <p className="muted">No flags.</p>
      ) : (
        <ul className="chips">
          {flags.map((f) => (
            <li key={f} className="chip" aria-label={f}>
              {f}
              <Confirm
                title={`Take away the flag ${f}?`}
                body={<p>The account loses what the flag gives it.</p>}
                confirmLabel="Take away"
                danger
                disabled={busy}
                onConfirm={() => {
                  add.reset();
                  remove.mutate({ githubId: id, flag: f });
                }}
              >
                Remove
              </Confirm>
            </li>
          ))}
        </ul>
      )}
      <form
        className="form-inline"
        onSubmit={(e) => {
          e.preventDefault();
          if (!FLAG.test(flag)) {
            setInvalid(true);
            return;
          }
          setInvalid(false);
          remove.reset();
          add.mutate({ githubId: id, flag }, { onSuccess: () => setFlag("") });
        }}
      >
        <input
          className="input"
          aria-label="New flag"
          placeholder="flag_name"
          value={flag}
          onChange={(e) => {
            setFlag(e.target.value);
            setInvalid(false);
          }}
        />
        <button type="submit" className="button" disabled={busy}>
          Add flag
        </button>
      </form>
      {invalid && (
        <p className="message" data-tone="error" role="alert">
          A flag starts with a lower case letter and has up to 32 letters, digits or underscores.
        </p>
      )}
      <Failure error={error} />
    </section>
  );
}

function Plan({ id, plan }: { id: number; plan: string | null }) {
  const plans = useQuery(plansQuery);
  const set = useSetPlan();
  const [chosen, setChosen] = useState<string | null>(null);
  // The plan the server holds is the select's value until the operator picks.
  const value = chosen ?? plan ?? "free";
  const names = plans.data?.plans ?? [];
  const options = names.includes(value) ? names : [value, ...names];
  return (
    <section className="section">
      <h2>Plan</h2>
      <div className="form-inline">
        <select className="input" aria-label="Plan" value={value} disabled={plans.isPending || set.isPending} onChange={(e) => setChosen(e.target.value)}>
          {options.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="button"
          disabled={set.isPending || plans.isPending || value === plan}
          onClick={() => set.mutate({ githubId: id, plan: value }, { onSuccess: () => setChosen(null) })}
        >
          Set plan
        </button>
      </div>
      <Failure error={plans.error ?? set.error} />
    </section>
  );
}

function Block({ id, blockedAt, reason }: { id: number; blockedAt: number | null; reason: string | null }) {
  const block = useBlock();
  const unblock = useUnblock();
  const [why, setWhy] = useState("");
  const busy = block.isPending || unblock.isPending;
  const error = block.error ?? unblock.error;
  return (
    <section className="section">
      <h2>Block</h2>
      {blockedAt !== null ? (
        <>
          <p>
            Blocked <Time at={blockedAt} />
            {reason ? `: ${reason}` : ""}
          </p>
          <button
            type="button"
            className="button"
            disabled={busy}
            onClick={() => {
              block.reset();
              unblock.mutate(id);
            }}
          >
            Unblock
          </button>
        </>
      ) : (
        <div className="form-inline">
          <input
            className="input"
            aria-label="Reason"
            placeholder="Reason (optional)"
            maxLength={MAX_REASON}
            value={why}
            onChange={(e) => setWhy(e.target.value)}
          />
          <Confirm
            title={`Block GitHub ID ${id}?`}
            body={<p>The account is signed out and cannot sign in until it is unblocked.</p>}
            confirmLabel="Block"
            danger
            disabled={busy || why.length > MAX_REASON}
            onConfirm={() => {
              unblock.reset();
              block.mutate({ githubId: id, reason: why.trim() || undefined });
            }}
          >
            Block
          </Confirm>
        </div>
      )}
      <Failure error={error} />
    </section>
  );
}

function SignOut({ id }: { id: number }) {
  const close = useCloseSessions();
  return (
    <div className="form-inline">
      <Confirm
        title="Sign out everywhere?"
        body={<p>Every device of this account is signed out and has to sign in again.</p>}
        confirmLabel="Sign out"
        danger
        disabled={close.isPending}
        onConfirm={() => close.mutate(id)}
      >
        Sign out everywhere
      </Confirm>
      {close.data && (
        <span className="muted" role="status">
          Signed out {close.data.closed} {close.data.closed === 1 ? "device" : "devices"}.
        </span>
      )}
      <Failure error={close.error} />
    </div>
  );
}
