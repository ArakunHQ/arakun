// Cloud machines: who may have one, the key and session a container starts
// with, and the lifecycle (start, sleep with a snapshot, wake from it) driven
// with a stand-in for `ctx.container`. The tests run without containers.
import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { CLOUD_FLAG } from "../src/cloud_access";
import {
  BACKSTOP_MS,
  CHECK_MS,
  IDLE_MS,
  IMAGE,
  INSTANCE,
  MachineLife,
  SERVER_DIR,
  newMachineKey,
  nextStep,
  parseStatus,
  startEnv,
  type Box,
  type MachineRecord,
} from "../src/cloud_machine";
import { labelOf } from "../src/relay/label";
import { fromB64url } from "../src/util";
import { ADMIN, as, call, signIn, type SignedIn } from "./helpers";
import { claim, openPc, type Computer } from "./relay/helpers";

let nextId = 70_000;

/// A signed-in account, with the `cloud` flag unless told otherwise.
async function owner(flag = true): Promise<SignedIn> {
  const id = nextId++;
  const me = await signIn(id, `cloud${id}`);
  if (flag) expect((await call("PUT", `/admin/users/${id}/flags/${CLOUD_FLAG}`, ADMIN)).status).toBe(200);
  return me;
}

function post(path: string, session: string, body?: unknown): Promise<Response> {
  return call("POST", path, { ...as(session), "Content-Type": "application/json" }, body === undefined ? undefined : JSON.stringify(body));
}

/// A machine record for `who`, with a fresh key.
async function machineOf(who: SignedIn, name = "Cloud"): Promise<MachineRecord> {
  const key = await newMachineKey();
  return { ...key, githubId: who.github_id, name, base: "https://app.arakun.dev" };
}

/// The machine's key as the relay tests' computer, to claim with.
async function computerOf(m: MachineRecord): Promise<Computer> {
  const privateKey = await crypto.subtle.importKey("pkcs8", fromB64url(m.identity)!, { name: "ECDSA", namedCurve: "P-256" }, true, ["sign"]);
  const pub = fromB64url(m.identityPublic)!;
  const publicKey = await crypto.subtle.importKey("raw", pub, { name: "ECDSA", namedCurve: "P-256" }, true, ["verify"]);
  return { key: { privateKey, publicKey }, pub, pubB64: m.identityPublic, label: m.label };
}

/// What the stand-in container was asked, and what it answers.
class FakeBox implements Box {
  running = false;
  images: Record<string, string> = { [IMAGE]: "registry.test/server@sha256:one" };
  starts: ContainerStartupOptions[] = [];
  execs: { cmd: string[]; options?: ContainerExecOptions }[] = [];
  snapshots = 0;
  destroyed: unknown[] = [];
  timeouts: number[] = [];
  /// What `arakun-server status` prints; null makes it fail.
  status: string | null = statusText(0, 0, 0);
  /// The next exec of `true` throws, as when a snapshot does not restore.
  failNextStart = false;

  start(options?: ContainerStartupOptions): void {
    if (this.running) throw new Error("already running");
    this.starts.push(options!);
    this.running = true;
  }
  monitor(): Promise<void> {
    return new Promise(() => {});
  }
  async destroy(error?: unknown): Promise<void> {
    this.destroyed.push(error);
    this.running = false;
  }
  async snapshotContainer(options?: ContainerSnapshotOptions): Promise<ContainerSnapshot> {
    this.snapshots++;
    return { id: `snap-${this.snapshots}`, size: 1000, name: options?.name };
  }
  async setInactivityTimeout(durationMs: number | bigint): Promise<void> {
    this.timeouts.push(Number(durationMs));
  }
  async exec(cmd: string[], options?: ContainerExecOptions): Promise<ExecProcess> {
    this.execs.push({ cmd, options });
    if (cmd[0] === "true" && this.failNextStart) {
      this.failNextStart = false;
      throw new Error("snapshot not found");
    }
    const isStatus = cmd.join(" ") === "/usr/local/bin/arakun-server status";
    const text = isStatus ? (this.status ?? "") : "";
    const exitCode = isStatus && this.status === null ? 1 : 0;
    return {
      stdin: null,
      stdout: null,
      stderr: null,
      pid: 1,
      isPty: false,
      exitCode: Promise.resolve(exitCode),
      output: async () => ({ stdout: new TextEncoder().encode(text).buffer as ArrayBuffer, stderr: new ArrayBuffer(0), exitCode }),
      kill() {},
      resize() {},
    };
  }
}

function statusText(working: number, busy: number, connected: number): string {
  return [
    "arakun server 0.24.0 on Cloud",
    "reach     relay",
    "account   octocat",
    `devices   2 paired, ${connected} connected`,
    "folders   1",
    `sessions  3 (${working} working, 0 waiting on you, ${busy} agents busy)`,
  ].join("\n");
}

/// Runs `body` with the lifecycle of a fresh machine object, over its real
/// storage and a stand-in container.
async function withLife<T>(m: MachineRecord, box: FakeBox, body: (life: MachineLife, storage: DurableObjectStorage) => Promise<T>): Promise<T> {
  const ns = env.CLOUD_MACHINE!;
  const stub = ns.get(ns.idFromName(m.label));
  return runInDurableObject(stub, async (_instance, state) => {
    const life = new MachineLife(state.storage, box, env);
    if (!(await life.record())) await life.create(m);
    return body(life, state.storage);
  });
}

async function cloudSessions(label: string): Promise<number> {
  return (await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions WHERE kind = 'cloud' AND machine_label = ?").bind(label).first<number>("n"))!;
}

describe("the cloud machine routes", () => {
  it("need a session", async () => {
    expect((await call("GET", "/cloud/machines")).status).toBe(401);
    expect((await call("POST", "/cloud/machines")).status).toBe(401);
  });

  it("need the cloud flag", async () => {
    const me = await owner(false);
    expect((await call("GET", "/cloud/machines", as(me.session))).status).toBe(403);
    expect((await post("/cloud/machines", me.session)).status).toBe(403);
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM cloud_machines").first<number>("n");
    expect(n).toBe(0);
  });

  it("refuse a blocked account, flag or not", async () => {
    const me = await owner();
    expect((await call("PUT", `/admin/users/${me.github_id}/block`, ADMIN)).status).toBe(204);
    expect((await post("/cloud/machines", me.session)).status).toBe(403);
  });

  it("refuse a cloud session, even its own machine's", async () => {
    const me = await owner();
    const made = await (await post("/cloud/machines", me.session)).json<{ label: string }>();
    const vars = await startEnv(env, { ...(await machineOf(me)), label: made.label }, false);
    for (const [method, path] of [
      ["GET", "/cloud/machines"],
      ["POST", "/cloud/machines"],
      ["POST", `/cloud/machines/${made.label}/wake`],
      ["DELETE", `/cloud/machines/${made.label}`],
    ] as const) {
      expect((await call(method, path, as(vars!["ARAKUN_CLOUD_SESSION"]!))).status, `${method} ${path}`).toBe(401);
    }
  });

  it("make one machine per account, named, and list it", async () => {
    const me = await owner();
    const res = await post("/cloud/machines", me.session, { name: "  Builder  " });
    expect(res.status).toBe(201);
    const made = await res.json<{ label: string; name: string; running: boolean; snapshot: boolean }>();
    expect(made).toMatchObject({ name: "Builder", running: false, snapshot: false });
    expect(made.label).toMatch(/^[a-z2-7]{16}$/);
    const row = await env.DB.prepare("SELECT github_id, name FROM cloud_machines WHERE label = ?").bind(made.label).first();
    expect(row).toEqual({ github_id: me.github_id, name: "Builder" });

    expect((await post("/cloud/machines", me.session)).status).toBe(409);
    const list = await (await call("GET", "/cloud/machines", as(me.session))).json<{ label: string }[]>();
    expect(list.map((m) => m.label)).toEqual([made.label]);
  });

  it("refuse a bad name", async () => {
    const me = await owner();
    expect((await post("/cloud/machines", me.session, { name: "" })).status).toBe(400);
    expect((await post("/cloud/machines", me.session, { name: "x".repeat(101) })).status).toBe(400);
    expect((await post("/cloud/machines", me.session, { name: 7 })).status).toBe(400);
  });

  it("answer 503 for a wake where there is no container, as in the tests", async () => {
    const me = await owner();
    const made = await (await post("/cloud/machines", me.session)).json<{ label: string }>();
    expect((await post(`/cloud/machines/${made.label}/wake`, me.session)).status).toBe(503);
  });

  it("hide another account's machine", async () => {
    const me = await owner();
    const other = await owner();
    const made = await (await post("/cloud/machines", me.session)).json<{ label: string }>();
    expect((await post(`/cloud/machines/${made.label}/wake`, other.session)).status).toBe(404);
    expect((await post(`/cloud/machines/${made.label}/sleep`, other.session)).status).toBe(404);
    expect((await call("DELETE", `/cloud/machines/${made.label}`, as(other.session))).status).toBe(404);
    expect(await (await call("GET", "/cloud/machines", as(other.session))).json()).toEqual([]);
  });

  it("remove a machine with its key, its registration and its session", async () => {
    const me = await owner();
    const made = await (await post("/cloud/machines", me.session)).json<{ label: string }>();
    await env.DB.prepare("INSERT INTO machines (label, github_id, pub, name, kind, created_at, last_seen_at) VALUES (?, ?, 'p', 'Cloud', 'server', 1, 1)")
      .bind(made.label, me.github_id)
      .run();
    expect((await call("DELETE", `/cloud/machines/${made.label}`, as(me.session))).status).toBe(204);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM cloud_machines").first<number>("n")).toBe(0);
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM machines WHERE label = ?").bind(made.label).first<number>("n")).toBe(0);
    expect(await env.CLOUD_MACHINE!.get(env.CLOUD_MACHINE!.idFromName(made.label)).state()).toBeNull();
    // Room for a new one.
    expect((await post("/cloud/machines", me.session)).status).toBe(201);
  });

  it("count against the plan's machines", async () => {
    const me = await owner();
    await env.CONFIG.put("plans", JSON.stringify({ free: { machines: 1 } }));
    await env.DB.prepare("INSERT INTO machines (label, github_id, pub, name, kind, created_at, last_seen_at) VALUES ('deskdeskdeskdesk', ?, 'p', 'Desk', 'app', 1, 1)")
      .bind(me.github_id)
      .run();
    expect((await post("/cloud/machines", me.session)).status).toBe(409);
  });
});

describe("the operator's cloud machine routes", () => {
  it("need the admin secret", async () => {
    expect((await call("GET", "/admin/cloud/machines")).status).toBe(401);
  });

  it("list every machine and put one to sleep, with a log entry", async () => {
    const me = await owner();
    const made = await (await post("/cloud/machines", me.session)).json<{ label: string }>();
    const list = await (await call("GET", "/admin/cloud/machines", ADMIN)).json<Record<string, unknown>[]>();
    expect(list).toEqual([expect.objectContaining({ label: made.label, github_id: me.github_id, login: me.login, running: false, snapshot: false })]);
    expect((await call("POST", `/admin/cloud/machines/${made.label}/sleep`, ADMIN)).status).toBe(204);
    const log = await env.DB.prepare("SELECT action, github_id, detail FROM admin_log WHERE action = 'cloud.machine.sleep'").first();
    expect(log).toEqual({ action: "cloud.machine.sleep", github_id: me.github_id, detail: JSON.stringify({ label: made.label }) });
    expect((await call("POST", "/admin/cloud/machines/aaaaaaaaaaaaaaaa/sleep", ADMIN)).status).toBe(404);
  });
});

describe("a machine's key", () => {
  it("is ring's PKCS#8, and gives the label of its public half", async () => {
    const key = await newMachineKey();
    const der = fromB64url(key.identity)!;
    const pub = fromB64url(key.identityPublic)!;
    expect(der.length).toBe(138);
    expect(pub.length).toBe(65);
    expect(Array.from(der.subarray(der.length - 65))).toEqual(Array.from(pub));
    expect(key.label).toBe(await labelOf(pub));
    // A standard PKCS#8 too: WebCrypto reads it and signs what the public half verifies.
    const m = { ...key, githubId: 1, name: "x", base: "https://x" };
    const me = await computerOf(m);
    const data = new TextEncoder().encode("hello");
    const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, me.key.privateKey, data);
    expect(await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, me.key.publicKey, sig, data)).toBe(true);
  });
});

describe("the environment a machine starts with", () => {
  it("signs the server in at its own door, where the key claims the label", async () => {
    const me = await owner();
    const m = await machineOf(me, "Builder");
    const vars = (await startEnv(env, m, true))!;
    expect(vars).toMatchObject({
      ARAKUN_CLOUD_NAME: "Builder",
      ARAKUN_CLOUD_BASE: "https://app.arakun.dev",
      ARAKUN_CLOUD_GITHUB_ID: String(me.github_id),
      ARAKUN_CLOUD_LOGIN: me.login,
      ARAKUN_CLOUD_IDENTITY: m.identity,
      ARAKUN_CLOUD_IDENTITY_PUBLIC: m.identityPublic,
    });
    const pc = await openPc(m.label, vars["ARAKUN_CLOUD_SESSION"]!, "name=Builder&kind=server");
    expect(await claim(pc, await computerOf(m))).toEqual({ type: "claimed" });
    const row = await env.DB.prepare("SELECT github_id, kind, name FROM machines WHERE label = ?").bind(m.label).first();
    expect(row).toEqual({ github_id: me.github_id, kind: "server", name: "Builder" });
  });

  it("leaves the key out when the disk has it", async () => {
    const me = await owner();
    const vars = (await startEnv(env, await machineOf(me), false))!;
    expect(vars["ARAKUN_CLOUD_IDENTITY"]).toBeUndefined();
    expect(vars["ARAKUN_CLOUD_IDENTITY_PUBLIC"]).toBeUndefined();
  });

  it("ends the session of the start before", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const first = (await startEnv(env, m, true))!;
    const second = (await startEnv(env, m, false))!;
    expect(second["ARAKUN_CLOUD_SESSION"]).not.toBe(first["ARAKUN_CLOUD_SESSION"]);
    expect(await cloudSessions(m.label)).toBe(1);
    const pc = await openPc(m.label, first["ARAKUN_CLOUD_SESSION"]!, "name=Cloud&kind=server");
    expect(await pc.inbox.control()).toEqual({ type: "bye", reason: "signed_out" });
  });

  it("is refused without the flag, or for a blocked account", async () => {
    const plain = await owner(false);
    expect(await startEnv(env, await machineOf(plain), true)).toBeNull();
    const blocked = await owner();
    await call("PUT", `/admin/users/${blocked.github_id}/block`, ADMIN);
    expect(await startEnv(env, await machineOf(blocked), true)).toBeNull();
    expect(await env.DB.prepare("SELECT COUNT(*) AS n FROM sessions WHERE kind = 'cloud'").first<number>("n")).toBe(0);
  });
});

describe("the server's status", () => {
  it("reads the counts", () => {
    expect(parseStatus(statusText(1, 2, 3))).toEqual({ working: 1, agentsBusy: 2, connected: 3 });
    expect(parseStatus("arakun server is not running")).toBeNull();
  });

  it("keeps a machine up while a turn runs or a device is connected, and lets it sleep after the idle time", () => {
    const t = 10_000_000;
    const idle = parseStatus(statusText(0, 0, 0));
    expect(nextStep(t, t - IDLE_MS + 1, idle)).toEqual({ sleep: false, lastActive: t - IDLE_MS + 1 });
    expect(nextStep(t, t - IDLE_MS, idle)).toEqual({ sleep: true, lastActive: t - IDLE_MS });
    for (const busy of [statusText(1, 0, 0), statusText(0, 1, 0), statusText(0, 0, 1)]) {
      expect(nextStep(t, 0, parseStatus(busy))).toEqual({ sleep: false, lastActive: t });
    }
    // A server that does not answer is idle.
    expect(nextStep(t, 0, null).sleep).toBe(true);
  });
});

describe("a machine's lifecycle", () => {
  it("starts from the image with the key, then sleeps with a snapshot and no session", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    await withLife(m, box, async (life, storage) => {
      expect(await life.wake()).toBe("running");
      expect(box.starts).toHaveLength(1);
      const start = box.starts[0]!;
      expect(start.image).toBe(box.images[IMAGE]);
      expect(start.instance).toEqual(INSTANCE);
      expect(start.enableInternet).toBe(true);
      expect(start.env?.["ARAKUN_CLOUD_IDENTITY"]).toBe(m.identity);
      expect(box.timeouts).toEqual([BACKSTOP_MS]);
      expect(await storage.getAlarm()).toBeGreaterThan(Date.now() + CHECK_MS - 5_000);
      expect(await cloudSessions(m.label)).toBe(1);

      // Waking a running machine starts nothing.
      expect(await life.wake()).toBe("running");
      expect(box.starts).toHaveLength(1);

      await life.sleep("idle");
      expect(box.running).toBe(false);
      expect(box.snapshots).toBe(1);
      // The sign-in goes before the snapshot.
      const rm = box.execs.findIndex((e) => e.cmd[0] === "rm");
      expect(box.execs[rm]!.cmd).toEqual(["rm", "-f", `${SERVER_DIR}/account.json`]);
      expect(await cloudSessions(m.label)).toBe(0);
      expect(await storage.getAlarm()).toBeNull();
      expect(await life.state()).toMatchObject({ running: false, snapshot: true });
    });
  });

  it("wakes from its snapshot, without the key", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    await withLife(m, box, async (life) => {
      await life.wake();
      await life.sleep("idle");
      expect(await life.wake()).toBe("running");
      const start = box.starts[1]!;
      expect(start.containerSnapshot).toEqual({ id: "snap-1" });
      expect(start.image).toBeUndefined();
      expect(start.env?.["ARAKUN_CLOUD_IDENTITY"]).toBeUndefined();
      expect(start.env?.["ARAKUN_CLOUD_SESSION"]).toMatch(/^[A-Za-z0-9_-]{43}$/);
    });
  });

  it("starts from the image when the snapshot does not restore, or is of another image", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    await withLife(m, box, async (life) => {
      await life.wake();
      await life.sleep("idle");
      box.failNextStart = true;
      expect(await life.wake()).toBe("running");
      expect(box.starts[1]!.containerSnapshot).toEqual({ id: "snap-1" });
      expect(box.starts[2]!.image).toBe(box.images[IMAGE]);
      expect(box.starts[2]!.env?.["ARAKUN_CLOUD_IDENTITY"]).toBe(m.identity);

      await life.sleep("idle");
      box.images = { [IMAGE]: "registry.test/server@sha256:two" };
      await life.wake();
      expect(box.starts[3]!.image).toBe("registry.test/server@sha256:two");
    });
  });

  it("starts once for wakes that come together", async () => {
    const me = await owner();
    const box = new FakeBox();
    await withLife(await machineOf(me), box, async (life) => {
      expect(await Promise.all([life.wake(), life.wake(), life.wake()])).toEqual(["running", "running", "running"]);
      expect(box.starts).toHaveLength(1);
    });
  });

  it("does not start for an account that lost the flag", async () => {
    const me = await owner();
    const box = new FakeBox();
    await call("DELETE", `/admin/users/${me.github_id}/flags/${CLOUD_FLAG}`, ADMIN);
    await withLife(await machineOf(me), box, async (life) => {
      expect(await life.wake()).toBe("not_allowed");
      expect(box.starts).toHaveLength(0);
    });
  });

  it("renews the timeout while a turn runs, and sleeps once idle", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    await withLife(m, box, async (life, storage) => {
      await life.wake();
      // Idle for longer than IDLE_MS, but an agent is busy.
      await storage.put("last_active", Date.now() - IDLE_MS - 1000);
      box.status = statusText(0, 1, 0);
      await life.check();
      expect(box.running).toBe(true);
      expect(box.timeouts).toEqual([BACKSTOP_MS, BACKSTOP_MS]);
      expect(await storage.get<number>("last_active")).toBeGreaterThan(Date.now() - 5_000);
      const status = box.execs.find((e) => e.cmd[1] === "status")!;
      expect(status.options).toMatchObject({ user: "arakun", env: { ARAKUN_SERVER_DIR: SERVER_DIR } });

      // The turn ended long ago, and nobody is connected.
      await storage.put("last_active", Date.now() - IDLE_MS - 1000);
      box.status = statusText(0, 0, 0);
      await life.check();
      expect(box.running).toBe(false);
      expect(box.snapshots).toBe(1);
    });
  });

  it("stays up through a quiet check before the idle time", async () => {
    const me = await owner();
    const box = new FakeBox();
    await withLife(await machineOf(me), box, async (life) => {
      await life.wake();
      box.status = null;
      await life.check();
      expect(box.running).toBe(true);
    });
  });

  it("sleeps at the next check once the account is blocked", async () => {
    const me = await owner();
    const box = new FakeBox();
    await withLife(await machineOf(me), box, async (life) => {
      await life.wake();
      box.status = statusText(1, 1, 1);
      await call("PUT", `/admin/users/${me.github_id}/block`, ADMIN);
      await life.check();
      expect(box.running).toBe(false);
    });
  });

  it("ends the session of a container that stopped by itself", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    await withLife(m, box, async (life) => {
      await life.wake();
      box.running = false;
      await life.check();
      expect(await cloudSessions(m.label)).toBe(0);
    });
  });

  it("stops a container that never came up, and ends its session", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    box.failNextStart = true;
    await withLife(m, box, async (life) => {
      await expect(life.wake()).rejects.toThrow("snapshot not found");
      expect(box.running).toBe(false);
      expect(await cloudSessions(m.label)).toBe(0);
      expect(await life.wake()).toBe("running");
    });
  });

  it("forgets everything when removed", async () => {
    const me = await owner();
    const m = await machineOf(me);
    const box = new FakeBox();
    await withLife(m, box, async (life, storage) => {
      await life.wake();
      await life.remove();
      expect(box.running).toBe(false);
      expect(box.destroyed).toEqual(["removed"]);
      expect(await life.record()).toBeNull();
      expect(await storage.getAlarm()).toBeNull();
      expect(await cloudSessions(m.label)).toBe(0);
    });
  });
});
