import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Confirm } from "./Confirm";
import { Time } from "./Time";
import { plansQuery, useAddFlag, useBlock, useCloseSessions, useRemoveFlag, useSetPlan, useUnblock } from "../queries";

const FLAG = /^[a-z][a-z0-9_]{0,31}$/;
const MAX_REASON = 500;

/// The server's error text for a failed call, next to the control that made it.
export function Failure({ error }: { error: Error | null }) {
  return error ? (
    <p className="message" data-tone="error" role="alert">
      {error.message}
    </p>
  ) : null;
}

export function Flags({ id, flags }: { id: number; flags: string[] }) {
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

export function Plan({ id, plan }: { id: number; plan: string | null }) {
  const plans = useQuery(plansQuery);
  const set = useSetPlan();
  const [chosen, setChosen] = useState<string | null>(null);
  // The plan the server holds is the select's value until the operator picks.
  // An account with no row has none: the operator must choose one.
  const value = chosen ?? plan ?? "";
  const names = plans.data?.plans ?? [];
  const options = value === "" || names.includes(value) ? names : [value, ...names];
  return (
    <section className="section">
      <h2>Plan</h2>
      <div className="form-inline">
        <select className="input" aria-label="Plan" value={value} disabled={plans.isPending || set.isPending} onChange={(e) => setChosen(e.target.value)}>
          {value === "" && (
            <option value="" disabled>
              Choose a plan
            </option>
          )}
          {options.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="button"
          disabled={set.isPending || plans.isPending || value === "" || value === plan}
          onClick={() => set.mutate({ githubId: id, plan: value }, { onSuccess: () => setChosen(null) })}
        >
          Set plan
        </button>
      </div>
      <Failure error={plans.error ?? set.error} />
    </section>
  );
}

export function Block({ id, blockedAt, reason }: { id: number; blockedAt: number | null; reason: string | null }) {
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
            body={<p>Its computers and devices are disconnected now and cannot connect until it is unblocked. It stays signed in: use Sign out everywhere to end that too.</p>}
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

export function SignOut({ id }: { id: number }) {
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
