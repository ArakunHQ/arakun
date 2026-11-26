// A cloud machine: `arakun-server` in a container we run for an account with
// the `cloud` flag. One object per machine, named by its relay label, drives
// the container through `ctx.container` under the `durable_object` scheduling
// policy (the `Container` class loses support on 2026-12-31).
//
// The server inside dials the relay like any other computer: it claims its
// label at /machines/<label>/pc, and the account's devices reach it at
// /machines/<label>/ws. No new client protocol.
//
// Identity. The object makes the machine's P-256 key when the machine is
// created and keeps it, so the label is known before the container first runs
// and stays the same when a snapshot is lost. The key goes into the container
// only on a start from the image; a restored snapshot has it on disk already.
//
// Sign-in. Each start makes a new `cloud` session for the label (auth.ts,
// `newCloudSession`), which ends the one before. It goes in as an environment
// variable, never into the image; the entrypoint writes it where the server
// reads it, and sleep removes that file and ends the session before the
// snapshot, so a snapshot never holds a live token.
//
// Lifecycle. A device knocking at the machine's door wakes it (machines.ts).
// An alarm checks every few minutes: while the server says an agent turn runs
// or a device is connected, the container stays up and its inactivity timeout
// is renewed, since work inside the container is no activity to Cloudflare.
// After IDLE_MS of neither, the object snapshots the disk and stops the
// container; the next wake restores the snapshot.

import { DurableObject } from "cloudflare:workers";
import { newCloudSession } from "./auth";
import { CLOUD_FLAG } from "./cloud_access";
import { labelOf } from "./relay/label";
import { b64url, fromB64url, type Env } from "./util";

/// Asleep after this long with no agent turn and no device connected.
export const IDLE_MS = 15 * 60 * 1000;
/// How often the alarm asks the server what it is doing.
export const CHECK_MS = 5 * 60 * 1000;
/// Cloudflare stops the container this long after the object last ran, as a
/// backstop for an alarm that never comes. Two missed checks past the idle
/// time; the API takes at most 6 hours. A stop here loses the disk since the
/// last snapshot.
export const BACKSTOP_MS = IDLE_MS + 2 * CHECK_MS;
/// standard-2 (1 vCPU, 6 GiB) with the most disk an account gets by default:
/// standard-2's 12 GB is too small for a checkout, its dependencies and a build.
/// A snapshot holds at most 20 GB too.
export const INSTANCE = { vcpu: 1, memoryMib: 6144, diskMb: 20_000 } as const;
/// The image's name under `[containers.images]` in wrangler.toml.
export const IMAGE = "server";
/// Where the image keeps the server's files, and who runs it (machine/).
export const SERVER_DIR = "/home/arakun/.local/share/arakun/server";
export const SERVER_USER = "arakun";
/// How long one `arakun-server status` may take.
const STATUS_TIMEOUT_MS = 10_000;

/// What the object keeps about its machine.
export interface MachineRecord {
  label: string;
  /// The owner's account ID. Stored under its old name, so records written
  /// before accounts had their own IDs read the same; for those it was the
  /// GitHub ID, which is the account's ID now.
  githubId: number;
  name: string;
  /// The account host the server signs in to and dials, such as
  /// `https://app.arakun.dev`.
  base: string;
  /// The relay key, PKCS#8 in the form `arakun-server` reads (base64url).
  identity: string;
  /// Its public half, the raw point (base64url): the label derives from it.
  identityPublic: string;
}

/// A snapshot restores only on the image it was taken from.
interface Snap {
  id: string;
  size: number;
  image: string;
  at: number;
}

/// What `arakun-server status` says about work and devices.
export interface ServerStatus {
  working: number;
  agentsBusy: number;
  connected: number;
}

/// What the owner and the operator see.
export interface MachineState {
  label: string;
  name: string;
  running: boolean;
  /// A snapshot to wake from.
  snapshot: boolean;
  /// Unix milliseconds of the last activity seen, or null before the first.
  last_active: number | null;
}

/// The part of `ctx.container` the lifecycle uses, so tests can stand in for it.
export type Box = Pick<Container, "running" | "images" | "start" | "monitor" | "destroy" | "snapshotContainer" | "setInactivityTimeout" | "exec">;

/// The part of the object's storage the lifecycle uses.
export type Store = Pick<DurableObjectStorage, "get" | "put" | "delete" | "deleteAll" | "setAlarm" | "deleteAlarm">;

/// Why a start was refused.
export type Refused = "no_machine" | "not_allowed" | "no_container";

/// A new relay key for a machine: the PKCS#8 that ring, which `arakun-server`
/// uses, reads and writes, and the label it gives. ring takes its own layout
/// only, so the bytes are laid out here rather than exported as WebCrypto's
/// PKCS#8, which may add the curve parameters.
export async function newMachineKey(): Promise<{ identity: string; identityPublic: string; label: string }> {
  const pair = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const raw = new Uint8Array((await crypto.subtle.exportKey("raw", pair.publicKey)) as ArrayBuffer);
  const jwk = (await crypto.subtle.exportKey("jwk", pair.privateKey)) as JsonWebKey;
  const d = fromB64url(jwk.d ?? "");
  if (!d || d.length !== 32 || raw.length !== 65) throw new Error("unexpected P-256 key");
  return { identity: b64url(ringPkcs8(d, raw)), identityPublic: b64url(raw), label: await labelOf(raw) };
}

/// ring's PKCS#8 v1 for a P-256 key: its fixed template around the private
/// scalar and the public point (ecPublicKey_p256_pkcs8_v1_template.der).
export function ringPkcs8(d: Uint8Array, publicRaw: Uint8Array): Uint8Array {
  const head = [
    0x30, 0x81, 0x87, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce,
    0x3d, 0x03, 0x01, 0x07, 0x04, 0x6d, 0x30, 0x6b, 0x02, 0x01, 0x01, 0x04, 0x20,
  ];
  const middle = [0xa1, 0x44, 0x03, 0x42, 0x00];
  return Uint8Array.from([...head, ...d, ...middle, ...publicRaw]);
}

/// The counts in `arakun-server status`, or null when it does not read as one.
export function parseStatus(text: string): ServerStatus | null {
  const sessions = /^sessions\s+\d+ \((\d+) working, \d+ waiting on you, (\d+) agents busy\)$/m.exec(text);
  const devices = /^devices\s+\d+ paired, (\d+) connected$/m.exec(text);
  if (!sessions || !devices) return null;
  return { working: Number(sessions[1]), agentsBusy: Number(sessions[2]), connected: Number(devices[1]) };
}

/// Whether the server is doing anything a person waits for: an agent turn, or
/// a device following along.
export function isActive(status: ServerStatus | null): boolean {
  return status !== null && (status.working > 0 || status.agentsBusy > 0 || status.connected > 0);
}

/// What the alarm does next: sleep, or check again. A server that does not
/// answer counts as idle, so a broken one still goes to sleep.
export function nextStep(now: number, lastActive: number, status: ServerStatus | null): { sleep: boolean; lastActive: number } {
  const last = isActive(status) ? now : lastActive;
  return { sleep: now - last >= IDLE_MS, lastActive: last };
}

/// Whether the account may run a cloud machine now: it has the `cloud` flag
/// and is not blocked. Its login, for the server's sign-in file, or null.
export async function allowedOwner(env: Env, accountId: number): Promise<string | null> {
  const row = await env.DB.prepare(
    `SELECT u.login FROM accounts u JOIN user_flags f ON f.account_id = u.id AND f.flag = ?2
     WHERE u.id = ?1 AND u.blocked_at IS NULL`,
  )
    .bind(accountId, CLOUD_FLAG)
    .first<{ login: string }>();
  return row?.login ?? null;
}

/// Ends the machine's cloud session, so a token copied out of the container
/// stops working while the machine sleeps or after it is gone.
export async function endCloudSession(env: Env, label: string): Promise<void> {
  await env.DB.prepare("DELETE FROM sessions WHERE kind = 'cloud' AND machine_label = ?").bind(label).run();
}

/// The environment the container starts with: what the entrypoint
/// (machine/entrypoint.sh) needs to sign the server in and, on a start from
/// the image, its relay key. Makes a new cloud session, which ends the one
/// before. Null when the owner may not run a machine.
export async function startEnv(env: Env, m: MachineRecord, withIdentity: boolean): Promise<Record<string, string> | null> {
  const login = await allowedOwner(env, m.githubId);
  if (login === null) return null;
  const made = await newCloudSession(env, m.githubId, m.label, m.name);
  if (!made) return null;
  const vars: Record<string, string> = {
    ARAKUN_CLOUD_NAME: m.name,
    ARAKUN_CLOUD_BASE: m.base,
    ARAKUN_CLOUD_SESSION: made.session,
    ARAKUN_CLOUD_SESSION_ID: made.session_id,
    // The account ID, under the name the image reads: it writes it as the
    // login's `github_id`, the name clients still use for the account ID.
    ARAKUN_CLOUD_GITHUB_ID: String(m.githubId),
    ARAKUN_CLOUD_LOGIN: login,
  };
  if (withIdentity) {
    vars["ARAKUN_CLOUD_IDENTITY"] = m.identity;
    vars["ARAKUN_CLOUD_IDENTITY_PUBLIC"] = m.identityPublic;
  }
  return vars;
}

/// The machine's lifecycle over its storage and its container. The object
/// below is a thin shell around it; tests drive it with a stand-in container.
export class MachineLife {
  /// Wakes that arrive while one start runs wait for it.
  private starting: Promise<"running" | Refused> | null = null;

  constructor(
    private readonly storage: Store,
    private readonly box: Box | undefined,
    private readonly env: Env,
  ) {}

  async record(): Promise<MachineRecord | null> {
    return (await this.storage.get<MachineRecord>("machine")) ?? null;
  }

  async create(m: MachineRecord): Promise<void> {
    if (await this.record()) throw new Error(`machine ${m.label} exists`);
    await this.storage.put("machine", m);
  }

  async state(): Promise<MachineState | null> {
    const m = await this.record();
    if (!m) return null;
    const [snap, last] = await Promise.all([this.storage.get<Snap>("snap"), this.storage.get<number>("last_active")]);
    return { label: m.label, name: m.name, running: this.box?.running ?? false, snapshot: snap !== undefined, last_active: last ?? null };
  }

  /// Starts the machine if it sleeps, and counts as activity either way.
  async wake(): Promise<"running" | Refused> {
    await this.storage.put("last_active", Date.now());
    if (this.box?.running) return "running";
    this.starting ??= this.boot().finally(() => (this.starting = null));
    return this.starting;
  }

  private async boot(): Promise<"running" | Refused> {
    const m = await this.record();
    if (!m) return "no_machine";
    const box = this.box;
    if (!box) return "no_container";
    if (box.running) return "running";
    const image = box.images[IMAGE];
    if (!image) return "no_container";
    const snap = await this.storage.get<Snap>("snap");
    if (snap && snap.image === image) {
      const vars = await startEnv(this.env, m, false);
      if (!vars) return "not_allowed";
      try {
        box.start({ containerSnapshot: { id: snap.id }, instance: INSTANCE, enableInternet: true, env: vars });
        await this.started(box, image);
        return "running";
      } catch (e) {
        // Expired (30 days without a restore) or broken: start clean, with
        // the key, so the machine keeps its label.
        console.error(`cloud machine ${m.label}: snapshot ${snap.id} did not restore:`, e);
        await this.storage.delete("snap");
        if (box.running) await box.destroy("snapshot restore failed");
      }
    }
    const vars = await startEnv(this.env, m, true);
    if (!vars) return "not_allowed";
    box.start({ image, instance: INSTANCE, enableInternet: true, env: vars });
    try {
      await this.started(box, image);
    } catch (e) {
      // Half started is worse than stopped: the next wake tries again.
      await endCloudSession(this.env, m.label);
      if (box.running) await box.destroy("start failed");
      throw e;
    }
    return "running";
  }

  /// After `start()`: `exec` waits for the container to be up, and throws if
  /// it never comes. The server dials the relay by itself; nothing waits for it.
  private async started(box: Box, image: string): Promise<void> {
    box.monitor().catch((e) => console.error("cloud machine container exited:", e));
    await (await box.exec(["true"], { stdout: "ignore", stderr: "ignore" })).exitCode;
    await box.setInactivityTimeout(BACKSTOP_MS);
    await this.storage.put("image", image);
    await this.storage.setAlarm(Date.now() + CHECK_MS);
  }

  /// The alarm: keep it up while it works or someone watches, else sleep.
  async check(): Promise<void> {
    const box = this.box;
    const m = await this.record();
    if (!m) return;
    // Stopped without a sleep (a crash, or the backstop): its session goes too.
    if (!box?.running) {
      await endCloudSession(this.env, m.label);
      return;
    }
    // A flag taken away or a block stops the machine at the next check.
    if ((await allowedOwner(this.env, m.githubId)) === null) {
      await this.sleep("access ended");
      return;
    }
    const now = Date.now();
    const last = (await this.storage.get<number>("last_active")) ?? 0;
    const step = nextStep(now, last, await this.status(box));
    if (step.sleep) {
      await this.sleep("idle");
      return;
    }
    await this.storage.put("last_active", step.lastActive);
    await box.setInactivityTimeout(BACKSTOP_MS);
    await this.storage.setAlarm(now + CHECK_MS);
  }

  /// What the server says it is doing; null when it does not answer.
  private async status(box: Box): Promise<ServerStatus | null> {
    try {
      const proc = await box.exec(["/usr/local/bin/arakun-server", "status"], {
        user: SERVER_USER,
        env: { HOME: `/home/${SERVER_USER}`, ARAKUN_SERVER_DIR: SERVER_DIR },
        stderr: "ignore",
        signal: AbortSignal.timeout(STATUS_TIMEOUT_MS),
      });
      const out = await proc.output();
      return out.exitCode === 0 ? parseStatus(new TextDecoder().decode(out.stdout)) : null;
    } catch {
      return null;
    }
  }

  /// Ends the session, snapshots the disk and stops the container. A failed
  /// snapshot (over 20 GB, or the API) still stops it; the next start is then
  /// from the image, and the work since the last snapshot is lost.
  async sleep(reason: string): Promise<void> {
    const box = this.box;
    const m = await this.record();
    if (!m) return;
    await endCloudSession(this.env, m.label);
    if (!box?.running) return;
    try {
      // The session is over, but its file has no business in a snapshot.
      await (await box.exec(["rm", "-f", `${SERVER_DIR}/account.json`], { stdout: "ignore", stderr: "ignore" })).exitCode;
    } catch (e) {
      console.error(`cloud machine ${m.label}: could not remove the sign-in before the snapshot:`, e);
    }
    const image = await this.storage.get<string>("image");
    try {
      if (!image) throw new Error("the image it runs is unknown");
      const s = await box.snapshotContainer({ name: `${m.label}-${Date.now()}` });
      await this.storage.put("snap", { id: s.id, size: s.size, image, at: Date.now() } satisfies Snap);
    } catch (e) {
      console.error(`cloud machine ${m.label}: snapshot failed (${reason}):`, e);
    }
    await box.destroy(reason);
    await this.storage.deleteAlarm();
  }

  /// Stops the container and forgets everything: the key, the snapshot.
  async remove(): Promise<void> {
    const m = await this.record();
    if (m) await endCloudSession(this.env, m.label);
    if (this.box?.running) await this.box.destroy("removed");
    await this.storage.deleteAlarm();
    await this.storage.deleteAll();
  }
}

/// The Durable Object, one per cloud machine, named by its label. Called over
/// RPC from cloud_machines.ts and machines.ts only.
export class CloudMachine extends DurableObject<Env> {
  private readonly life: MachineLife;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.life = new MachineLife(ctx.storage, ctx.container, env);
    // The timeout does not outlive the object: after a deploy it restarts
    // without one, and the container would stop shortly after.
    const c = ctx.container;
    if (c?.running) void ctx.blockConcurrencyWhile(() => c.setInactivityTimeout(BACKSTOP_MS));
  }

  create(m: MachineRecord): Promise<void> {
    return this.life.create(m);
  }

  state(): Promise<MachineState | null> {
    return this.life.state();
  }

  wake(): Promise<"running" | Refused> {
    return this.life.wake();
  }

  sleep(reason: string): Promise<void> {
    return this.life.sleep(reason);
  }

  remove(): Promise<void> {
    return this.life.remove();
  }

  override async alarm(): Promise<void> {
    await this.life.check();
  }
}
