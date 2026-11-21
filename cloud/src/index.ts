import { DurableObject } from "cloudflare:workers";
import { HOUR, MONTH_LIMIT, reservation, STANDARD1_USD_HOUR_MAX, TASK_LIMIT, transition, workspaceTransition, type Action, type Phase } from "./budget.ts";
import { WORKSPACE_LIMIT_MS, workspaceStep } from "./workspace.ts";

interface Env {
  BETA: DurableObjectNamespace<BudgetMachine>;
  BETA_TOKEN?: string;
  // Only the loopback Miniflare harness binds this service.
  OFFLINE_BOX?: Fetcher;
  WORKSPACE_R2?: R2Bucket;
}
type Task = { id: string; phase: Phase; used: number; checkpoint: string | null; runs: number; limit_ms: number };
type Lease = { id: string; nonce: string; month: string; started: number; reserved: number };
const monthOf = (ms: number) => new Date(ms).toISOString().slice(0, 7);
const reply = (data: unknown, status = 200) => Response.json(data, { status, headers: { "cache-control": "no-store" } });
const sha = (s: string) => crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));

// One test account. No host, tenant, repository, command or credential is
// accepted from the request. There is no public webhook here.
export default {
  async fetch(req: Request, env: Env) {
    const token = env.BETA_TOKEN;
    if (!token || token.length < 32) return reply({ error: "rehearsal disabled" }, 503);
    if (!crypto.subtle.timingSafeEqual(await sha(req.headers.get("authorization") ?? ""), await sha(`Bearer ${token}`)))
      return reply({ error: "unauthorized" }, 401);
    return env.BETA.getByName("dogfood").fetch(req);
  },
};

export class BudgetMachine extends DurableObject<Env> {
  private sql = this.ctx.storage.sql;
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql.exec("CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, phase TEXT NOT NULL, used INTEGER NOT NULL DEFAULT 0, checkpoint TEXT, runs INTEGER NOT NULL DEFAULT 0)");
    if (!this.sql.exec<{name: string}>("PRAGMA table_info(tasks)").toArray().some(c => c.name === "limit_ms"))
      this.sql.exec(`ALTER TABLE tasks ADD COLUMN limit_ms INTEGER NOT NULL DEFAULT ${TASK_LIMIT}`);
    this.sql.exec("CREATE TABLE IF NOT EXISTS months (month TEXT PRIMARY KEY, used INTEGER NOT NULL)");
    this.sql.exec("CREATE TABLE IF NOT EXISTS lease (slot INTEGER PRIMARY KEY CHECK(slot=1), id TEXT, nonce TEXT, month TEXT, started INTEGER, reserved INTEGER)");
  }
  private task(id: string) { return this.sql.exec<Task>("SELECT * FROM tasks WHERE id=?", id).toArray()[0]; }
  private lease() { return this.sql.exec<Lease>("SELECT * FROM lease").toArray()[0]; }
  private used(month: string) { return this.sql.exec<{used: number}>("SELECT used FROM months WHERE month=?", month).toArray()[0]?.used ?? 0; }
  private matches(l: Lease) { return this.lease()?.nonce === l.nonce; }
  private view(id: string) {
    const t = this.task(id);
    const l = this.lease();
    return { task: t ? { ...t, checkpoint: t.checkpoint && JSON.parse(t.checkpoint) } : null,
      computeReserved: !!l, monthlyLimitMs: MONTH_LIMIT,
      monthUsedMs: this.used(monthOf(Date.now())), reservedMs: l?.reserved ?? 0,
      taskComputeUsdAtFullCpu: t ? t.used / HOUR * STANDARD1_USD_HOUR_MAX : 0 };
  }

  override async fetch(req: Request): Promise<Response> {
    const path = new URL(req.url).pathname;
    const workspace = this.env.WORKSPACE_R2 && this.env.OFFLINE_BOX &&
      /^\/workspaces\/(fixture)(?:\/(start|resume|retry))?$/.exec(path);
    const match = workspace || /^\/tasks\/([a-z0-9][a-z0-9-]{0,39})(?:\/(start|ci-green|ci-red|ack|retry))?$/.exec(path);
    if (!match) return reply({ error: "unknown route" }, 404);
    const [, name, act] = match;
    const id = workspace ? `workspace:${name}` : name;
    if (req.method === "GET" && !act) return reply(this.view(id), this.task(id) ? 200 : 404);
    if (req.method !== "POST" || !act || (req.body && req.headers.get("content-length") !== "0"))
      return reply({ error: "expected a bodyless POST" }, 400);
    const task = this.task(id);
    const seconds = req.headers.get("x-budget-seconds");
    const maxLimit = workspace ? WORKSPACE_LIMIT_MS : TASK_LIMIT;
    if (seconds !== null && (act !== "start" || !/^[1-9]\d{0,3}$/.test(seconds) || Number(seconds) > maxLimit / 1000))
      return reply({ error: `budget must be 1 to ${maxLimit / 1000} seconds on start` }, 400);
    const limit = task?.limit_ms ?? (seconds === null ? maxLimit : Number(seconds) * 1000);
    const next = workspace ? workspaceTransition(task?.phase ?? "new", act, !!task?.checkpoint)
      : transition(task?.phase ?? "new", act as Action, !!task?.checkpoint);
    if (next === "unchanged") return reply(this.view(id));
    if (next === "conflict") return reply({ error: "event out of order" }, 409);
    if (next === "done") {
      this.sql.exec("UPDATE tasks SET phase='done' WHERE id=?", id);
      return reply(this.view(id));
    }
    if (this.lease()) return reply({ error: "machine already reserved" }, 409);
    const started = Date.now(), month = monthOf(started), reserved = reservation(started, this.used(month), task?.used ?? 0, limit);
    if (reserved < 1000) return reply({ error: "compute budget exhausted" }, 429);
    const l = { id, started, month, reserved, nonce: crypto.randomUUID() };
    // Commit the reservation and alarm together, before any machine can boot.
    await this.ctx.storage.transaction(async tx => {
      if (this.lease() || this.task(id)?.phase !== task?.phase) return;
      this.sql.exec("INSERT INTO lease VALUES(1,?,?,?,?,?)", id, l.nonce, month, started, reserved);
      this.sql.exec("INSERT INTO tasks(id,phase,runs,limit_ms) VALUES(?,'running',1,?) ON CONFLICT(id) DO UPDATE SET phase='running', runs=runs+1", id, limit);
      await tx.setAlarm(started + reserved);
    });
    if (!this.matches(l)) return reply(this.view(id), 409);
    let checkpoint: string | null = null;
    try {
      const deadline = started + reserved;
      if (Date.now() >= deadline) throw new Error("reservation expired");
      const port = this.env.OFFLINE_BOX ?? await this.boot(l);
      if (Date.now() >= deadline || !this.matches(l)) throw new Error("deadline");
      const exchange = (step: string, body: string) => {
        if (Date.now() >= deadline || !this.matches(l)) throw new Error("reservation expired");
        return port.fetch(`http://box/${step}`, {
          method: "POST", body, signal: AbortSignal.timeout(Math.max(1, deadline - Date.now())),
          headers: { "x-lease": l.nonce, "x-deadline": String(deadline) },
        });
      };
      if (next === "capture" || next === "restore") {
        checkpoint = await workspaceStep(next, task?.checkpoint ?? null, this.env.WORKSPACE_R2!, exchange);
      } else {
        const response = await exchange(next, task?.checkpoint ?? "");
        if (!response.ok) throw new Error("step failed");
        const result = await response.json<{ head?: string; pr?: number; checks?: string }>();
        const allowedHeads: Record<number, string> = { 272: "7f0cf45530f4b488d263418e647e8c62788c1c4c" };
        if (!result.head || !result.pr || allowedHeads[result.pr] !== result.head || result.checks !== (next === "review" ? "awaiting-ci" : "passed") ||
          (task?.checkpoint && JSON.parse(task.checkpoint).head !== result.head))
          throw new Error("invalid checkpoint");
        checkpoint = JSON.stringify({ pr: result.pr, head: result.head, checks: result.checks });
      }
    } catch { /* keep the last verified checkpoint */ }
    try {
      const success = { review: "waiting_ci", test: "needs_decision", capture: "paused", restore: "done" } as const;
      await this.finish(l, checkpoint ? success[next] : "interrupted", checkpoint);
    } catch {
      // At the deadline the runner and alarm can still be stopping Docker.
      // Keep the lease until shutdown is confirmed, including after a crash.
      if (this.matches(l)) await this.ctx.storage.setAlarm(Date.now() + 1000);
      return reply({ ...this.view(id), error: "shutdown pending; check task status" }, 503);
    }
    const view = this.view(id);
    return reply(view, view.task?.phase === "interrupted" ? 503 : 200);
  }

  private async boot(l: Lease) {
    const c = this.ctx.container!;
    if (c.running) throw new Error("unexpected running machine");
    const deadline = l.started + l.reserved;
    c.start({ image: c.images.budget, instance: "standard-1", enableInternet: false });
    c.monitor().catch(() => {});
    await c.setInactivityTimeout(Math.max(1, deadline - Date.now()));
    // Retry readiness only, never a submitted operation.
    while (c.running && Date.now() < deadline && this.matches(l)) {
      const port = c.getTcpPort(8080);
      try {
        const ping = await port.fetch(new Request("http://box/ready", { signal: AbortSignal.timeout(Math.max(1, Math.min(1000, deadline - Date.now()))) }));
        if (ping.ok) return port;
      } catch { /* booting */ }
      await scheduler.wait(200);
    }
    throw new Error("machine did not become ready");
  }

  private async finish(l: Lease, phase: Phase, checkpoint: string | null) {
    if (!this.matches(l)) return;
    if (this.env.OFFLINE_BOX) {
      if (!(await this.env.OFFLINE_BOX.fetch("http://box/stopped", { headers: { "x-lease": l.nonce } })).ok)
        throw new Error("machine not stopped");
    } else {
      const c = this.ctx.container!;
      if (c.running) await c.destroy();
      if (c.running) throw new Error("machine not stopped"); // The durable alarm retries cleanup.
    }
    if (!this.matches(l)) return;
    const elapsed = Math.min(l.reserved, Math.max(0, Date.now() - l.started));
    if (Date.now() >= l.started + l.reserved) { phase = "interrupted"; checkpoint = null; }
    this.ctx.storage.transactionSync(() => {
      this.sql.exec("INSERT INTO months VALUES(?,?) ON CONFLICT(month) DO UPDATE SET used=used+excluded.used", l.month, elapsed);
      this.sql.exec("UPDATE tasks SET phase=?, used=used+?, checkpoint=COALESCE(?,checkpoint) WHERE id=?", phase, elapsed, checkpoint, l.id);
      this.sql.exec("DELETE FROM lease WHERE nonce=?", l.nonce);
    });
  }
  override async alarm() {
    const l = this.lease();
    if (!l) return;
    if (Date.now() < l.started + l.reserved) {
      await this.ctx.storage.setAlarm(l.started + l.reserved);
    } else {
      await this.finish(l, "interrupted", null);
    }
  }
}
