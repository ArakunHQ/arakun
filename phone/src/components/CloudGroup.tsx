// The cloud environment on the Computers screen. An account with the `cloud`
// flag sees its environment, the hours left this month and why it stopped, and
// makes, wakes, stops or removes it here. Any other account sees the request
// for access instead, and where its request stands.

import { useState } from "react";
import {
  accessLine,
  canAsk,
  type CloudMachine,
  machineLine,
  MAX_REASON,
  type Usage,
  usageLine,
  useAskAccess,
  useCloudAccess,
  useCloudMachines,
  useCloudUsage,
  useMachineAction,
  useMakeMachine,
} from "../lib/cloud";
import { Icon } from "../lib/icons";
import { toast } from "../lib/toast";
import "./CloudGroup.css";

/** The request for access, with an optional reason for the reviewer. */
function AskForm() {
  const [reason, setReason] = useState("");
  const ask = useAskAccess();
  return (
    <form
      className="cloud-form"
      onSubmit={(e) => {
        e.preventDefault();
        ask.mutate(reason.trim(), { onSuccess: () => toast("Asked for access") });
      }}
    >
      <textarea
        rows={3}
        maxLength={MAX_REASON}
        placeholder="What would you use it for? (optional)"
        aria-label="What you would use it for"
        value={reason}
        disabled={ask.isPending}
        onChange={(e) => setReason(e.target.value)}
      />
      <div className="cloud-buttons">
        <button type="submit" className="btn outline small" disabled={ask.isPending}>
          {ask.isPending ? "Asking…" : "Ask for access"}
        </button>
      </div>
      {ask.isError ? <div className="cloud-err">{ask.error.message}</div> : null}
    </form>
  );
}

function MachineRow({ m, usage }: { m: CloudMachine; usage: Usage | null }) {
  const act = useMachineAction();
  const busy = act.isPending;
  const remove = () => {
    if (confirm(`Remove ${m.name}? Its files and sessions go with it.`)) act.mutate({ m, action: "remove" });
  };
  // A stop the operator or the allowance made is lifted only there: waking would be refused.
  const canWake = !m.running && (m.stopped === null || m.stopped === "task_cap");
  return (
    <div className="prow cloud-row">
      <span className="ricon">
        <Icon name="cloud" size={20} />
      </span>
      <span className="rbody">
        <span className="rt">{m.name}</span>
        <span className="rs">{machineLine(m, usage)}</span>
        {act.isError ? <span className="cloud-err">{act.error.message}</span> : null}
        <span className="cloud-buttons">
          {m.running ? (
            <button className="btn outline small" disabled={busy} onClick={() => act.mutate({ m, action: "sleep" })}>
              {busy && act.variables?.action === "sleep" ? "Stopping…" : "Stop"}
            </button>
          ) : canWake ? (
            <button className="btn outline small" disabled={busy} onClick={() => act.mutate({ m, action: "wake" })}>
              {busy && act.variables?.action === "wake" ? "Waking…" : "Wake"}
            </button>
          ) : null}
          <button className="btn ghost small" disabled={busy} aria-label={`Remove ${m.name}`} onClick={remove}>
            Remove
          </button>
        </span>
      </span>
    </div>
  );
}

/** The group, on the account host once signed in; `flagged` is whether the account has the `cloud` flag. */
export function CloudGroup({ flagged }: { flagged: boolean }) {
  const access = useCloudAccess(!flagged);
  const machines = useCloudMachines(flagged);
  const usage = useCloudUsage(flagged);
  const make = useMakeMachine();

  let rows;
  if (flagged) {
    // Not set up on this host: nothing to offer.
    if (machines.data == null) return null;
    const u = usage.data ?? null;
    rows = (
      <>
        {machines.data.map((m) => (
          <MachineRow key={m.label} m={m} usage={u} />
        ))}
        {machines.data.length === 0 ? (
          <div className="prow cloud-row">
            <span className="ricon">
              <Icon name="cloud" size={20} />
            </span>
            <span className="rbody">
              <span className="rt">New cloud environment</span>
              <span className="rs">Runs Arakun in the cloud, so sessions keep working with your computer off.</span>
              {make.isError ? <span className="cloud-err">{make.error.message}</span> : null}
              <span className="cloud-buttons">
                <button className="btn outline small" disabled={make.isPending} onClick={() => make.mutate()}>
                  {make.isPending ? "Making…" : "Make one"}
                </button>
              </span>
            </span>
          </div>
        ) : null}
        {u ? (
          <div className="prow">
            <span className="ricon">
              <Icon name="gauge" size={20} />
            </span>
            <span className="rbody">
              <span className="rt">{usageLine(u)}</span>
              {u.message ? <span className="rs">{u.message}</span> : null}
            </span>
          </div>
        ) : null}
      </>
    );
  } else {
    const a = access.data;
    if (!a) return null;
    rows = (
      <div className="prow cloud-row">
        <span className="ricon">
          <Icon name="cloud" size={20} />
        </span>
        <span className="rbody">
          <span className="rt">Cloud environments</span>
          <span className="rs">{accessLine(a)}</span>
          {canAsk(a) ? <AskForm /> : null}
        </span>
      </div>
    );
  }
  return (
    <section className="group">
      <div className="glabel">Cloud</div>
      <div className="gbox">{rows}</div>
    </section>
  );
}
