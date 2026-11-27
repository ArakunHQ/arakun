// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// GitHub access for a cloud machine, through the Arakun Cloud GitHub App (not
// the OAuth App used for sign-in). The owner installs the app on chosen
// repositories or on a whole account.
//
// No token goes into the container. The machine's object has every HTTPS
// connection to github.com handed to `GitHubGit` below (outbound
// interception); Cloudflare ends TLS there with a CA made for that one start,
// which the image trusts (machine/entrypoint.sh). For git's smart HTTP, the
// entrypoint adds an installation token for the one repository in the URL and
// passes the request on. Other github.com requests go on as they came.
//
// A token is for one repository and lasts an hour; it can read, or with a push
// write, the repository's contents and nothing else. The machine's object
// keeps it in memory, never on disk, so a snapshot holds none.
//
// A fetch with no token to give (the app is not on the repository, or the
// owner cannot read it) goes on without one, as it did before: a public
// repository, such as a dependency, still clones. Only when GitHub answers 401,
// a private repository, does git get the refusal, with the install link.
//
// The app being installed is not enough: anyone may install it anywhere, so
// the owner's own GitHub account must be able to read the repository, and to
// write to it for a push. The agent never gets more than its owner has.

import { WorkerEntrypoint } from "cloudflare:workers";
import { b64url, type Env } from "./util";

/// What git asks for at a repository: its owner and name as written in the
/// URL, and whether it pushes.
export interface GitRequest {
  owner: string;
  repo: string;
  push: boolean;
}

/// A token for one repository, or why there is none. `anonymous`: a fetch that
/// goes on without a token, refused only if GitHub wants one.
export type Access = { token: string; expiresAt: number } | { refused: string; anonymous?: boolean };

/// Props the machine's object hands the entrypoint with each intercept.
export interface GitProps {
  label: string;
}

/// The host whose HTTPS traffic the machine's object hands to `GitHubGit`.
export const GITHUB_HOST = "github.com";
/// A token is used again while it has longer than this to live: a clone or a
/// push of a large repository takes minutes.
export const TOKEN_MARGIN_MS = 10 * 60 * 1000;
/// How long a fetch with no token is let go on without asking GitHub again.
/// Short, so a repository the owner installs the app on just now gets a token
/// soon; long enough that one clone asks once.
export const ANONYMOUS_MS = 2 * 60 * 1000;

const API = "https://api.github.com";
const USER_AGENT = "arakun-cloud";
/// The Accept and version GitHub's REST API asks for.
const API_HEADERS = { Accept: "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28", "User-Agent": USER_AGENT };

/// The parts of a git smart-HTTP request: `/<owner>/<repo>[.git]/info/refs`
/// with a `service`, `git-upload-pack` (fetch) or `git-receive-pack` (push).
/// Null for anything else on github.com.
export function parseGitRequest(url: URL): GitRequest | null {
  const m = /^\/([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))\/([A-Za-z0-9._-]{1,100})\/(info\/refs|git-upload-pack|git-receive-pack)$/.exec(url.pathname);
  if (!m) return null;
  const owner = m[1]!;
  const repo = m[2]!.replace(/\.git$/, "");
  if (repo === "" || repo === "." || repo === "..") return null;
  let service = m[3]!;
  if (service === "info/refs") {
    service = url.searchParams.get("service") ?? "";
    if (service !== "git-upload-pack" && service !== "git-receive-pack") return null;
  }
  return { owner, repo, push: service === "git-receive-pack" };
}

/// Whether this Worker has a GitHub App to give machines access with.
export function appConfigured(env: Env): boolean {
  return !!env.GITHUB_APP_ID && !!env.GITHUB_APP_PRIVATE_KEY && !!env.GITHUB_APP_SLUG;
}

/// Where the owner installs the app, or picks more repositories for it.
export function installUrl(env: Env): string {
  return `https://github.com/apps/${encodeURIComponent(env.GITHUB_APP_SLUG ?? "")}/installations/new`;
}

/// What git shows the owner when a repository is refused: the body of a
/// text/plain error, which git prints as `remote:` lines.
export function notInstalled(env: Env, owner: string, repo: string): string {
  return `Arakun Cloud has no access to ${owner}/${repo}: the Arakun Cloud GitHub App is not installed on it.\nInstall it on this repository: ${installUrl(env)}\n`;
}

/// The DER length of `n` bytes.
function derLength(n: number): number[] {
  if (n < 0x80) return [n];
  const bytes: number[] = [];
  for (let v = n; v > 0; v >>= 8) bytes.unshift(v & 0xff);
  return [0x80 | bytes.length, ...bytes];
}

/// A PKCS#1 RSA key (what GitHub hands out, `BEGIN RSA PRIVATE KEY`) as the
/// PKCS#8 WebCrypto takes: the same key in an envelope that names RSA.
export function pkcs1ToPkcs8(pkcs1: Uint8Array): Uint8Array {
  // INTEGER 0, then SEQUENCE { OID rsaEncryption, NULL }.
  const head = [0x02, 0x01, 0x00, 0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00];
  const octets = [0x04, ...derLength(pkcs1.length)];
  const body = head.length + octets.length + pkcs1.length;
  return Uint8Array.from([0x30, ...derLength(body), ...head, ...octets, ...pkcs1]);
}

/// The app's private key from its PEM, PKCS#1 or PKCS#8.
export async function importAppKey(pem: string): Promise<CryptoKey> {
  const m = /-----BEGIN (RSA )?PRIVATE KEY-----([\s\S]+?)-----END (?:RSA )?PRIVATE KEY-----/.exec(pem);
  if (!m) throw new Error("GITHUB_APP_PRIVATE_KEY is not a PEM private key");
  const der = Uint8Array.from(atob(m[2]!.replace(/\s+/g, "")), (c) => c.charCodeAt(0));
  const pkcs8 = m[1] ? pkcs1ToPkcs8(der) : der;
  return crypto.subtle.importKey("pkcs8", pkcs8, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["sign"]);
}

/// The app's own JWT, good for nine minutes (GitHub takes at most ten), from
/// a minute back in case GitHub's clock is behind ours.
export async function appJwt(env: Env, nowSec = Math.floor(Date.now() / 1000)): Promise<string> {
  const key = await importAppKey(env.GITHUB_APP_PRIVATE_KEY ?? "");
  const enc = new TextEncoder();
  const part = (v: unknown) => b64url(enc.encode(JSON.stringify(v)));
  const unsigned = `${part({ alg: "RS256", typ: "JWT" })}.${part({ iat: nowSec - 60, exp: nowSec + 9 * 60, iss: env.GITHUB_APP_ID })}`;
  const sig = new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, enc.encode(unsigned)));
  return `${unsigned}.${b64url(sig)}`;
}

/// The owner's GitHub user ID, or null when the account has no GitHub
/// identity (one that signs in by email only).
export async function githubUserOf(env: Env, accountId: number): Promise<number | null> {
  const row = await env.DB.prepare("SELECT subject FROM identities WHERE account_id = ? AND provider = 'github'").bind(accountId).first<{ subject: string }>();
  const id = row ? Number(row.subject) : NaN;
  return Number.isSafeInteger(id) && id > 0 ? id : null;
}

function bearer(token: string): Record<string, string> {
  return { ...API_HEADERS, Authorization: `Bearer ${token}` };
}

/// The permission levels that may push, as GitHub's `permission` field names them.
const WRITES = new Set(["admin", "maintain", "write"]);

/// A token for one repository on behalf of the owner's GitHub user, or the
/// reason there is none. Asks GitHub four times: where the app is installed,
/// for the token, the user's login, and what that user may do there.
export async function mintToken(env: Env, githubUser: number, req: GitRequest): Promise<Access> {
  const where = `${encodeURIComponent(req.owner)}/${encodeURIComponent(req.repo)}`;
  const jwt = await appJwt(env);
  const inst = await fetch(`${API}/repos/${where}/installation`, { headers: bearer(jwt) });
  if (inst.status === 404) return { refused: notInstalled(env, req.owner, req.repo), anonymous: !req.push };
  if (!inst.ok) throw new Error(`GitHub answered ${inst.status} for the installation of ${req.owner}/${req.repo}`);
  const { id } = await inst.json<{ id: number }>();
  const made = await fetch(`${API}/app/installations/${id}/access_tokens`, {
    method: "POST",
    headers: { ...bearer(jwt), "Content-Type": "application/json" },
    body: JSON.stringify({ repositories: [req.repo], permissions: { contents: req.push ? "write" : "read", metadata: "read" } }),
  });
  // 422: the installation covers some of the account's repositories, not this one.
  if (made.status === 422 || made.status === 404) return { refused: notInstalled(env, req.owner, req.repo), anonymous: !req.push };
  if (!made.ok) throw new Error(`GitHub answered ${made.status} to a token for ${req.owner}/${req.repo}`);
  const { token, expires_at } = await made.json<{ token: string; expires_at: string }>();
  // The login may have changed since sign-in; the ID does not.
  const user = await fetch(`${API}/user/${githubUser}`, { headers: bearer(token) });
  if (!user.ok) throw new Error(`GitHub answered ${user.status} for user ${githubUser}`);
  const { login } = await user.json<{ login: string }>();
  const perm = await fetch(`${API}/repos/${where}/collaborators/${encodeURIComponent(login)}/permission`, { headers: bearer(token) });
  const level = perm.ok ? (await perm.json<{ permission?: string }>()).permission ?? "none" : "none";
  if (req.push ? !WRITES.has(level) : level === "none") {
    await revoke(token);
    return {
      refused: `Arakun Cloud will not ${req.push ? "push to" : "read"} ${req.owner}/${req.repo}: your GitHub account ${login} ${req.push ? "cannot write to it" : "has no access to it"}.\n`,
      anonymous: !req.push,
    };
  }
  return { token, expiresAt: Date.parse(expires_at) };
}

/// Ends a token we made and will not use.
async function revoke(token: string): Promise<void> {
  try {
    await fetch(`${API}/installation/token`, { method: "DELETE", headers: bearer(token) });
  } catch (e) {
    console.error("could not revoke an unused installation token:", e);
  }
}

/// The tokens one machine holds, in memory only, by repository and access,
/// and the fetches that go on without one for a short while.
export class TokenCache {
  private readonly tokens = new Map<string, { access: Access; until: number }>();

  private static key(req: GitRequest): string {
    // GitHub's names are not case-sensitive.
    return `${req.owner}/${req.repo}:${req.push ? "w" : "r"}`.toLowerCase();
  }

  get(req: GitRequest, nowMs = Date.now()): Access | null {
    const k = TokenCache.key(req);
    const t = this.tokens.get(k);
    if (t && t.until > nowMs) return t.access;
    this.tokens.delete(k);
    return null;
  }

  put(req: GitRequest, token: string, expiresAt: number): void {
    this.tokens.set(TokenCache.key(req), { access: { token, expiresAt }, until: expiresAt - TOKEN_MARGIN_MS });
  }

  /// A fetch that goes on without a token, and what git hears if GitHub wants one.
  putAnonymous(req: GitRequest, refused: string, nowMs = Date.now()): void {
    this.tokens.set(TokenCache.key(req), { access: { refused, anonymous: true }, until: nowMs + ANONYMOUS_MS });
  }

  clear(): void {
    this.tokens.clear();
  }
}

/// `user:password` for git over HTTPS, as GitHub takes an installation token.
export function basicAuth(token: string): string {
  return `Basic ${btoa(`x-access-token:${token}`)}`;
}

function refusal(status: number, text: string): Response {
  return new Response(text, { status, headers: { "Content-Type": "text/plain; charset=utf-8" } });
}

/// Gets every HTTPS request a cloud machine makes to github.com. The machine's
/// object makes one per container start with the machine's label as props.
export class GitHubGit extends WorkerEntrypoint<Env, GitProps> {
  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    // The intercept matched the TLS name; the URL comes from the Host header,
    // which the container writes. Credentials go to github.com only.
    if (url.protocol !== "https:" || url.hostname !== GITHUB_HOST || (url.port !== "" && url.port !== "443")) {
      return refusal(421, `Arakun Cloud: this connection is for ${GITHUB_HOST} only.\n`);
    }
    const git = parseGitRequest(url);
    if (!git) return fetch(request, { redirect: "manual" });
    const ns = this.env.CLOUD_MACHINE;
    if (!ns) return refusal(503, "Arakun Cloud: cloud machines are not set up here.\n");
    let access: Access;
    try {
      access = await ns.getByName(this.ctx.props.label).gitAccess(git);
    } catch (e) {
      console.error(`cloud machine ${this.ctx.props.label}: no GitHub token for ${git.owner}/${git.repo}:`, e);
      return refusal(502, `Arakun Cloud could not get access to ${git.owner}/${git.repo} from GitHub. Try again in a minute.\n`);
    }
    const headers = new Headers(request.headers);
    if ("refused" in access) {
      // 403, not 401: on 401 git asks for a password no one is there to type.
      if (!access.anonymous) return refusal(403, access.refused);
      headers.delete("Authorization");
      const res = await fetch(new Request(request, { headers, redirect: "manual" }));
      return res.status === 401 ? refusal(403, access.refused) : res;
    }
    headers.set("Authorization", basicAuth(access.token));
    // A redirect (a renamed repository) goes back to git without the token.
    return fetch(new Request(request, { headers, redirect: "manual" }));
  }
}
