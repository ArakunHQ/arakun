import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { b64url, ulid } from "../src/util";
import { api, envelope, expectJson, makeDevice, newKey, register, seal, signText, team, user, join } from "./clients";
import { nowSeconds } from "./helpers";

describe("devices", () => {
  it("registers a user's first device without an endorsement, and again answers the same ID", async () => {
    const u = await user(1001, "octocat");
    const d = await makeDevice();
    expect(await expectJson(register(u, d), 201)).toEqual({ device_id: d.id });
    expect(await expectJson(register(u, d), 200)).toEqual({ device_id: d.id });
    const list = await expectJson(api(u, "GET", "/users/1001/devices"), 200);
    expect(list).toEqual([{ device_id: d.id, sig_pub: d.sigPub, enc_pub: d.encPub, name: "laptop", created_at: expect.any(Number), endorsed_by: null }]);
  });

  it("takes a later device only with an endorsement by one of the user's devices", async () => {
    const u = await user(1001, "octocat");
    const first = await makeDevice();
    const second = await makeDevice();
    await expectJson(register(u, first), 201);
    expect((await expectJson(register(u, second), 400)).error).toMatch(/endorsement/);
    expect(await expectJson(register(u, second, first, "phone"), 201)).toEqual({ device_id: second.id });
    const list = await expectJson<{ device_id: string; endorsed_by: string | null }[]>(api(u, "GET", "/users/1001/devices"), 200);
    expect(list.find((d) => d.device_id === second.id)?.endorsed_by).toBe(first.id);
  });

  it("refuses a forged endorsement", async () => {
    const u = await user(1001, "octocat");
    const other = await user(1002, "someone");
    const first = await makeDevice();
    const theirs = await makeDevice();
    await expectJson(register(u, first), 201);
    await expectJson(register(other, theirs), 201);
    const fresh = await makeDevice();
    const stranger = await makeDevice();
    const text = `arakun-teams/v1/device|1001|${fresh.sigPub}|${fresh.encPub}`;
    for (const endorsement of [
      // Claims to be the user's device, signed by another key.
      { by: first.id, sig: await signText(stranger, text) },
      // The right key, over another enc_pub: the server would have swapped it.
      { by: first.id, sig: await signText(first, `arakun-teams/v1/device|1001|${fresh.sigPub}|${first.encPub}`) },
      // The right text for another user.
      { by: first.id, sig: await signText(first, `arakun-teams/v1/device|1002|${fresh.sigPub}|${fresh.encPub}`) },
      // Another user's device.
      { by: theirs.id, sig: await signText(theirs, text) },
      { by: first.id, sig: "AAAA" },
    ]) {
      const res = await api(u, "POST", "/devices", { body: { sig_pub: fresh.sigPub, enc_pub: fresh.encPub, name: "x", endorsement } });
      expect(res.status, JSON.stringify(endorsement)).toBe(400);
    }
    const n = await env.DB.prepare("SELECT COUNT(*) AS n FROM devices WHERE github_id = 1001").first<{ n: number }>();
    expect(n?.n).toBe(1);
  });

  it("refuses keys that are not P-256 points and names that are empty or too long", async () => {
    const u = await user(1001, "octocat");
    const d = await makeDevice();
    const bad = [
      { sig_pub: "AAAA", enc_pub: d.encPub, name: "x" },
      { sig_pub: b64url(new Uint8Array(65).fill(4)), enc_pub: d.encPub, name: "x" },
      { sig_pub: d.sigPub, enc_pub: d.encPub.slice(0, -2), name: "x" },
      { sig_pub: d.sigPub + "=", enc_pub: d.encPub, name: "x" },
      { sig_pub: d.sigPub, enc_pub: d.encPub, name: "" },
      { sig_pub: d.sigPub, enc_pub: d.encPub, name: "a".repeat(101) },
      { sig_pub: d.sigPub, enc_pub: d.encPub, name: "tab\there" },
    ];
    for (const body of bad) expect((await api(u, "POST", "/devices", { body: { ...body, endorsement: null } })).status, JSON.stringify(body)).toBe(400);
    expect((await api(u, "POST", "/devices", { body: { sig_pub: d.sigPub, enc_pub: d.encPub, name: "é".repeat(100), endorsement: null } })).status).toBe(201);
  });

  it("refuses another account's key", async () => {
    const u = await user(1001, "octocat");
    const other = await user(1002, "someone");
    const d = await makeDevice();
    await expectJson(register(u, d), 201);
    await expectJson(register(other, d), 409);
  });

  it("allows at most 20 devices per user", async () => {
    const u = await user(1001, "octocat");
    const first = await makeDevice();
    await expectJson(register(u, first), 201);
    for (let i = 1; i < 20; i++) await expectJson(register(u, await makeDevice(), first), 201);
    expect((await expectJson(register(u, await makeDevice(), first), 409)).error).toMatch(/20 devices/);
  });

  it("lists a user's devices only to someone who shares a team with them", async () => {
    const t = await team();
    const m = await join(t);
    const outsider = await user(1003, "outsider");
    await expectJson(api(m.user, "GET", `/users/${t.owner.user.id}/devices`), 200);
    await expectJson(api(outsider, "GET", `/users/${t.owner.user.id}/devices`), 404);
    await expectJson(api(outsider, "GET", "/users/abc/devices"), 404);
  });
});

describe("the gate", () => {
  it("answers 401 without a session and 403 without the teams flag", async () => {
    const t = await team();
    const noFlag = await user(1009, "no-flag", false);
    const d = await makeDevice();
    const paths: [string, string][] = [
      ["POST", "/devices"],
      ["GET", "/teams"],
      ["POST", "/teams"],
      ["GET", `/teams/${t.teamId}`],
      ["GET", "/invites"],
      ["GET", `/teams/${t.teamId}/envelopes`],
      ["GET", `/teams/${t.teamId}/channels/${t.channelId}/messages`],
      ["GET", `/teams/${t.teamId}/channels/${t.channelId}/ws`],
    ];
    for (const [method, path] of paths) {
      const anonymous = await api({ ...noFlag, session: "x" }, method, path, { device: d });
      expect(anonymous.status, `${method} ${path}`).toBe(401);
      const res = await api(noFlag, method, path, { device: d, body: method === "POST" ? {} : undefined });
      expect(res.status, `${method} ${path}`).toBe(403);
      expect(await res.json()).toEqual({ error: "teams is not enabled for this account" });
    }
    // The owner loses the flag: the team is closed to them too.
    await env.DB.prepare("DELETE FROM user_flags WHERE github_id = ?").bind(t.owner.user.id).run();
    await expectJson(api(t.owner.user, "GET", `/teams/${t.teamId}`), 403);
  });

  it("answers 404 to non-members and 403 to members who are not admins", async () => {
    const t = await team();
    const m = await join(t);
    const outsider = await user(1003, "outsider");
    const od = await makeDevice();
    await expectJson(register(outsider, od), 201);
    const base = `/teams/${t.teamId}`;
    for (const [method, path] of [
      ["GET", base],
      ["GET", `${base}/envelopes`],
      ["PUT", `${base}/envelopes`],
      ["POST", `${base}/invites`],
      ["GET", `${base}/pending`],
      ["POST", `${base}/channels`],
      ["DELETE", `${base}/members/${m.user.id}`],
      ["GET", `${base}/channels/${t.channelId}/messages`],
      ["GET", `${base}/channels/${t.channelId}/ws`],
    ] as const) {
      const res = await api(outsider, method, path, { device: od, body: method === "GET" ? undefined : {} });
      expect(res.status, `${method} ${path}`).toBe(404);
    }
    // An unknown team and a malformed ID look the same as someone else's.
    await expectJson(api(outsider, "GET", `/teams/${ulid()}`), 404);
    await expectJson(api(outsider, "GET", "/teams/nope"), 404);

    for (const [method, path] of [
      ["PUT", `${base}/envelopes`],
      ["POST", `${base}/invites`],
      ["GET", `${base}/pending`],
      ["POST", `${base}/channels`],
      ["POST", `${base}/pending/${outsider.id}/approve`],
      ["POST", `${base}/pending/${outsider.id}/reject`],
      ["DELETE", `${base}/members/${t.owner.user.id}`],
    ] as const) {
      const res = await api(m.user, method, path, { device: m.device, body: method === "GET" ? undefined : {} });
      expect(res.status, `${method} ${path}`).toBe(403);
    }
  });

  it("wants X-Arakun-Device to be one of the caller's devices", async () => {
    const t = await team();
    const m = await join(t);
    const path = `/teams/${t.teamId}/envelopes`;
    await expectJson(api(m.user, "GET", path), 400);
    await expectJson(api(m.user, "GET", path, { device: t.owner.device }), 403);
    await expectJson(api(m.user, "GET", path, { device: "not a device" }), 403);
  });
});

describe("teams", () => {
  it("makes the creator the owner at epoch 1, with a rotation pending until the first key", async () => {
    const u = await user(1001, "octocat");
    const d = await makeDevice();
    await expectJson(register(u, d), 201);
    const created = await expectJson<{ team_id: string; epoch: number }>(api(u, "POST", "/teams", { body: { name: "  Arakun  " } }), 201);
    expect(created.epoch).toBe(1);
    expect(await expectJson(api(u, "GET", "/teams"), 200)).toEqual([{ team_id: created.team_id, name: "Arakun", role: "owner", epoch: 1, rotation_pending: true }]);
    await expectJson(api(u, "PUT", `/teams/${created.team_id}/envelopes`, { device: d, body: { envelopes: [await envelope(created.team_id, 1, newKey(), d, d)] } }), 200);
    const got = await expectJson(api(u, "GET", `/teams/${created.team_id}`), 200);
    expect(got).toEqual({
      team_id: created.team_id,
      name: "Arakun",
      epoch: 1,
      rotation_pending: false,
      members: [{ github_id: 1001, login: "octocat", role: "owner", joined_at: expect.any(Number) }],
      channels: [],
    });
    await expectJson(api(u, "POST", "/teams", { body: { name: "x".repeat(101) } }), 400);
  });

  it("runs the whole flow: invite, accept, pending, approve with envelopes, member", async () => {
    const t = await team();
    const owner = t.owner;
    const newcomer = await user(2001, "RikVisser-dev");
    const phone = await makeDevice();
    const laptop = await makeDevice();
    await expectJson(register(newcomer, laptop), 201);
    await expectJson(register(newcomer, phone, laptop, "phone"), 201);

    // Logins ignore case.
    const { invite_id } = await expectJson(api(owner.user, "POST", `/teams/${t.teamId}/invites`, { body: { github_login: "rikvisser-DEV" } }), 201);
    const invites = await expectJson(api(newcomer, "GET", "/invites"), 200);
    expect(invites).toEqual([
      { invite_id, team_id: t.teamId, team_name: "Arakun", invited_by_login: owner.user.login, created_at: expect.any(Number), expires_at: expect.any(Number) },
    ]);
    // Not visible to anyone else, nor theirs to accept.
    const other = await user(2002, "other");
    const od = await makeDevice();
    await expectJson(register(other, od), 201);
    expect(await expectJson(api(other, "GET", "/invites"), 200)).toEqual([]);
    await expectJson(api(other, "POST", `/invites/${invite_id}/accept`, { device: od }), 404);

    // Accepting needs a registered device.
    await expectJson(api(newcomer, "POST", `/invites/${invite_id}/accept`), 400);
    expect(await expectJson(api(newcomer, "POST", `/invites/${invite_id}/accept`, { device: laptop }), 200)).toEqual({ team_id: t.teamId, status: "pending" });
    expect(await expectJson(api(newcomer, "GET", "/invites"), 200)).toEqual([]);
    // Pending is not a member yet.
    await expectJson(api(newcomer, "GET", `/teams/${t.teamId}`), 404);
    expect(await expectJson(api(newcomer, "GET", "/teams"), 200)).toEqual([]);

    const pending = await expectJson<{ github_id: number; login: string; devices: { device_id: string }[] }[]>(api(owner.user, "GET", `/teams/${t.teamId}/pending`), 200);
    expect(pending).toHaveLength(1);
    expect(pending[0]).toMatchObject({ github_id: 2001, login: "RikVisser-dev", requested_at: expect.any(Number) });
    expect(pending[0]!.devices.map((d) => d.device_id).sort()).toEqual([laptop.id, phone.id].sort());

    const key = t.keys.get(1)!;
    const approvePath = `/teams/${t.teamId}/pending/2001/approve`;
    // A device left out is refused.
    await expectJson(api(owner.user, "POST", approvePath, { device: owner.device, body: { envelopes: [await envelope(t.teamId, 1, key, laptop, owner.device)] } }), 400);
    // Signed by a device that is not the caller's.
    const forger = await makeDevice();
    const forged = [await envelope(t.teamId, 1, key, laptop, forger), await envelope(t.teamId, 1, key, phone, forger)].map((e) => ({ ...e, from: owner.device.id }));
    await expectJson(api(owner.user, "POST", approvePath, { device: owner.device, body: { envelopes: forged } }), 400);
    // For a device that is not theirs.
    await expectJson(
      api(owner.user, "POST", approvePath, {
        device: owner.device,
        body: { envelopes: [await envelope(t.teamId, 1, key, laptop, owner.device), await envelope(t.teamId, 1, key, od, owner.device)] },
      }),
      400,
    );
    // An epoch without a key yet.
    await expectJson(
      api(owner.user, "POST", approvePath, {
        device: owner.device,
        body: { envelopes: [await envelope(t.teamId, 2, key, laptop, owner.device), await envelope(t.teamId, 2, key, phone, owner.device)] },
      }),
      400,
    );
    await expectJson(api(newcomer, "GET", `/teams/${t.teamId}`), 404);

    const good = [await envelope(t.teamId, 1, key, laptop, owner.device), await envelope(t.teamId, 1, key, phone, owner.device)];
    expect(await expectJson(api(owner.user, "POST", approvePath, { device: owner.device, body: { envelopes: good } }), 200)).toEqual({ github_id: 2001, role: "member" });

    expect(await expectJson(api(newcomer, "GET", "/teams"), 200)).toEqual([{ team_id: t.teamId, name: "Arakun", role: "member", epoch: 1, rotation_pending: false }]);
    const got = await expectJson<{ members: { github_id: number; role: string }[] }>(api(newcomer, "GET", `/teams/${t.teamId}`), 200);
    expect(got.members.map((m) => [m.github_id, m.role])).toEqual([
      [owner.user.id, "owner"],
      [2001, "member"],
    ]);
    expect(await expectJson(api(owner.user, "GET", `/teams/${t.teamId}/pending`), 200)).toEqual([]);
    expect(await expectJson(api(newcomer, "GET", `/teams/${t.teamId}/envelopes`, { device: phone }), 200)).toEqual([good[1]]);
    // Invited again once a member: refused.
    await expectJson(api(owner.user, "POST", `/teams/${t.teamId}/invites`, { body: { github_login: "rikvisser-dev" } }), 409);
  });

  it("lets an admin reject a request", async () => {
    const t = await team();
    const u = await user(2001, "asker");
    const d = await makeDevice();
    await expectJson(register(u, d), 201);
    const { invite_id } = await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/invites`, { body: { github_login: "asker" } }), 201);
    await expectJson(api(u, "POST", `/invites/${invite_id}/accept`, { device: d }), 200);
    expect((await api(t.owner.user, "POST", `/teams/${t.teamId}/pending/2001/reject`)).status).toBe(204);
    expect(await expectJson(api(t.owner.user, "GET", `/teams/${t.teamId}/pending`), 200)).toEqual([]);
    await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/pending/2001/reject`), 404);
    await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/pending/2001/approve`, { device: t.owner.device, body: { envelopes: [] } }), 404);
  });

  it("lets invites expire after 7 days", async () => {
    const t = await team();
    const u = await user(2001, "late");
    const d = await makeDevice();
    await expectJson(register(u, d), 201);
    const { invite_id, expires_at } = await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/invites`, { body: { github_login: "late" } }), 201);
    expect(expires_at - nowSeconds()).toBeGreaterThan(7 * 86400 - 60);
    expect(expires_at - nowSeconds()).toBeLessThanOrEqual(7 * 86400);
    await env.DB.prepare("UPDATE invites SET expires_at = ?").bind(nowSeconds() - 1).run();
    expect(await expectJson(api(u, "GET", "/invites"), 200)).toEqual([]);
    await expectJson(api(u, "POST", `/invites/${invite_id}/accept`, { device: d }), 404);
  });
});

describe("envelopes", () => {
  it("are fetched per device", async () => {
    const t = await team();
    const owner = t.owner;
    const second = await makeDevice();
    await expectJson(register(owner.user, second, owner.device, "desktop"), 201);
    const key = t.keys.get(1)!;
    const forSecond = await envelope(t.teamId, 1, key, second, owner.device);
    await expectJson(api(owner.user, "PUT", `/teams/${t.teamId}/envelopes`, { device: owner.device, body: { envelopes: [forSecond] } }), 200);

    const mine = await expectJson<{ device_id: string }[]>(api(owner.user, "GET", `/teams/${t.teamId}/envelopes`, { device: owner.device }), 200);
    expect(mine.map((e) => e.device_id)).toEqual([owner.device.id]);
    expect(await expectJson(api(owner.user, "GET", `/teams/${t.teamId}/envelopes`, { device: second }), 200)).toEqual([forSecond]);
    // A member cannot ask for someone else's device's envelopes.
    const m = await join(t);
    await expectJson(api(m.user, "GET", `/teams/${t.teamId}/envelopes`, { device: second }), 403);
  });

  it("are refused when they come from another device or carry a bad signature", async () => {
    const t = await team();
    const owner = t.owner;
    const second = await makeDevice();
    await expectJson(register(owner.user, second, owner.device), 201);
    const key = t.keys.get(1)!;
    const path = `/teams/${t.teamId}/envelopes`;
    // Signed by the second device, uploaded as the first.
    await expectJson(api(owner.user, "PUT", path, { device: owner.device, body: { envelopes: [await envelope(t.teamId, 1, key, second, second)] } }), 400);
    const e = await envelope(t.teamId, 1, key, second, owner.device);
    const flipped = { ...e, ct: b64url(new Uint8Array(48)) };
    await expectJson(api(owner.user, "PUT", path, { device: owner.device, body: { envelopes: [flipped] } }), 400);
    await expectJson(api(owner.user, "PUT", path, { device: owner.device, body: { envelopes: [{ ...e, team_id: ulid() }] } }), 400);
    await expectJson(api(owner.user, "PUT", path, { device: owner.device, body: { envelopes: [e, e] } }), 400);
    await expectJson(api(owner.user, "PUT", path, { device: owner.device, body: { envelopes: [e] } }), 200);
  });

  it("for a new epoch must cover every member device at once", async () => {
    const t = await team();
    const m = await join(t);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${t.owner.user.id}`), 409);
    const third = await join(t);
    const removed = await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${third.user.id}`), 200);
    expect(removed).toEqual({ epoch: 2, rotation_pending: true });
    const key2 = newKey();
    const path = `/teams/${t.teamId}/envelopes`;
    const onlyOwner = await envelope(t.teamId, 2, key2, t.owner.device, t.owner.device);
    await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: [onlyOwner] } }), 400);
    // The removed member's device is not a member's any more.
    const toRemoved = await envelope(t.teamId, 2, key2, third.device, t.owner.device);
    const toMember = await envelope(t.teamId, 2, key2, m.device, t.owner.device);
    await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: [onlyOwner, toMember, toRemoved] } }), 400);
    expect(await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: [onlyOwner, toMember] } }), 200)).toEqual({
      stored: 2,
      epoch: 2,
      rotation_pending: false,
    });
    // Epoch 3 does not exist yet.
    await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: [await envelope(t.teamId, 3, newKey(), m.device, t.owner.device)] } }), 400);
  });

  it("refuse an eph_pub that is not on the curve, even signed", async () => {
    const t = await team();
    const e = await envelope(t.teamId, 1, t.keys.get(1)!, t.owner.device, t.owner.device);
    const off = new Uint8Array(65).fill(1);
    off[0] = 4;
    const eph_pub = b64url(off);
    const sig = await signText(t.owner.device, `arakun-teams/v1/envelope|${t.teamId}|1|${t.owner.device.id}|${eph_pub}|${e["nonce"]}|${e["ct"]}`);
    const res = await expectJson(api(t.owner.user, "PUT", `/teams/${t.teamId}/envelopes`, { device: t.owner.device, body: { envelopes: [{ ...e, eph_pub, sig }] } }), 400);
    expect(res.error).toMatch(/eph_pub/);
  });

  it("of a key made too late do not replace the key in use", async () => {
    const t = await team();
    const second = await makeDevice();
    await expectJson(register(t.owner.user, second, t.owner.device, "desktop"), 201);
    const m = await join(t);
    const gone = await join(t);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${gone.user.id}`), 200);
    const path = `/teams/${t.teamId}/envelopes`;
    const devices = [t.owner.device, second, m.device];
    const won = newKey();
    const first = await Promise.all(devices.map((d) => envelope(t.teamId, 2, won, d, t.owner.device)));
    expect(await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: first } }), 200)).toMatchObject({ stored: 3, rotation_pending: false });
    // The second device made its own key for epoch 2 before it saw the first.
    const late = await Promise.all(devices.map((d) => envelope(t.teamId, 2, newKey(), d, second)));
    expect(await expectJson(api(t.owner.user, "PUT", path, { device: second, body: { envelopes: late } }), 200)).toMatchObject({ stored: 0 });
    const mine = await expectJson<{ epoch: number; from: string }[]>(api(m.user, "GET", path, { device: m.device }), 200);
    expect(mine.find((e) => e.epoch === 2)).toEqual(first[2]);
  });

  it("are asked only for epochs with a key when two removals skipped one", async () => {
    const t = await team();
    const a = await join(t);
    const b = await join(t);
    await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${a.user.id}`), 200);
    expect(await expectJson(api(t.owner.user, "DELETE", `/teams/${t.teamId}/members/${b.user.id}`), 200)).toEqual({ epoch: 3, rotation_pending: true });
    const path = `/teams/${t.teamId}/envelopes`;
    const key3 = newKey();
    await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: [await envelope(t.teamId, 3, key3, t.owner.device, t.owner.device)] } }), 200);
    // Epoch 2 was skipped: nothing may be sealed with it.
    await expectJson(api(t.owner.user, "PUT", path, { device: t.owner.device, body: { envelopes: [await envelope(t.teamId, 2, newKey(), t.owner.device, t.owner.device)] } }), 400);
    // A newcomer gets epochs 1 and 3, not 2.
    t.keys.set(3, key3);
    const c = await join(t);
    const theirs = await expectJson<{ epoch: number }[]>(api(c.user, "GET", path, { device: c.device }), 200);
    expect(theirs.map((e) => e.epoch)).toEqual([1, 3]);
  });
});

describe("members", () => {
  it("are at most 25 per team", async () => {
    const t = await team();
    const t0 = nowSeconds();
    for (let i = 0; i < 24; i++) {
      await env.DB.batch([
        env.DB.prepare("INSERT INTO users (github_id, login, created_at, last_seen_at) VALUES (?, ?, ?, ?)").bind(9000 + i, `filler-${i}`, t0, t0),
        env.DB.prepare("INSERT INTO members (team_id, github_id, role, joined_at) VALUES (?, ?, 'member', ?)").bind(t.teamId, 9000 + i, t0),
      ]);
    }
    await expect(join(t)).rejects.toThrow(/expected 200, got 409.*at most 25 members/);
  });
});

describe("channels", () => {
  it("are created by admins with a sealed name, once per ID", async () => {
    const t = await team();
    const key = t.keys.get(1)!;
    const id = ulid();
    const name_ct = await seal(key, 1, `arakun-teams/v1/channel|${t.teamId}|${id}|${id}`, "random");
    const path = `/teams/${t.teamId}/channels`;
    expect(await expectJson(api(t.owner.user, "POST", path, { body: { channel_id: id, name_ct } }), 201)).toEqual({ channel_id: id });
    await expectJson(api(t.owner.user, "POST", path, { body: { channel_id: id, name_ct } }), 409);
    await expectJson(api(t.owner.user, "POST", path, { body: { channel_id: "general", name_ct } }), 400);
    await expectJson(api(t.owner.user, "POST", path, { body: { channel_id: ulid(), name_ct: { ...name_ct, v: 2 } } }), 400);
    await expectJson(api(t.owner.user, "POST", path, { body: { channel_id: ulid(), name_ct: { ...name_ct, epoch: 2 } } }), 400);
    const got = await expectJson<{ channels: { channel_id: string; name_ct: unknown }[] }>(api(t.owner.user, "GET", `/teams/${t.teamId}`), 200);
    expect(got.channels.map((c) => c.channel_id).sort()).toEqual([t.channelId, id].sort());
    expect(got.channels.find((c) => c.channel_id === id)?.name_ct).toEqual(name_ct);
  });

  it("are at most 500 per team", async () => {
    const t = await team();
    const name_ct = await seal(t.keys.get(1)!, 1, "x", "n");
    const stmt = env.DB.prepare("INSERT INTO channels (team_id, channel_id, name_ct, created_by, created_at) VALUES (?, ?, ?, 1, 0)");
    await env.DB.batch(Array.from({ length: 499 }, () => stmt.bind(t.teamId, ulid(), JSON.stringify(name_ct))));
    const res = await expectJson(api(t.owner.user, "POST", `/teams/${t.teamId}/channels`, { body: { channel_id: ulid(), name_ct } }), 409);
    expect(res.error).toMatch(/500 channels/);
  });
});
