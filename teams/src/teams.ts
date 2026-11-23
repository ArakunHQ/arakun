// Devices, teams, invites, envelopes, members and channels: the endpoints of
// PROTOCOL.md. Every one needs a session and the user's `teams` flag. Team
// endpoints answer 404 to non-members, so a team's existence does not leak,
// and 403 to members who are not an owner or admin where one is needed. The
// one exception is remind, which only pending users may call.
//
// The server never holds a team key. What it checks is who may store what:
// device endorsements, the signatures on envelopes and messages, and that
// every device gets the keys it should.

import { githubAccount, session, type Session } from "./auth";
import { TRUSTED, epochAllowed, epochRefused } from "./channel";
import { DAY, GITHUB_ID, GITHUB_LOGIN, TEAMS_FLAG as FLAG, ULID, error, fromB64url, isObject, jsonBody, now, ulid, type Env } from "./util";
import {
  DEVICE_ID,
  deviceIdOf,
  deviceSigned,
  envelopeSigned,
  isEncKey,
  parseEnvelope,
  parseSealedName,
  sigKey,
  verify,
  type Envelope,
} from "./wire";

const DEVICE_HEADER = "X-Arakun-Device";
const MAX_DEVICES = 20;
/// With 20 devices each, a rotation is at most 500 envelopes: one request.
const MAX_MEMBERS = 25;
const MAX_CHANNELS = 500;
/// Teams a user may be in when they create one, so one account cannot fill
/// the database with teams.
const MAX_TEAMS = 50;
/// Everyone a user can share a team with: the most `GET /devices?users=` takes.
const MAX_USERS = MAX_TEAMS * MAX_MEMBERS;
const MAX_NAME = 100;
const INVITE_DAYS = 7;
/// A pending user may remind a team's admins once per this many seconds.
const REMIND_EVERY = 60 * 60;
/// Per request: a rotation of MAX_MEMBERS with 20 devices each, or one new
/// device's keys for 500 epochs. D1 runs them as one batch. A larger grant of
/// history goes in parts: the approval with the newest epochs, then uploads.
const MAX_ENVELOPES = 500;
const MAX_BODY = 1024 * 1024;
const HISTORY_LIMIT = 200;

type Role = "owner" | "admin" | "member";

interface Ctx {
  request: Request;
  env: Env;
  url: URL;
  me: Session;
}

interface Team {
  team_id: string;
  name: string;
  epoch: number;
  keyed_epoch: number;
  role: Role;
}

interface Device {
  device_id: string;
  sig_pub: string;
}

type Handler = (c: Ctx, ...params: string[]) => Promise<Response>;

const ROUTES: [string, RegExp, Handler][] = [
  ["POST", /^\/devices$/, registerDevice],
  ["GET", /^\/devices$/, listPeopleDevices],
  ["GET", /^\/users\/([^/]+)\/devices$/, listDevices],
  ["POST", /^\/teams$/, createTeam],
  ["GET", /^\/teams$/, listTeams],
  ["GET", /^\/teams\/([^/]+)$/, getTeam],
  ["POST", /^\/teams\/([^/]+)\/invites$/, invite],
  ["GET", /^\/teams\/([^/]+)\/invites$/, teamInvites],
  ["DELETE", /^\/teams\/([^/]+)\/invites\/([^/]+)$/, withdraw],
  ["GET", /^\/invites$/, listInvites],
  ["POST", /^\/invites\/([^/]+)\/accept$/, accept],
  ["POST", /^\/invites\/([^/]+)\/decline$/, decline],
  ["GET", /^\/pending$/, myRequests],
  ["POST", /^\/teams\/([^/]+)\/remind$/, remind],
  ["GET", /^\/teams\/([^/]+)\/pending$/, listPending],
  ["POST", /^\/teams\/([^/]+)\/pending\/([^/]+)\/approve$/, approve],
  ["POST", /^\/teams\/([^/]+)\/pending\/([^/]+)\/reject$/, reject],
  ["PUT", /^\/teams\/([^/]+)\/envelopes$/, putEnvelopes],
  ["GET", /^\/teams\/([^/]+)\/envelopes$/, getEnvelopes],
  ["DELETE", /^\/teams\/([^/]+)\/members\/([^/]+)$/, removeMember],
  ["POST", /^\/teams\/([^/]+)\/channels$/, createChannel],
  ["PUT", /^\/teams\/([^/]+)\/channels\/([^/]+)$/, renameChannel],
  ["GET", /^\/teams\/([^/]+)\/channels\/([^/]+)\/ws$/, openSocket],
  ["GET", /^\/teams\/([^/]+)\/channels\/([^/]+)\/messages$/, history],
  ["GET", /^\/teams\/([^/]+)\/channels\/([^/]+)\/senders\/([^/]+)$/, sender],
];

/// Answers a teams request, or null when the path is not one of ours.
export async function teams(request: Request, env: Env, url: URL): Promise<Response | null> {
  for (const [method, pattern, handler] of ROUTES) {
    const match = pattern.exec(url.pathname);
    if (!match || request.method !== method) continue;
    const me = await session(request, env, { flag: FLAG });
    if (!me) return error(401, "not signed in");
    if (!me.flagged) return error(403, "teams is not enabled for this account");
    return handler({ request, env, url, me }, ...match.slice(1));
  }
  return null;
}

const notFound = () => error(404, "not found");
const notAdmin = () => error(403, "only an owner or admin can do this");

function isAdmin(role: Role): boolean {
  return role === "owner" || role === "admin";
}

/// The team, if the caller is a member of it, as a statement to run alone or
/// in a batch. Every check of who may see a team goes through here.
function teamQuery(c: Ctx, teamId: string): D1PreparedStatement {
  return c.env.DB.prepare(
    `SELECT t.team_id, t.name, t.epoch, t.keyed_epoch, m.role
     FROM members m JOIN teams t ON t.team_id = m.team_id
     WHERE m.team_id = ? AND m.github_id = ?`,
  ).bind(teamId, c.me.githubId);
}

/// The team, if the caller is a member of it.
async function teamOf(c: Ctx, teamId: string): Promise<Team | null> {
  if (!ULID.test(teamId)) return null;
  return teamQuery(c, teamId).first<Team>();
}

/// The device the request names in X-Arakun-Device, if it is the caller's.
async function callerDevice(c: Ctx): Promise<Device | Response> {
  const id = c.request.headers.get(DEVICE_HEADER);
  if (!id) return error(400, `${DEVICE_HEADER} is required`);
  const row = DEVICE_ID.test(id)
    ? await c.env.DB.prepare("SELECT device_id, sig_pub FROM devices WHERE device_id = ? AND github_id = ?").bind(id, c.me.githubId).first<Device>()
    : null;
  return row ?? error(403, `${DEVICE_HEADER} is not one of your devices`);
}

/// A name of 1 to 100 characters without control characters, trimmed.
function cleanName(v: unknown): string | null {
  if (typeof v !== "string") return null;
  const name = v.trim();
  const length = [...name].length;
  return length >= 1 && length <= MAX_NAME && !/\p{Cc}/u.test(name) ? name : null;
}

// ---- devices ---------------------------------------------------------------

async function registerDevice(c: Ctx): Promise<Response> {
  const body = await jsonBody(c.request, 4096);
  if (!isObject(body)) return error(400, 'expected {"sig_pub", "enc_pub", "name", "endorsement"}');
  const { sig_pub: sigPub, enc_pub: encPub, endorsement } = body;
  const name = cleanName(body["name"]);
  if (name === null) return error(400, `name is 1 to ${MAX_NAME} characters`);
  if (typeof sigPub !== "string" || !(await sigKey(sigPub))) return error(400, "sig_pub is not a P-256 public key");
  if (typeof encPub !== "string" || !(await isEncKey(encPub))) return error(400, "enc_pub is not a P-256 public key");
  const me = c.me.githubId;
  const deviceId = await deviceIdOf(fromB64url(sigPub)!);
  const db = c.env.DB;

  const known = () => db.prepare("SELECT github_id, enc_pub FROM devices WHERE device_id = ?").bind(deviceId).first<{ github_id: number; enc_pub: string }>();
  const same = (row: { github_id: number; enc_pub: string }) =>
    row.github_id === me && row.enc_pub === encPub ? Response.json({ device_id: deviceId }) : error(409, "this key is registered already");
  const existing = await known();
  if (existing) return same(existing);

  let endorsedBy: string | null = null;
  let endorsementSig: string | null = null;
  if (endorsement !== null && endorsement !== undefined) {
    if (!isObject(endorsement) || typeof endorsement["by"] !== "string" || typeof endorsement["sig"] !== "string") {
      return error(400, 'endorsement is null or {"by", "sig"}');
    }
    const by = await db
      .prepare("SELECT sig_pub FROM devices WHERE device_id = ? AND github_id = ?")
      .bind(endorsement["by"], me)
      .first<{ sig_pub: string }>();
    if (!by) return error(400, "the endorsing device is not one of yours");
    const key = await sigKey(by.sig_pub);
    if (!key || !(await verify(key, endorsement["sig"], deviceSigned(me, sigPub, encPub)))) {
      return error(400, "the endorsement does not check out");
    }
    endorsedBy = endorsement["by"];
    endorsementSig = endorsement["sig"];
  }

  // The conditions sit in the insert, so two registrations at once cannot
  // both be a first device, or both be the 21st.
  const condition =
    endorsedBy === null
      ? "NOT EXISTS (SELECT 1 FROM devices WHERE github_id = ?2)"
      : `(SELECT COUNT(*) FROM devices WHERE github_id = ?2) < ${MAX_DEVICES}`;
  const result = await db
    .prepare(
      `INSERT INTO devices (device_id, github_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig)
       SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 WHERE ${condition} ON CONFLICT DO NOTHING`,
    )
    .bind(deviceId, me, sigPub, encPub, name, now(), endorsedBy, endorsementSig)
    .run();
  if (result.meta.changes === 1) return Response.json({ device_id: deviceId }, { status: 201 });
  const raced = await known();
  if (raced) return same(raced);
  return endorsedBy === null
    ? error(400, "a new device needs an endorsement from one of your devices")
    : error(409, `at most ${MAX_DEVICES} devices per account`);
}

async function listDevices(c: Ctx, id: string): Promise<Response> {
  if (!GITHUB_ID.test(id)) return notFound();
  const githubId = Number(id);
  if (githubId !== c.me.githubId) {
    const shared = await c.env.DB.prepare(
      "SELECT 1 FROM members a JOIN members b ON b.team_id = a.team_id AND b.github_id = ? WHERE a.github_id = ? LIMIT 1",
    )
      .bind(githubId, c.me.githubId)
      .first();
    if (!shared) return notFound();
  }
  const { results } = await c.env.DB.prepare(
    "SELECT device_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig FROM devices WHERE github_id = ? ORDER BY created_at, device_id",
  )
    .bind(githubId)
    .all();
  return Response.json(results);
}

/// The devices of the caller and everyone they share a team with, or of those
/// in `?users=`, in one answer: a client refreshing its teams would otherwise
/// ask for each member, once per team. A user out of reach is left out, as
/// `GET /users/<id>/devices` answers 404 for them.
async function listPeopleDevices(c: Ctx): Promise<Response> {
  const raw = c.url.searchParams.get("users");
  let wanted: string | null = null;
  if (raw !== null) {
    const ids = raw === "" ? [] : raw.split(",");
    if (ids.length > MAX_USERS || !ids.every((id) => GITHUB_ID.test(id)))
      return error(400, `users is a comma-separated list of at most ${MAX_USERS} GitHub IDs`);
    wanted = JSON.stringify(ids.map(Number));
  }
  const { results } = await c.env.DB.prepare(
    `WITH people(github_id) AS (
       SELECT ?1 UNION SELECT b.github_id FROM members a JOIN members b ON b.team_id = a.team_id WHERE a.github_id = ?1)
     SELECT p.github_id, d.device_id, d.sig_pub, d.enc_pub, d.name, d.created_at, d.endorsed_by, d.endorsement_sig
     FROM people p LEFT JOIN devices d ON d.github_id = p.github_id
     WHERE ?2 IS NULL OR p.github_id IN (SELECT value FROM json_each(?2))
     ORDER BY p.github_id, d.created_at, d.device_id`,
  )
    .bind(c.me.githubId, wanted)
    .all<{ github_id: number; device_id: string | null } & Record<string, unknown>>();
  // Each user once, one with no device too, so a client can tell "no
  // devices" from "left out".
  const people: { github_id: number; devices: unknown[] }[] = [];
  for (const { github_id, device_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig } of results) {
    if (people.at(-1)?.github_id !== github_id) people.push({ github_id, devices: [] });
    if (device_id !== null) people.at(-1)!.devices.push({ device_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig });
  }
  return Response.json(people);
}

// ---- teams -----------------------------------------------------------------

async function createTeam(c: Ctx): Promise<Response> {
  const body = await jsonBody(c.request, 4096);
  const name = isObject(body) ? cleanName(body["name"]) : null;
  if (name === null) return error(400, `expected {"name": "..."} of 1 to ${MAX_NAME} characters`);
  const teamId = ulid();
  const t = now();
  const db = c.env.DB;
  // The count is checked in the insert, so requests at once cannot all pass.
  const [made] = await db.batch([
    db
      .prepare(
        `INSERT INTO teams (team_id, name, epoch, keyed_epoch, created_by, created_at)
         SELECT ?1, ?2, 1, 0, ?3, ?4 WHERE (SELECT COUNT(*) FROM members WHERE github_id = ?3) < ${MAX_TEAMS}`,
      )
      .bind(teamId, name, c.me.githubId, t),
    db
      .prepare("INSERT INTO members (team_id, github_id, role, joined_at) SELECT ?1, ?2, 'owner', ?3 WHERE EXISTS (SELECT 1 FROM teams WHERE team_id = ?1)")
      .bind(teamId, c.me.githubId, t),
  ]);
  if (made!.meta.changes !== 1) return error(409, `at most ${MAX_TEAMS} teams per account`);
  return Response.json({ team_id: teamId, epoch: 1 }, { status: 201 });
}

async function listTeams(c: Ctx): Promise<Response> {
  const { results } = await c.env.DB.prepare(
    `SELECT t.team_id, t.name, m.role, t.epoch, t.keyed_epoch
     FROM members m JOIN teams t ON t.team_id = m.team_id
     WHERE m.github_id = ? ORDER BY t.created_at, t.team_id`,
  )
    .bind(c.me.githubId)
    .all<Team>();
  return Response.json(results.map((t) => ({ team_id: t.team_id, name: t.name, role: t.role, epoch: t.epoch, rotation_pending: t.keyed_epoch < t.epoch })));
}

async function getTeam(c: Ctx, teamId: string): Promise<Response> {
  if (!ULID.test(teamId)) return notFound();
  const db = c.env.DB;
  // One snapshot: a removal that lands between reads would otherwise list
  // members that no longer match the epoch and rotation status reported.
  const [team, members, channels] = await db.batch([
    teamQuery(c, teamId),
    db
      .prepare(
        `SELECT m.github_id, u.login, m.role, m.joined_at,
         (SELECT MIN(e.epoch) FROM devices d JOIN envelopes e ON e.device_id = d.device_id AND e.team_id = m.team_id
          WHERE d.github_id = m.github_id) AS first_key_epoch FROM members m JOIN users u ON u.github_id = m.github_id
         WHERE m.team_id = ? ORDER BY m.joined_at, m.rowid`,
      )
      .bind(teamId),
    db.prepare("SELECT channel_id, name_ct, created_at FROM channels WHERE team_id = ? ORDER BY channel_id").bind(teamId),
  ]);
  const row = team!.results[0] as Team | undefined;
  if (!row) return notFound();
  return Response.json({
    team_id: row.team_id,
    name: row.name,
    epoch: row.epoch,
    roster_epoch: row.epoch,
    keyed_epoch: row.keyed_epoch,
    rotation_pending: row.keyed_epoch < row.epoch,
    members: members!.results,
    channels: (channels!.results as { channel_id: string; name_ct: string; created_at: number }[]).map((ch) => ({
      channel_id: ch.channel_id,
      name_ct: JSON.parse(ch.name_ct),
      created_at: ch.created_at,
    })),
  });
}

// ---- invites and approval ----------------------------------------------------

async function invite(c: Ctx, teamId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  const body = await jsonBody(c.request, 4096);
  const given = isObject(body) ? body["github_login"] : null;
  if (typeof given !== "string" || !GITHUB_LOGIN.test(given)) return error(400, 'expected {"github_login": "..."}');
  let limited: boolean;
  try {
    limited = !(await c.env.INVITE_LIMIT.limit({ key: String(c.me.githubId) })).success;
  } catch (e) {
    console.error("invite limit failed:", e);
    return error(503, "the server could not check the invite: try again");
  }
  if (limited) return error(429, "too many invites, try again in a minute");
  // The invite names the account that has the login now, by its ID. The
  // login we hold for a user is only as new as their last sign-in, so a user
  // who gave up this login could otherwise see and accept the invite.
  const account = await githubAccount(given, c.env);
  if (account === "missing") return error(404, `no GitHub user has the login ${given}`);
  if (account === "unreachable") return error(502, "could not reach GitHub");
  const login = account.login.toLowerCase();
  const db = c.env.DB;
  const already = await db
    .prepare(
      `SELECT 1 WHERE EXISTS (SELECT 1 FROM members WHERE team_id = ?1 AND github_id = ?2)
         OR EXISTS (SELECT 1 FROM pending WHERE team_id = ?1 AND github_id = ?2)`,
    )
    .bind(teamId, account.id)
    .first();
  if (already) return error(409, "already a member of this team, or waiting for approval");
  const inviteId = ulid();
  const t = now();
  const expiresAt = t + INVITE_DAYS * DAY;
  await db.batch([
    db.prepare("DELETE FROM invites WHERE expires_at <= ?").bind(t),
    // One open invite per person: one under a login they had before goes.
    db.prepare("DELETE FROM invites WHERE team_id = ? AND github_id = ? AND login <> ?").bind(teamId, account.id, login),
    // Inviting again replaces the open invite, with a new ID and expiry.
    db
      .prepare(
        `INSERT INTO invites (invite_id, team_id, login, github_id, invited_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (team_id, login) DO UPDATE SET invite_id = excluded.invite_id, github_id = excluded.github_id,
           invited_by = excluded.invited_by, created_at = excluded.created_at, expires_at = excluded.expires_at`,
      )
      .bind(inviteId, teamId, login, account.id, c.me.githubId, t, expiresAt),
  ]);
  return Response.json({ invite_id: inviteId, expires_at: expiresAt, github_id: account.id, login: account.login }, { status: 201 });
}

async function teamInvites(c: Ctx, teamId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  const { results } = await c.env.DB.prepare(
    `SELECT i.invite_id, i.login, u.login AS invited_by_login, i.created_at, i.expires_at
     FROM invites i LEFT JOIN users u ON u.github_id = i.invited_by
     WHERE i.team_id = ? AND i.expires_at > ? ORDER BY i.created_at DESC, i.invite_id DESC`,
  )
    .bind(teamId, now())
    .all();
  return Response.json(results);
}

async function withdraw(c: Ctx, teamId: string, inviteId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  // Scoped to the team, so an admin of one team cannot withdraw another's.
  const result = ULID.test(inviteId)
    ? await c.env.DB.prepare("DELETE FROM invites WHERE invite_id = ? AND team_id = ? AND expires_at > ?").bind(inviteId, teamId, now()).run()
    : null;
  if (!result?.meta.changes) return error(404, "no such invite");
  return new Response(null, { status: 204 });
}

async function listInvites(c: Ctx): Promise<Response> {
  // member_count leaves out pending ones: they live in their own table.
  const { results } = await c.env.DB.prepare(
    `SELECT i.invite_id, i.team_id, t.name AS team_name, u.login AS invited_by_login, i.created_at, i.expires_at,
       (SELECT COUNT(*) FROM members m WHERE m.team_id = i.team_id) AS member_count
     FROM invites i JOIN teams t ON t.team_id = i.team_id LEFT JOIN users u ON u.github_id = i.invited_by
     WHERE i.github_id = ? AND i.expires_at > ? ORDER BY i.created_at, i.invite_id`,
  )
    .bind(c.me.githubId, now())
    .all();
  return Response.json(results);
}

async function decline(c: Ctx, inviteId: string): Promise<Response> {
  // Someone else's invite is answered like one that does not exist, as in accept.
  const result = ULID.test(inviteId)
    ? await c.env.DB.prepare("DELETE FROM invites WHERE invite_id = ? AND github_id = ? AND expires_at > ?")
        .bind(inviteId, c.me.githubId, now())
        .run()
    : null;
  if (!result?.meta.changes) return error(404, "no such invite");
  return new Response(null, { status: 204 });
}

async function accept(c: Ctx, inviteId: string): Promise<Response> {
  const device = await callerDevice(c);
  if (device instanceof Response) return device;
  if (!ULID.test(inviteId)) return notFound();
  const db = c.env.DB;
  const inv = await db
    .prepare("SELECT team_id, github_id, invited_by, expires_at FROM invites WHERE invite_id = ?")
    .bind(inviteId)
    .first<{ team_id: string; github_id: number; invited_by: number; expires_at: number }>();
  // Someone else's invite is answered like one that does not exist.
  if (!inv || inv.github_id !== c.me.githubId || inv.expires_at <= now()) return error(404, "no such invite");
  const member = await db.prepare("SELECT 1 FROM members WHERE team_id = ? AND github_id = ?").bind(inv.team_id, c.me.githubId).first();
  if (member) return error(409, "already a member of this team");
  // The invite and the membership are checked again in the batch: a replaced
  // or expired invite makes no pending row, and an approval that landed since
  // the read above must not leave the user in members and pending at once.
  // The insert's outcome, not the invite's deletion, says it was accepted.
  const t = now();
  const valid = "FROM invites WHERE invite_id = ?1 AND github_id = ?2 AND expires_at > ?3";
  const results = await db.batch([
    db
      .prepare(
        `INSERT INTO pending (team_id, github_id, invited_by, requested_at, request_id, invited_login)
         SELECT team_id, ?2, invited_by, ?3, invite_id, login ${valid}
           AND NOT EXISTS (SELECT 1 FROM members m WHERE m.team_id = invites.team_id AND m.github_id = ?2)
         ON CONFLICT DO NOTHING`,
      )
      .bind(inviteId, c.me.githubId, t),
    db.prepare(`DELETE ${valid}`).bind(inviteId, c.me.githubId, t),
  ]);
  if (results[1]!.meta.changes !== 1) return error(404, "no such invite");
  if (results[0]!.meta.changes !== 1) return error(409, "already a member of this team");
  return Response.json({ team_id: inv.team_id, status: "pending" });
}

async function listPending(c: Ctx, teamId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  const db = c.env.DB;
  const [people, devices] = await db.batch([
    db
      .prepare(
        `SELECT p.github_id, u.login, p.invited_login, p.request_id, p.requested_at, p.reminded_at FROM pending p JOIN users u ON u.github_id = p.github_id
         WHERE p.team_id = ? ORDER BY p.requested_at, p.github_id`,
      )
      .bind(teamId),
    db
      .prepare(
        `SELECT d.github_id, d.device_id, d.sig_pub, d.enc_pub, d.name, d.created_at, d.endorsed_by, d.endorsement_sig
         FROM devices d JOIN pending p ON p.github_id = d.github_id
         WHERE p.team_id = ? ORDER BY d.created_at, d.device_id`,
      )
      .bind(teamId),
  ]);
  const byUser = new Map<number, unknown[]>();
  for (const { github_id, ...device } of devices!.results as { github_id: number }[]) {
    byUser.set(github_id, [...(byUser.get(github_id) ?? []), device]);
  }
  return Response.json(
    (people!.results as { github_id: number }[]).map((p) => ({
      ...p,
      devices: byUser.get(p.github_id) ?? [],
    })),
  );
}

/// The caller's own requests to join, so a client finds them again after a
/// restart.
async function myRequests(c: Ctx): Promise<Response> {
  const { results } = await c.env.DB.prepare(
    `SELECT p.team_id, t.name AS team_name, p.requested_at, u.login AS invited_by_login, p.invited_by AS invited_by_id, p.reminded_at
     FROM pending p JOIN teams t ON t.team_id = p.team_id LEFT JOIN users u ON u.github_id = p.invited_by
     WHERE p.github_id = ? ORDER BY p.requested_at, p.team_id`,
  )
    .bind(c.me.githubId)
    .all();
  return Response.json(results);
}

/// The one team endpoint a pending user may call. It does not go through
/// teamOf, which only knows members: the pending row is the check, and a
/// member (who has no business reminding anyone) gets the same 404 as a
/// stranger.
async function remind(c: Ctx, teamId: string): Promise<Response> {
  if (!ULID.test(teamId)) return notFound();
  const db = c.env.DB;
  const t = now();
  // The hour is checked in the update, so two reminders at once set it once.
  // The read is in the same batch, so it sees the request the update saw: a
  // request rejected and made again in between cannot answer 429 by mistake.
  const [result, read] = await db.batch([
    db
      .prepare(
        `UPDATE pending SET reminded_at = ?3 WHERE team_id = ?1 AND github_id = ?2
           AND (reminded_at IS NULL OR reminded_at <= ?3 - ${REMIND_EVERY})
           AND NOT EXISTS (SELECT 1 FROM members WHERE team_id = ?1 AND github_id = ?2)`,
      )
      .bind(teamId, c.me.githubId, t),
    db
      .prepare(
        `SELECT reminded_at FROM pending WHERE team_id = ?1 AND github_id = ?2
           AND NOT EXISTS (SELECT 1 FROM members WHERE team_id = ?1 AND github_id = ?2)`,
      )
      .bind(teamId, c.me.githubId),
  ]);
  if (result!.meta.changes === 1) return new Response(null, { status: 204 });
  const row = read!.results[0] as { reminded_at: number | null } | undefined;
  if (!row) return notFound();
  const retryAt = (row.reminded_at ?? t) + REMIND_EVERY;
  const minutes = Math.max(1, Math.ceil((retryAt - t) / 60));
  return Response.json(
    { error: `you reminded this team in the last hour: try again in ${minutes} min`, retry_at: retryAt },
    { status: 429, headers: { "Retry-After": String(Math.max(0, retryAt - t)) } },
  );
}

/// SQL that holds while the session `session` is live at time `t`, its user
/// has the flag, and is an owner or admin of team `team`. For a write that
/// must not commit after the caller lost any of them while it ran.
function stillAdmin(team: string, session: string, t: string): string {
  return `EXISTS (SELECT 1 FROM sessions s
    JOIN members a ON a.github_id = s.github_id AND a.team_id = ${team} AND a.role IN ('owner', 'admin')
    JOIN user_flags f ON f.github_id = s.github_id AND f.flag = '${FLAG}'
    WHERE s.token_hash = ${session} AND s.expires_at > ${t})`;
}

async function approve(c: Ctx, teamId: string, id: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  const device = await callerDevice(c);
  if (device instanceof Response) return device;
  if (!GITHUB_ID.test(id)) return error(404, "no such request");
  const githubId = Number(id);
  const db = c.env.DB;
  const pending = await db.prepare("SELECT request_id FROM pending WHERE team_id = ? AND github_id = ?").bind(teamId, githubId).first<{ request_id: string }>();
  if (!pending) return error(404, "no such request");
  const body = await jsonBody(c.request, MAX_BODY);
  // The request the admin reviewed. A reject and a new accept in between make
  // a new one, which this approval must not admit.
  const requestId = isObject(body) ? body["request_id"] : null;
  if (typeof requestId !== "string" || requestId.length === 0 || requestId.length > 64) {
    return error(400, 'expected {"request_id", "envelopes"}: request_id as GET .../pending lists it');
  }
  if (pending.request_id !== requestId) return error(409, "the request changed: fetch the pending list again");
  // Without a key there is nothing to hand over yet.
  if (team.keyed_epoch < 1) return error(409, "the team has no key yet: upload the first envelopes");

  const envelopes = await signedEnvelopes(body, teamId, device);
  if (envelopes instanceof Response) return envelopes;
  const { results } = await db.prepare("SELECT device_id FROM devices WHERE github_id = ?").bind(githubId).all<{ device_id: string }>();
  const theirs = new Set(results.map((d) => d.device_id));
  // What the admin reviewed: their devices and the team's keyed_epoch. An
  // applicant who added a device, or a rotation that ended, since then is a
  // refresh and retry (409), not a bad request. Without these fields an
  // approval is only checked against what is there now.
  const seen = reviewed(body);
  if (typeof seen === "string") return error(400, seen);
  if (
    (seen.keyedEpoch !== null && seen.keyedEpoch !== team.keyed_epoch) ||
    (seen.devices !== null && (seen.devices.size !== theirs.size || [...seen.devices].some((d) => !theirs.has(d))))
  ) {
    return error(409, "the team's key or their devices changed: fetch them again and retry");
  }
  for (const e of envelopes) {
    if (!theirs.has(e.device_id)) return error(400, `device ${e.device_id} is not one of the new member's`);
    if (e.epoch > team.keyed_epoch) return error(400, `epoch ${e.epoch} has no key yet`);
  }
  // Every device, every epoch with a key from the first one given through the
  // newest. An admin may leave out the oldest epochs, never a device. Two
  // removals before a rotation skip an epoch, so the epochs with a key are
  // the ones with envelopes, not every number in between.
  const first = Math.min(...envelopes.map((e) => e.epoch));
  const keyed = await keyedEpochs(db, teamId, first, team.keyed_epoch);
  for (const e of envelopes) {
    if (!keyed.has(e.epoch)) return error(400, `epoch ${e.epoch} has no key`);
  }
  if (envelopes.length !== theirs.size * keyed.size) {
    return error(400, `expected one envelope per device of the new member and epoch with a key, ${first} to ${team.keyed_epoch}`);
  }

  // The checks above hold at commit, or nobody is added: the caller still an
  // owner or admin with a live session and the flag, the same request still
  // pending, the same newest key, the same devices (they are never deleted,
  // so the same count is the same set), and room in the team.
  const t = now();
  const done = await db.batch([
    db
      .prepare(
        `INSERT INTO members (team_id, github_id, role, joined_at)
         SELECT ?1, ?2, 'member', ?3
         WHERE EXISTS (SELECT 1 FROM pending WHERE team_id = ?1 AND github_id = ?2 AND request_id = ?6)
           AND ${stillAdmin("?1", "?7", "?3")}
           AND (SELECT keyed_epoch FROM teams WHERE team_id = ?1) = ?4
           AND (SELECT COUNT(*) FROM devices WHERE github_id = ?2) = ?5
           AND (SELECT COUNT(*) FROM members WHERE team_id = ?1) < ${MAX_MEMBERS}
         ON CONFLICT DO NOTHING`,
      )
      .bind(teamId, githubId, t, team.keyed_epoch, theirs.size, requestId, c.me.hash),
    // Before the pending row goes: it still being there, with the member
    // added (insertEnvelope checks that), means this batch added them. An
    // approval that lost to another one, or whose request changed, writes
    // nothing.
    ...envelopes.map((e) =>
      db
        .prepare(
          insertEnvelope("AND EXISTS (SELECT 1 FROM pending WHERE team_id = ?1 AND github_id = ?6 AND request_id = ?7)"),
        )
        .bind(e.team_id, e.device_id, e.epoch, JSON.stringify(e), t, githubId, requestId),
    ),
    db
      .prepare(
        "DELETE FROM pending WHERE team_id = ?1 AND github_id = ?2 AND request_id = ?3 AND EXISTS (SELECT 1 FROM members WHERE team_id = ?1 AND github_id = ?2)",
      )
      .bind(teamId, githubId, requestId),
  ]);
  if (done[0]!.meta.changes === 1) return Response.json({ github_id: githubId, role: "member" });
  const [allowed, still, count] = await db.batch([
    db.prepare(`SELECT 1 WHERE ${stillAdmin("?1", "?2", "?3")}`).bind(teamId, c.me.hash, now()),
    db.prepare("SELECT request_id FROM pending WHERE team_id = ? AND github_id = ?").bind(teamId, githubId),
    db.prepare("SELECT COUNT(*) AS n FROM members WHERE team_id = ?").bind(teamId),
  ]);
  if (allowed!.results.length === 0) return notAdmin();
  if (still!.results.length === 0) return error(404, "no such request");
  if ((still!.results[0] as { request_id: string }).request_id !== requestId) return error(409, "the request changed: fetch the pending list again");
  if ((count!.results[0] as { n: number }).n >= MAX_MEMBERS) return error(409, `at most ${MAX_MEMBERS} members per team`);
  return error(409, "the team's key or their devices changed: fetch them again and retry");
}

/// The optional `devices` and `keyed_epoch` of an approval: the applicant's
/// device IDs and the team's keyed_epoch as the admin saw them.
function reviewed(body: unknown): { devices: Set<string> | null; keyedEpoch: number | null } | string {
  const devices = isObject(body) ? body["devices"] : undefined;
  const keyedEpoch = isObject(body) ? body["keyed_epoch"] : undefined;
  if (devices !== undefined && (!Array.isArray(devices) || devices.length > MAX_ENVELOPES || !devices.every((d) => typeof d === "string")))
    return "devices is a list of the device IDs you reviewed";
  if (keyedEpoch !== undefined && !Number.isSafeInteger(keyedEpoch)) return "keyed_epoch is the team's keyed_epoch you reviewed";
  return {
    devices: devices === undefined ? null : new Set(devices as string[]),
    keyedEpoch: keyedEpoch === undefined ? null : (keyedEpoch as number),
  };
}

async function reject(c: Ctx, teamId: string, id: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  if (!GITHUB_ID.test(id)) return error(404, "no such request");
  const body = await jsonBody(c.request, MAX_BODY);
  // As in approve: a reject and a new accept in between make a new request,
  // which a late reject of the old one must leave alone.
  const requestId = isObject(body) ? body["request_id"] : null;
  if (typeof requestId !== "string" || requestId.length === 0 || requestId.length > 64) {
    return error(400, 'expected {"request_id"} as GET .../pending lists it');
  }
  const db = c.env.DB;
  const [result, allowed, still] = await db.batch([
    db.prepare(`DELETE FROM pending WHERE team_id = ?1 AND github_id = ?2 AND request_id = ?3 AND ${stillAdmin("?1", "?4", "unixepoch()")}`)
      .bind(teamId, Number(id), requestId, c.me.hash),
    db.prepare(`SELECT 1 WHERE ${stillAdmin("?1", "?2", "unixepoch()")}`).bind(teamId, c.me.hash),
    db.prepare("SELECT 1 FROM pending WHERE team_id = ? AND github_id = ?").bind(teamId, Number(id)),
  ]);
  if (result!.meta.changes) return new Response(null, { status: 204 });
  if (allowed!.results.length === 0) return notAdmin();
  if (still!.results.length > 0) return error(409, "the request changed: fetch the pending list again");
  return error(404, "no such request");
}

// ---- envelopes -----------------------------------------------------------------

/// The body's envelopes: in shape, for this team, from the calling device and
/// signed by it, at most one per device and epoch.
async function signedEnvelopes(body: unknown, teamId: string, from: Device): Promise<Envelope[] | Response> {
  const list = isObject(body) ? body["envelopes"] : null;
  if (!Array.isArray(list) || list.length === 0 || list.length > MAX_ENVELOPES) {
    return error(400, `expected {"envelopes": [...]} with 1 to ${MAX_ENVELOPES} envelopes`);
  }
  const key = await sigKey(from.sig_pub);
  if (!key) return error(400, "the calling device has no usable key");
  const seen = new Set<string>();
  const out: Envelope[] = [];
  for (const raw of list) {
    const e = parseEnvelope(raw, teamId);
    if (typeof e === "string") return error(400, e);
    if (e.from !== from.device_id) return error(400, "envelopes come from the calling device");
    // On the curve, or the recipient could not use it; the shape check above
    // only looks at its length and first byte.
    if (!(await isEncKey(e.eph_pub))) return error(400, "an envelope's eph_pub is not a P-256 public key");
    const slot = `${e.device_id}|${e.epoch}`;
    if (seen.has(slot)) return error(400, `two envelopes for device ${e.device_id} and epoch ${e.epoch}`);
    seen.add(slot);
    out.push(e);
  }
  for (const e of out) {
    if (!(await verify(key, e.sig, envelopeSigned(e)))) return error(400, `bad signature on the envelope for ${e.device_id}, epoch ${e.epoch}`);
  }
  return out;
}

/// Stores an envelope only while its device belongs to a member, so a removal
/// that lands first is not undone. It replaces one only from the same sending
/// device, or one whose sender is no longer a device of an admin (which
/// recipients would refuse). So a device that made a key too late cannot
/// overwrite the key that is in use. A replaced envelope keeps its first
/// time, so a rewrap does not move the epoch's `made_at`.
function storeEnvelope(db: D1Database, e: Envelope, t: number, roster: number, session: string): D1PreparedStatement {
  const guard = `AND (SELECT epoch FROM teams WHERE team_id = ?1) = ?6 AND ${stillAdmin("?1", "?7", "unixepoch()")}`;
  return db.prepare(insertEnvelope(guard)).bind(e.team_id, e.device_id, e.epoch, JSON.stringify(e), t, roster, session);
}

function insertEnvelope(extra: string): string {
  return `INSERT INTO envelopes (team_id, device_id, epoch, body, created_at)
    SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (
      SELECT 1 FROM devices d JOIN members m ON m.github_id = d.github_id AND m.team_id = ?1 WHERE d.device_id = ?2) ${extra}
    ON CONFLICT DO UPDATE SET body = excluded.body
    WHERE json_extract(envelopes.body, '$.from') = json_extract(excluded.body, '$.from') OR NOT EXISTS (
      SELECT 1 FROM devices d JOIN members m ON m.github_id = d.github_id AND m.team_id = envelopes.team_id
      WHERE d.device_id = json_extract(envelopes.body, '$.from') AND m.role IN ('owner', 'admin'))`;
}

/// Whether this upload made the new key: the update in the same batch set
/// `key_upload` to its random ID (?6). Only the upload whose update won passes:
/// not another device's, nor a second upload of the same device, even with the
/// same envelopes.
const KEY_WON = "AND (SELECT key_upload FROM teams WHERE team_id = ?1) = ?6";

/// An envelope of an upload that makes a new key, written only after the same
/// batch made the new epoch the keyed one. So a key never lands for an epoch a
/// removal skipped, a second rotation cannot replace the key that won, and an
/// upload answered 409 writes nothing, its envelopes of older epochs included.
function storeWithNewKey(db: D1Database, e: Envelope, upload: string, t: number): D1PreparedStatement {
  return db.prepare(insertEnvelope(KEY_WON)).bind(e.team_id, e.device_id, e.epoch, JSON.stringify(e), t, upload);
}

/// The epochs from `lo` to `hi` that have a key: the ones with envelopes. A
/// skipped epoch never gets any.
async function keyedEpochs(db: D1Database, teamId: string, lo: number, hi: number): Promise<Set<number>> {
  const { results } = await db
    .prepare("SELECT DISTINCT epoch FROM envelopes WHERE team_id = ? AND epoch BETWEEN ? AND ?")
    .bind(teamId, lo, hi)
    .all<{ epoch: number }>();
  return new Set(results.map((r) => r.epoch));
}

async function putEnvelopes(c: Ctx, teamId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  const device = await callerDevice(c);
  if (device instanceof Response) return device;
  const body = await jsonBody(c.request, MAX_BODY);
  const envelopes = await signedEnvelopes(body, teamId, device);
  if (envelopes instanceof Response) return envelopes;

  const db = c.env.DB;
  const { results } = await db
    .prepare("SELECT d.device_id FROM devices d JOIN members m ON m.github_id = d.github_id WHERE m.team_id = ?")
    .bind(teamId)
    .all<{ device_id: string }>();
  const memberDevices = new Set(results.map((d) => d.device_id));
  const newKey = team.epoch > team.keyed_epoch;
  // The keyed_epoch the client saw when it made these envelopes. A rotation
  // upload is one made while the epoch was not keyed; arriving after it was,
  // it is a loser, not a rewrap, and must not replace the envelopes of the
  // key that won. Rewraps and grants to new devices name the keyed_epoch
  // they saw, which is the current one, and go through. Without the field
  // the old behavior stays, which is why clients send it.
  const expectedKeyed = isObject(body) && typeof body["keyed_epoch"] === "number" ? body["keyed_epoch"] : null;
  const roster = isObject(body) ? body["roster_epoch"] : undefined;
  if (roster !== undefined && !Number.isSafeInteger(roster)) return error(400, "roster_epoch must be an integer");
  if (roster !== undefined && roster !== team.epoch) return error(409, "membership changed: fetch the team again");
  if (expectedKeyed !== null && expectedKeyed > team.keyed_epoch) return error(400, `keyed_epoch ${expectedKeyed} is ahead of the team's`);
  if (expectedKeyed !== null && expectedKeyed < team.keyed_epoch && envelopes.some((e) => e.epoch === team.epoch))
    return error(409, `another upload made the key for epoch ${team.epoch}: fetch it`);
  const keyed = await keyedEpochs(db, teamId, 1, team.keyed_epoch);
  const covered = new Set<string>();
  for (const e of envelopes) {
    if (!memberDevices.has(e.device_id)) return error(400, `device ${e.device_id} is not a member's device`);
    if (newKey && e.epoch === team.epoch) covered.add(e.device_id);
    else if (!keyed.has(e.epoch)) return error(400, `epoch ${e.epoch} is neither keyed nor the current one`);
  }
  // A new key is all or nothing: once it exists the old epoch is refused, so a
  // device left out would be locked out of the team.
  const keying = covered.size > 0;
  if (keying && covered.size !== memberDevices.size) {
    return error(400, `the first envelopes of epoch ${team.epoch} must cover every device of every member`);
  }

  const t = now();
  const upload = crypto.randomUUID();
  const done = await db.batch([
    // First, so the new key's envelopes below can see whether it won. Only if
    // no removal bumped the epoch since we read it, no other device keyed it
    // first, and every member device is still covered: an approval or a new
    // device in between would otherwise be left without the key.
    ...(keying
      ? [
          db
            .prepare(
              `UPDATE teams SET keyed_epoch = ?1, key_upload = ?4 WHERE team_id = ?2 AND epoch = ?1 AND keyed_epoch < ?1 AND ${stillAdmin("?2", "?5", "unixepoch()")} AND NOT EXISTS (
                 SELECT 1 FROM devices d JOIN members m ON m.github_id = d.github_id AND m.team_id = ?2
                 WHERE d.device_id NOT IN (SELECT value FROM json_each(?3)))`,
            )
            .bind(team.epoch, teamId, JSON.stringify([...covered]), upload, c.me.hash),
        ]
      : []),
    ...envelopes.map((e) => (keying ? storeWithNewKey(db, e, upload, t) : storeEnvelope(db, e, t, team.epoch, c.me.hash))),
    db.prepare(`SELECT epoch, keyed_epoch, ${stillAdmin("?1", "?2", "unixepoch()")} AS admin FROM teams WHERE team_id = ?1`).bind(teamId, c.me.hash),
  ]);
  const after = done[done.length - 1]!.results[0] as { epoch: number; keyed_epoch: number; admin: number } | undefined;
  if (!after || !after.admin) return notAdmin();
  if (!keying && after.epoch !== team.epoch) return error(409, "membership changed: fetch the team again");
  if (keying && done[0]!.meta.changes !== 1) {
    if (after.epoch !== team.epoch) return error(409, `epoch ${team.epoch} was skipped: make a key for epoch ${after.epoch}`);
    if (after.keyed_epoch >= team.epoch) return error(409, `another upload made the key for epoch ${team.epoch}: fetch it`);
    return error(409, "a member or device changed: fetch the devices again and retry");
  }
  // What was written, which is less than what was sent when another device's
  // envelopes were there first.
  const stored = done.slice(keying ? 1 : 0, -1).reduce((n, r) => n + r.meta.changes, 0);
  return Response.json({ stored, epoch: after.epoch, rotation_pending: after.keyed_epoch < after.epoch });
}

async function getEnvelopes(c: Ctx, teamId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  const device = await callerDevice(c);
  if (device instanceof Response) return device;
  // made_at is when the epoch's key first reached the team, not when this
  // device got it: a device added later still sees how old the key is. It sits
  // next to the signed fields and is not covered by the signature.
  const { results } = await c.env.DB.prepare(
    `SELECT e.body, (SELECT MIN(x.created_at) FROM envelopes x WHERE x.team_id = e.team_id AND x.epoch = e.epoch) AS made_at
     FROM envelopes e WHERE e.team_id = ? AND e.device_id = ? ORDER BY e.epoch`,
  )
    .bind(teamId, device.device_id)
    .all<{ body: string; made_at: number }>();
  return Response.json(results.map((r) => ({ ...JSON.parse(r.body), made_at: r.made_at })));
}

// ---- members -------------------------------------------------------------------

async function removeMember(c: Ctx, teamId: string, id: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!GITHUB_ID.test(id)) return notFound();
  const target = Number(id);
  const self = target === c.me.githubId;
  if (!self && !isAdmin(team.role)) return notAdmin();
  const db = c.env.DB;
  const row = await db.prepare("SELECT role FROM members WHERE team_id = ? AND github_id = ?").bind(teamId, target).first<{ role: Role }>();
  if (!row) return notFound();
  if (row.role === "owner" && !self && team.role !== "owner") return error(403, "only an owner can remove an owner");

  // Every statement holds only while the target is still a member and is not
  // the last owner. The owner count is read in the batch, not before it, so
  // two owners who leave at once cannot both pass and leave the team with
  // none. The member row goes last, so the conditions read the same rows in
  // each statement.
  const removable = `EXISTS (SELECT 1 FROM members WHERE team_id = ?1 AND github_id = ?2 AND (role <> 'owner'
    OR (SELECT COUNT(*) FROM members WHERE team_id = ?1 AND role = 'owner') > 1))`;
  const results = await db.batch([
    // The removed member holds every key so far; the next one must be new.
    // Only once, so two removals of the same member at once bump it once.
    db.prepare(`UPDATE teams SET epoch = epoch + 1 WHERE team_id = ?1 AND ${removable}`).bind(teamId, target),
    db
      .prepare(`DELETE FROM envelopes WHERE team_id = ?1 AND device_id IN (SELECT device_id FROM devices WHERE github_id = ?2) AND ${removable}`)
      .bind(teamId, target),
    db.prepare(`DELETE FROM members WHERE team_id = ?1 AND github_id = ?2 AND ${removable}`).bind(teamId, target),
    db.prepare("SELECT epoch FROM teams WHERE team_id = ?").bind(teamId),
    db.prepare("SELECT channel_id FROM channels WHERE team_id = ?").bind(teamId),
    db.prepare("SELECT role FROM members WHERE team_id = ? AND github_id = ?").bind(teamId, target),
  ]);
  if (results[0]!.meta.changes !== 1) {
    // Still there means the batch found them to be the last owner.
    return results[5]!.results.length > 0 ? error(409, "the last owner cannot leave") : notFound();
  }
  const epoch = (results[3]!.results[0] as { epoch: number }).epoch;
  // Their open sockets go now, not at the next message. A kick that fails is
  // not retried: every message checks each recipient and closes their socket
  // before it delivers, so a missed socket never gets anything new.
  const channels = results[4]!.results as { channel_id: string }[];
  const kicks = await Promise.allSettled(channels.map((ch) => channelStub(c.env, teamId, ch.channel_id).kick(target)));
  for (const k of kicks) if (k.status === "rejected") console.error("kick failed:", k.reason);
  return Response.json({ epoch, rotation_pending: true });
}

// ---- channels ------------------------------------------------------------------

function channelStub(env: Env, teamId: string, channelId: string) {
  return env.CHANNEL.get(env.CHANNEL.idFromName(`${teamId}/${channelId}`));
}

async function createChannel(c: Ctx, teamId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  const body = await jsonBody(c.request, 8192);
  if (!isObject(body)) return error(400, 'expected {"channel_id", "name_ct"}');
  const channelId = body["channel_id"];
  if (typeof channelId !== "string" || !ULID.test(channelId)) return error(400, "channel_id is a ULID");
  const name = parseSealedName(body["name_ct"]);
  if (typeof name === "string") return error(400, name);
  if (!epochAllowed(name.epoch, team.epoch, team.keyed_epoch)) return error(400, `name_ct: ${epochRefused(name.epoch, team.epoch, team.keyed_epoch).error}`);
  const db = c.env.DB;
  // The name's epoch is checked in the insert too, as in renameChannel: a
  // removal or a rotation that ends between the read above and this write
  // would store a name sealed with a retired key.
  const result = await db
    .prepare(
      `INSERT INTO channels (team_id, channel_id, name_ct, created_by, created_at)
       SELECT ?1, ?2, ?3, ?4, ?5 WHERE (SELECT COUNT(*) FROM channels WHERE team_id = ?1) < ${MAX_CHANNELS}
         AND (SELECT keyed_epoch FROM teams WHERE team_id = ?1) = ?6
         AND (SELECT epoch FROM teams WHERE team_id = ?1) = ?6
       ON CONFLICT DO NOTHING`,
    )
    .bind(teamId, channelId, JSON.stringify(name), c.me.githubId, now(), name.epoch)
    .run();
  if (result.meta.changes === 1) return Response.json({ channel_id: channelId }, { status: 201 });
  const exists = await db.prepare("SELECT 1 FROM channels WHERE team_id = ? AND channel_id = ?").bind(teamId, channelId).first();
  if (exists) return error(409, "a channel with this ID exists");
  const still = await db.prepare("SELECT epoch, keyed_epoch FROM teams WHERE team_id = ?").bind(teamId).first<{ epoch: number; keyed_epoch: number }>();
  return error(409, still?.keyed_epoch !== name.epoch || still.epoch !== name.epoch ? "the team's key changed: fetch it and seal the name again" : `at most ${MAX_CHANNELS} channels per team`);
}

/// Seals a channel's name again: after a rotation, so members approved
/// without the older epochs can read it, or to rename it.
async function renameChannel(c: Ctx, teamId: string, channelId: string): Promise<Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  if (!isAdmin(team.role)) return notAdmin();
  if (!ULID.test(channelId)) return notFound();
  const body = await jsonBody(c.request, 8192);
  const name = parseSealedName(isObject(body) ? body["name_ct"] : null);
  if (typeof name === "string") return error(400, name);
  if (!epochAllowed(name.epoch, team.epoch, team.keyed_epoch)) return error(400, `name_ct: ${epochRefused(name.epoch, team.epoch, team.keyed_epoch).error}`);
  const db = c.env.DB;
  // Only while that epoch is still the current one with envelopes, and never
  // back to an older epoch than the stored name's.
  const result = await db
    .prepare(
      `UPDATE channels SET name_ct = ?3 WHERE team_id = ?1 AND channel_id = ?2
         AND json_extract(name_ct, '$.epoch') <= ?4 AND (SELECT keyed_epoch FROM teams WHERE team_id = ?1) = ?4
         AND (SELECT epoch FROM teams WHERE team_id = ?1) = ?4`,
    )
    .bind(teamId, channelId, JSON.stringify(name), name.epoch)
    .run();
  if (result.meta.changes === 1) return Response.json({ channel_id: channelId });
  const exists = await db.prepare("SELECT 1 FROM channels WHERE team_id = ? AND channel_id = ?").bind(teamId, channelId).first();
  return exists ? error(409, "the team's key changed: fetch it and seal the name again") : notFound();
}

/// The checks a channel's socket and history share: member, device, channel.
async function channelAccess(c: Ctx, teamId: string, channelId: string): Promise<{ team: Team; device: Device } | Response> {
  const team = await teamOf(c, teamId);
  if (!team) return notFound();
  const device = await callerDevice(c);
  if (device instanceof Response) return device;
  const channel = ULID.test(channelId)
    ? await c.env.DB.prepare("SELECT 1 FROM channels WHERE team_id = ? AND channel_id = ?").bind(teamId, channelId).first()
    : null;
  if (!channel) return notFound();
  return { team, device };
}

async function openSocket(c: Ctx, teamId: string, channelId: string): Promise<Response> {
  const access = await channelAccess(c, teamId, channelId);
  if (access instanceof Response) return access;
  if (c.request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return error(426, "expected a WebSocket upgrade");
  // A request made here, not the client's: only these headers reach the
  // object, so a client cannot send its own copies of the trusted ones.
  const headers = new Headers({
    Upgrade: "websocket",
    [TRUSTED.githubId]: String(c.me.githubId),
    [TRUSTED.deviceId]: access.device.device_id,
    [TRUSTED.sigPub]: access.device.sig_pub,
    [TRUSTED.teamId]: teamId,
    [TRUSTED.channelId]: channelId,
    // The session's hash, so the object can end the socket with the session.
    [TRUSTED.session]: c.me.hash,
  });
  return channelStub(c.env, teamId, channelId).fetch("https://channel/ws", { headers });
}

async function history(c: Ctx, teamId: string, channelId: string): Promise<Response> {
  const access = await channelAccess(c, teamId, channelId);
  if (access instanceof Response) return access;
  const afterText = c.url.searchParams.get("after") ?? "0";
  const limitText = c.url.searchParams.get("limit") ?? String(HISTORY_LIMIT);
  const after = /^[0-9]{1,15}$/.test(afterText) ? Number(afterText) : NaN;
  const limit = /^[0-9]{1,3}$/.test(limitText) ? Number(limitText) : NaN;
  if (!Number.isSafeInteger(after)) return error(400, "after is a seq, 0 or more");
  if (!(limit >= 1 && limit <= HISTORY_LIMIT)) return error(400, `limit is 1 to ${HISTORY_LIMIT}`);
  const beforeText = c.url.searchParams.get("before");
  const before = beforeText === "latest" ? Number.MAX_SAFE_INTEGER : beforeText === null ? undefined : Number(beforeText);
  if (beforeText !== null && (c.url.searchParams.has("after") || (beforeText !== "latest" && !/^[1-9][0-9]{0,14}$/.test(beforeText))))
    return error(400, "use either after or before; before is a positive seq or latest");
  const rows = await channelStub(c.env, teamId, channelId).history(after, limit, before);
  // Only release the captured page to a device that still has access.
  const grant = await c.env.DB.prepare(
    `SELECT s.expires_at FROM sessions s
     JOIN members m ON m.github_id = s.github_id AND m.team_id = ?2
     JOIN devices d ON d.github_id = s.github_id AND d.device_id = ?3
     JOIN user_flags f ON f.github_id = s.github_id AND f.flag = ?4
     WHERE s.token_hash = ?1`,
  ).bind(c.me.hash, teamId, access.device.device_id, FLAG).first<{ expires_at: number }>();
  if (!grant || grant.expires_at <= now()) return notFound();
  return Response.json(rows);
}

/// The user behind a device that wrote in this channel, with all their
/// devices, so a member can check messages of someone who has left the team:
/// `GET /users/<id>/devices` only lists people who share a team now. Only a
/// device with a message the channel still keeps, so a member cannot look up
/// the devices of anyone who never wrote here.
async function sender(c: Ctx, teamId: string, channelId: string, deviceId: string): Promise<Response> {
  const access = await channelAccess(c, teamId, channelId);
  if (access instanceof Response) return access;
  if (!DEVICE_ID.test(deviceId)) return notFound();
  if (!(await channelStub(c.env, teamId, channelId).sent(deviceId))) return notFound();
  const db = c.env.DB;
  const [user, devices] = await db.batch([
    db.prepare("SELECT u.github_id, u.login FROM devices d JOIN users u ON u.github_id = d.github_id WHERE d.device_id = ?").bind(deviceId),
    db
      .prepare(
        `SELECT device_id, sig_pub, enc_pub, name, created_at, endorsed_by, endorsement_sig FROM devices
         WHERE github_id = (SELECT github_id FROM devices WHERE device_id = ?) ORDER BY created_at, device_id`,
      )
      .bind(deviceId),
  ]);
  const row = user!.results[0] as { github_id: number; login: string } | undefined;
  if (!row) return notFound();
  return Response.json({ github_id: row.github_id, login: row.login, devices: devices!.results });
}
