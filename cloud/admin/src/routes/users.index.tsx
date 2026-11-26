import { useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { useCallback } from "react";
import { Time } from "../components/Time";
import { parseId } from "../format";
import { usersQuery } from "../queries";
import { useSearchBox } from "../useSearchBox";
import type { UserRow } from "../types";
import { Avatar } from "../components/Avatar";

interface UsersSearch {
  q?: string;
  /// The cursor of the page; none for the first.
  after?: string;
}

/// A typed-in URL like ?q=1099 reaches us parsed as a number.
const text = (v: unknown): string | undefined => {
  if (typeof v === "number" && Number.isSafeInteger(v)) return String(v);
  return typeof v === "string" && v !== "" ? v : undefined;
};

export const Route = createFileRoute("/users/")({
  validateSearch: (search: Record<string, unknown>): UsersSearch => ({ q: text(search.q), after: text(search.after) }),
  component: Users,
});

function Users() {
  const { q, after } = Route.useSearch();
  const navigate = Route.useNavigate();
  // A new search starts at the first page: a cursor belongs to the search that made it.
  const [typed, setTyped] = useSearchBox(
    q,
    useCallback((value) => void navigate({ search: { q: value }, replace: true }), [navigate]),
  );

  const users = useQuery(usersQuery({ q, after }));
  const id = q === undefined ? null : parseId(q);
  return (
    <>
      <h1>Users</h1>
      <div className="toolbar">
        <input
          className="input"
          type="search"
          aria-label="Search users"
          placeholder="Search by login, account ID or GitHub ID"
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
        />
        {id !== null && (
          <Link className="button" to="/users/$accountId" params={{ accountId: String(id) }}>
            Open account {id}
          </Link>
        )}
      </div>
      {users.isPending ? (
        <p className="muted">Loading users…</p>
      ) : users.isError ? (
        <p className="message" data-tone="error" role="alert">
          {users.error.message}
        </p>
      ) : users.data.users.length === 0 ? (
        <p className="muted">{q === undefined ? "No account has signed in yet." : "No account matches."}</p>
      ) : (
        <ul className="rows">
          {users.data.users.map((u) => (
            <UserItem key={u.account_id} user={u} />
          ))}
        </ul>
      )}
      <nav className="pager" aria-label="Pages">
        {after !== undefined && (
          <Link className="button" to="/users" search={{ q }}>
            First page
          </Link>
        )}
        {users.data?.next != null && (
          <Link className="button" to="/users" search={{ q, after: users.data.next }}>
            Next
          </Link>
        )}
      </nav>
    </>
  );
}

function UserItem({ user: u }: { user: UserRow }) {
  return (
    <li className="row" aria-label={u.login || `Account ${u.account_id}`}>
      <Avatar githubId={u.github_id} size={64} />
      <Link className="request-login" to="/users/$accountId" params={{ accountId: String(u.account_id) }}>
        {u.login || `Account ${u.account_id}`}
      </Link>
      <span className="chip">{u.plan}</span>
      {u.blocked_at !== null && <span className="chip" data-tone="error">blocked</span>}
      {u.flags.map((f) => (
        <span key={f} className="chip">
          {f}
        </span>
      ))}
      <span className="muted row-meta">
        {u.machines} {u.machines === 1 ? "machine" : "machines"} · seen <Time at={u.last_seen_at} />
      </span>
    </li>
  );
}
