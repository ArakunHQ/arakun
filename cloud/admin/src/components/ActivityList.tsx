import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { platformName } from "../format";
import { logQuery } from "../queries";
import type { LogEntry } from "../types";
import { Time } from "./Time";

/// What an entry says the operator did, in words. An action the panel does
/// not know, or an entry without the detail its words need, is shown as the
/// action's name, so a newer Worker never shows a blank or "undefined".
function words({ action, detail }: LogEntry): string {
  const has = (key: string) => detail?.[key] !== undefined && detail[key] !== null;
  switch (action) {
    case "flag.grant":
      if (has("flag")) return `granted the flag ${detail!.flag}`;
      break;
    case "flag.revoke":
      if (has("flag")) return `took away the flag ${detail!.flag}`;
      break;
    case "sessions.close":
      if (has("closed")) return `signed out ${detail!.closed} devices`;
      break;
    case "block":
      return detail?.reason ? `blocked the account: ${detail.reason}` : "blocked the account";
    case "unblock":
      return "unblocked the account";
    case "plan.set":
      if (has("plan")) return `set the plan to ${detail!.plan}`;
      break;
    case "notice.create":
      return "sent a notice";
    case "notice.delete":
      return "took back a notice";
    case "cloud.grant":
      return "granted cloud access";
    case "cloud.decline":
      return "declined cloud access";
    case "beta.admit":
      if (has("platform")) return `admitted to the ${platformName(detail!.platform)} beta`;
      break;
    case "beta.decline":
      if (has("platform")) return `declined for the ${platformName(detail!.platform)} beta`;
      break;
    case "operator.totp.add":
      return "set up an authenticator app";
    case "operator.totp.replace":
      return "replaced the authenticator app";
    case "operator.totp.reset":
      return "reset the authenticator app";
  }
  return String(action);
}

function Entry({ entry }: { entry: LogEntry }) {
  return (
    <li className="log-entry">
      <Time at={entry.at} />
      <span className="log-who">{entry.operator_login ?? "the secret"}</span>
      <span className="log-what">{words(entry)}</span>
      {entry.account_id === null ? (
        // An operator's own change names no account when they have none.
        !entry.action.startsWith("operator.") && <span className="muted">everyone</span>
      ) : (
        <Link className="log-account" to="/users/$accountId" params={{ accountId: String(entry.account_id) }}>
          {entry.login ? entry.login : `Account ${entry.account_id}`}
        </Link>
      )}
    </li>
  );
}

/// One page of the log. Only the last page offers the next one.
function Page({ accountId, before, last, onMore }: { accountId?: number; before?: number; last: boolean; onMore: (next: number) => void }) {
  const log = useQuery(logQuery({ accountId, before }));
  if (log.isPending) return <p className="muted">Loading the log…</p>;
  if (log.isError) {
    return (
      <p className="message" data-tone="error" role="alert">
        {log.error.message}
      </p>
    );
  }
  const { entries, next } = log.data;
  return (
    <>
      {entries.length === 0 && before === undefined && <p className="muted">Nothing has been changed yet.</p>}
      {entries.length > 0 && (
        <ul className="log">
          {entries.map((e) => (
            <Entry key={e.id} entry={e} />
          ))}
        </ul>
      )}
      {last && next !== null && (
        <button type="button" className="button" onClick={() => onMore(next)}>
          Load more
        </button>
      )}
    </>
  );
}

/// The log, newest first: all of it, or the changes to one account.
export function ActivityList({ accountId }: { accountId?: number }) {
  // The `before` of every page loaded so far; the first has none.
  const [cursors, setCursors] = useState<(number | undefined)[]>([undefined]);
  return (
    <div className="activity">
      {cursors.map((before, i) => (
        <Page
          key={before ?? "first"}
          accountId={accountId}
          before={before}
          last={i === cursors.length - 1}
          onMore={(next) => setCursors((c) => [...c, next])}
        />
      ))}
    </div>
  );
}
