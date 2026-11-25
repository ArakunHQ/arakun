import { useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { useCallback } from "react";
import { Time } from "../components/Time";
import { machinesQuery } from "../queries";
import type { MachineRow } from "../types";
import { useSearchBox } from "../useSearchBox";

interface MachinesSearch {
  q?: string;
}

export const Route = createFileRoute("/machines")({
  validateSearch: (search: Record<string, unknown>): MachinesSearch => {
    const q = search.q;
    if (typeof q === "number" && Number.isSafeInteger(q)) return { q: String(q) };
    return { q: typeof q === "string" && q !== "" ? q : undefined };
  },
  component: Machines,
});

function matches(m: MachineRow, needle: string): boolean {
  return [m.login, m.name, m.label].some((field) => field.toLowerCase().includes(needle));
}

function Machines() {
  const { q } = Route.useSearch();
  const navigate = Route.useNavigate();
  const [typed, setTyped] = useSearchBox(
    q,
    useCallback((value) => void navigate({ search: { q: value }, replace: true }), [navigate]),
  );
  const machines = useQuery(machinesQuery);
  const needle = q?.toLowerCase();
  const shown = machines.data?.filter((m) => needle === undefined || matches(m, needle));
  return (
    <>
      <h1>Machines</h1>
      <div className="toolbar">
        <input
          className="input"
          type="search"
          aria-label="Filter machines"
          placeholder="Filter by login, name or label"
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
        />
      </div>
      {machines.isPending ? (
        <p className="muted">Loading machines…</p>
      ) : machines.isError ? (
        <p className="message" data-tone="error" role="alert">
          {machines.error.message}
        </p>
      ) : shown!.length === 0 ? (
        <p className="muted">{q === undefined ? "No machine has registered yet." : "No machine matches."}</p>
      ) : (
        <ul className="rows">
          {shown!.map((m) => (
            <li key={m.label} className="row" aria-label={m.label}>
              <span className="reason">{m.name}</span>
              <span className="chip">{m.kind}</span>
              <Link className="request-login" to="/users/$githubId" params={{ githubId: String(m.github_id) }}>
                {m.login || `GitHub ID ${m.github_id}`}
              </Link>
              <span className="muted row-meta">
                {m.label} · registered <Time at={m.created_at} /> · seen <Time at={m.last_seen_at} />
              </span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
