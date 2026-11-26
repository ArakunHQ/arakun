import { useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { Confirm } from "../components/Confirm";
import { NoticeForm } from "../components/NoticeForm";
import { Time } from "../components/Time";
import { noticesQuery, useDeleteNotice } from "../queries";
import type { NoticeRow } from "../types";

export const Route = createFileRoute("/notices")({
  component: Notices,
});

function Notices() {
  const notices = useQuery(noticesQuery);
  return (
    <>
      <h1>Notices</h1>
      {notices.isPending ? (
        <p className="muted">Loading notices…</p>
      ) : notices.isError ? (
        <p className="message" data-tone="error" role="alert">
          {notices.error.message}
        </p>
      ) : notices.data.length === 0 ? (
        <p className="muted">No notice has been sent.</p>
      ) : (
        <ul className="rows">
          {notices.data.map((n) => (
            <Notice key={n.id} notice={n} />
          ))}
        </ul>
      )}
      <section className="section">
        <h2>Send a notice</h2>
        <NoticeForm />
      </section>
    </>
  );
}

function Notice({ notice: n }: { notice: NoticeRow }) {
  // Per row, so a failure shows next to the notice it is about and only this row waits.
  const take = useDeleteNotice();
  const ended = n.expires_at !== null && n.expires_at <= Date.now() / 1000;
  return (
    <li className="row" aria-label={n.text}>
      <span className="reason">{n.text}</span>
      {n.account_id === null ? (
        <span className="chip">Everyone</span>
      ) : (
        <Link className="request-login" to="/users/$accountId" params={{ accountId: String(n.account_id) }}>
          {n.login || `Account ${n.account_id}`}
        </Link>
      )}
      {n.url !== null && n.url.startsWith("https://") && (
        <a href={n.url} rel="noreferrer">
          {n.url}
        </a>
      )}
      <span className="muted row-meta">
        sent <Time at={n.created_at} />
        {n.expires_at !== null && (
          <>
            {" "}· {ended ? "ended" : "ends"} <Time at={n.expires_at} />
          </>
        )}{" "}
        · read by {n.reads}
      </span>
      <Confirm
        title="Take back this notice?"
        body={<p>Nobody will see it again.</p>}
        confirmLabel="Take back"
        danger
        disabled={take.isPending}
        onConfirm={() => take.mutate(n.id)}
      >
        Take back
      </Confirm>
      {take.error && (
        <p className="message" data-tone="error" role="alert">
          {take.error.message}
        </p>
      )}
    </li>
  );
}
