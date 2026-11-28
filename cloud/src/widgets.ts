// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The community widget index the Marketplace lists, and the reports people
// send from it. The format and the rules are in
// docs/superpowers/specs/2026-11-27-community-widgets-design.md.
//
//   GET  /widgets             → { widgets: [listing with its versions, newest first] }
//   POST /widgets/report      { id, version, reason } → 202
//   POST /widgets/installed   { id } → 204, counts an install
//
// The index is public and needs no sign-in, like the Marketplace. A person at
// Arakun lists each version, signs the ones they reviewed and decides each
// report, through the admin routes at the bottom (see admin.ts). The app
// checks the signature itself: the Worker never says "verified".
//
// A report and an install count need no sign-in either, so each client IP
// gets a few a minute, on the feedback limit with keys of their own.

import { record, type Operator } from "./admin_log";
import { error, isObject, jsonBody, now, type Env } from "./util";

/// `<author>/<name>`, as `manifest::check_id` in the app takes it.
export const WIDGET_ID = /^[a-z0-9][a-z0-9-]{0,38}\/[a-z0-9][a-z0-9-]{0,63}$/;
/// Semver without build metadata, as `manifest::check_version` takes it.
export const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$/;
const COMMIT = /^[0-9a-f]{40}$/;
const HASH = /^[0-9a-f]{64}$/;
const REPOSITORY = /^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
/// What the Report menu in the app offers.
export const REASONS = ["misleading", "harmful", "broken", "copied", "other"] as const;
/// The files the index carries for the needs and the preview.
export const FILES = ["widget.toml", "main.rhai", "sample.json"] as const;
/// The app refuses widget files over 512 KB in all; these three are less.
const MAX_FILES = 512 * 1024;
const MAX_TEXT = 500;
const MAX_SIGNATURE = 1024;
const MAX_BODY = 4096;
const MAX_LISTING = MAX_FILES * 2 + 16 * 1024;

interface ListingRow {
  id: string;
  name: string;
  about: string;
  license: string;
  repository: string;
  installs: number;
  pulled_reason: string | null;
}

interface VersionRow {
  widget_id: string;
  version: string;
  commit_sha: string;
  published_at: number;
  hash: string;
  files: string;
  signature: string | null;
  pulled_reason: string | null;
}

/// The answer for a public /widgets route, or null when the path is not one.
export async function widgets(request: Request, env: Env, url: URL): Promise<Response | null> {
  const route = `${request.method} ${url.pathname}`;
  if (route === "GET /widgets") return index(env);
  if (route === "POST /widgets/report") return report(request, env);
  if (route === "POST /widgets/installed") return installed(request, env);
  if (url.pathname === "/widgets" || url.pathname.startsWith("/widgets/")) return error(404, "not found");
  return null;
}

/// Every listing with its versions, newest first. Only the newest version
/// that is not pulled carries its files: the Marketplace shows that one.
/// A pulled version stays in, with its reason, so the app stops it.
async function index(env: Env): Promise<Response> {
  const db = env.DB;
  const [listings, versions] = await db.batch([
    db.prepare("SELECT id, name, about, license, repository, installs, pulled_reason FROM widgets ORDER BY installs DESC, id"),
    db.prepare("SELECT * FROM widget_versions ORDER BY widget_id, published_at DESC, version DESC"),
  ]);
  const byWidget = new Map<string, VersionRow[]>();
  for (const v of versions!.results as unknown as VersionRow[]) {
    const list = byWidget.get(v.widget_id) ?? [];
    list.push(v);
    byWidget.set(v.widget_id, list);
  }
  const out = (listings!.results as unknown as ListingRow[]).flatMap((w) => {
    const vs = byWidget.get(w.id) ?? [];
    if (vs.length === 0) return [];
    const current = w.pulled_reason ? undefined : vs.find((v) => !v.pulled_reason);
    return [
      {
        id: w.id,
        name: w.name,
        author: w.id.split("/")[0],
        about: w.about,
        license: w.license,
        repository: w.repository,
        installs: w.installs,
        pulled: w.pulled_reason,
        versions: vs.map((v) => ({
          version: v.version,
          commit: v.commit_sha,
          date: v.published_at,
          hash: v.hash,
          signature: v.signature,
          pulled: v.pulled_reason,
        })),
        files: current ? { version: current.version, ...(JSON.parse(current.files) as Record<string, string>) } : null,
      },
    ];
  });
  return Response.json({ widgets: out });
}

/// One client IP's turn on the feedback limit, under a key of its own.
async function limited(request: Request, env: Env, what: string): Promise<Response | null> {
  const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
  try {
    if ((await env.FEEDBACK_LIMIT.limit({ key: `${what}:${ip}` })).success) return null;
  } catch (e) {
    console.error(`${what} limit failed:`, e);
    return error(503, "the server could not check the request: try again");
  }
  return error(429, "too many requests, try again in a minute");
}

async function report(request: Request, env: Env): Promise<Response> {
  const stop = await limited(request, env, "widget-report");
  if (stop) return stop;
  const body = await jsonBody(request, MAX_BODY);
  if (!isObject(body)) return error(400, "expected a JSON object");
  const { id, version, reason } = body;
  if (typeof id !== "string" || !WIDGET_ID.test(id)) return error(400, "id must be <author>/<name>");
  if (typeof version !== "string" || !VERSION.test(version)) return error(400, "version must be a version");
  if (typeof reason !== "string" || !(REASONS as readonly string[]).includes(reason)) {
    return error(400, `reason must be one of ${REASONS.join(", ")}`);
  }
  const known = await env.DB.prepare("SELECT 1 FROM widget_versions WHERE widget_id = ? AND version = ?").bind(id, version).first();
  if (!known) return error(404, "no such widget version");
  await env.DB.prepare("INSERT INTO widget_reports (widget_id, version, reason, created_at) VALUES (?, ?, ?, ?)")
    .bind(id, version, reason, now())
    .run();
  return Response.json({ ok: true }, { status: 202 });
}

async function installed(request: Request, env: Env): Promise<Response> {
  const stop = await limited(request, env, "widget-install");
  if (stop) return stop;
  const body = await jsonBody(request, MAX_BODY);
  if (!isObject(body) || typeof body["id"] !== "string" || !WIDGET_ID.test(body["id"])) {
    return error(400, "expected { id: <author>/<name> }");
  }
  const result = await env.DB.prepare("UPDATE widgets SET installs = installs + 1 WHERE id = ?").bind(body["id"]).run();
  if (result.meta.changes === 0) return error(404, "no such widget");
  return new Response(null, { status: 204 });
}

// The operator's side, called from admin.ts:
//
//   PUT    /admin/widgets/<author>/<name>/<version>       → lists a version, { name, about, license, repository, commit, hash, files, signature? }
//   PUT    /admin/widgets/<author>/<name>[/<version>]/pull → pulls it, { reason }
//   DELETE /admin/widgets/<author>/<name>[/<version>]/pull → lists it again
//   GET    /admin/widgets/reports                         → reports, open ones first
//   PUT    /admin/widgets/reports/<id>/close              → marks one decided

const VERSION_PATH = /^\/admin\/widgets\/([^/]+\/[^/]+)\/([^/]+)$/;
const PULL_PATH = /^\/admin\/widgets\/([^/]+\/[^/]+?)(?:\/([^/]+))?\/pull$/;
const CLOSE_PATH = /^\/admin\/widgets\/reports\/([1-9][0-9]{0,15})\/close$/;

/// The answer for an /admin/widgets route, or null when the path is not one.
/// The caller has checked the admin secret.
export async function adminWidgets(request: Request, env: Env, url: URL, op: Operator): Promise<Response | null> {
  const path = url.pathname;
  if (!path.startsWith("/admin/widgets/")) return null;
  if (path === "/admin/widgets/reports" && request.method === "GET") return listReports(env);
  const close = CLOSE_PATH.exec(path);
  if (close && request.method === "PUT") return closeReport(env, op, Number(close[1]));
  const pull = PULL_PATH.exec(path);
  if (pull && (request.method === "PUT" || request.method === "DELETE")) {
    return setPulled(request, env, op, pull[1]!, pull[2] ?? null);
  }
  const version = VERSION_PATH.exec(path);
  if (version && request.method === "PUT") return listVersion(request, env, op, version[1]!, version[2]!);
  return error(404, "not found");
}

function text(v: unknown, max: number): string | null {
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t.length > 0 && t.length <= max ? t : null;
}

/// Adds a version, or replaces one, as when it is signed after review. The
/// listing takes the newest name, about, license and repository.
async function listVersion(request: Request, env: Env, op: Operator, id: string, version: string): Promise<Response> {
  if (!WIDGET_ID.test(id)) return error(400, "the id must be <author>/<name>");
  if (!VERSION.test(version)) return error(400, "the version must be semver, such as 0.4.1");
  const body = await jsonBody(request, MAX_LISTING);
  if (!isObject(body)) return error(400, "expected a JSON object");
  const name = text(body["name"], 100);
  const about = text(body["about"], MAX_TEXT);
  const license = text(body["license"], 100);
  if (!name || !about || !license) return error(400, "name, about and license must be short text");
  const repository = body["repository"];
  if (typeof repository !== "string" || !REPOSITORY.test(repository)) return error(400, "repository must be https://github.com/<owner>/<name>");
  const commit = body["commit"];
  if (typeof commit !== "string" || !COMMIT.test(commit)) return error(400, "commit must be a full commit");
  const hash = body["hash"];
  if (typeof hash !== "string" || !HASH.test(hash)) return error(400, "hash must be the SHA-256 of the files, in lowercase hex");
  const files = body["files"];
  if (!isObject(files)) return error(400, `files must have ${FILES.join(", ")}`);
  let size = 0;
  const kept: Record<string, string> = {};
  for (const f of FILES) {
    const t = files[f];
    if (typeof t !== "string") return error(400, `files must have ${FILES.join(", ")}`);
    size += t.length;
    kept[f] = t;
  }
  if (size > MAX_FILES) return error(400, "the files take more than 512 KB");
  const signature = body["signature"] ?? null;
  if (signature !== null && (typeof signature !== "string" || signature.length > MAX_SIGNATURE)) {
    return error(400, "signature must be a minisign signature");
  }
  const db = env.DB;
  const t = now();
  await db.batch([
    db
      .prepare(
        `INSERT INTO widgets (id, name, about, license, repository, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET name = ?2, about = ?3, license = ?4, repository = ?5, updated_at = ?6`,
      )
      .bind(id, name, about, license, repository, t),
    db
      .prepare(
        `INSERT INTO widget_versions (widget_id, version, commit_sha, published_at, hash, files, signature) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (widget_id, version) DO UPDATE SET commit_sha = ?3, hash = ?5, files = ?6, signature = ?7`,
      )
      .bind(id, version, commit, t, hash, JSON.stringify(kept), signature),
  ]);
  await record(env, op, "widget.list", null, { id, version, signed: signature === null ? 0 : 1 });
  return Response.json({ id, version, verified: signature !== null });
}

async function setPulled(request: Request, env: Env, op: Operator, id: string, version: string | null): Promise<Response> {
  if (!WIDGET_ID.test(id)) return error(400, "the id must be <author>/<name>");
  let reason: string | null = null;
  if (request.method === "PUT") {
    const body = await jsonBody(request, MAX_BODY);
    reason = isObject(body) ? text(body["reason"], MAX_TEXT) : null;
    if (!reason) return error(400, `expected { reason } of at most ${MAX_TEXT} characters`);
  }
  const result = version
    ? await env.DB.prepare("UPDATE widget_versions SET pulled_reason = ? WHERE widget_id = ? AND version = ?").bind(reason, id, version).run()
    : await env.DB.prepare("UPDATE widgets SET pulled_reason = ? WHERE id = ?").bind(reason, id).run();
  if (result.meta.changes === 0) return error(404, "no such widget");
  await record(env, op, reason ? "widget.pull" : "widget.unpull", null, { id, version, reason });
  return new Response(null, { status: 204 });
}

async function listReports(env: Env): Promise<Response> {
  const { results } = await env.DB.prepare(
    "SELECT id, widget_id, version, reason, created_at, closed_at, closed_by FROM widget_reports ORDER BY closed_at IS NOT NULL, created_at DESC LIMIT 200",
  ).all();
  return Response.json({ reports: results });
}

async function closeReport(env: Env, op: Operator, id: number): Promise<Response> {
  const result = await env.DB.prepare("UPDATE widget_reports SET closed_at = ?, closed_by = ? WHERE id = ? AND closed_at IS NULL")
    .bind(now(), op.login, id)
    .run();
  if (result.meta.changes === 0) return error(404, "no such open report");
  await record(env, op, "widget.report.close", null, { report: id });
  return new Response(null, { status: 204 });
}
