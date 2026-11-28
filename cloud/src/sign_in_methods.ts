// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The ways an account signs in, which a signed-in person adds and removes.
//
//   GET    /me/sign-in                 → { methods }
//   POST   /me/sign-in/github          { access_token }  → { methods }
//   POST   /me/sign-in/email           { email }         → { sent: true, expires_in }
//   POST   /me/sign-in/email/verify    { email, code }   → { methods }
//   DELETE /me/sign-in/<github|email>                    → { methods }
//
// `methods` is a list of { provider, name, linked_at }: the GitHub login, or
// the address. An account has at most one of each.
//
// Accounts are never joined. A GitHub account or an address that already
// signs in to another account is refused, only after the caller proved it is
// theirs, so the answer tells nobody else that it has an account. The last
// method stays: without it nobody could sign in to the account again.
//
// Each change is a row in `sign_in_changes` and a mail to every address the
// account had before or has after it, so whoever owns the account hears of a
// change made with a stolen session.

import { MAX_ACCESS_TOKEN, type Session, session, signInAllowed, verifyAndRevoke } from "./auth";
import { askedAddress, CODE, mailCode, mailWithCode, normalAddress, readBody, useCode, WRONG_CODE } from "./email_auth";
import { error, now, type Env } from "./util";

type Provider = "github" | "email";
const PROVIDERS: readonly Provider[] = ["github", "email"];

export interface Method {
  provider: Provider;
  /// The GitHub login, or the address.
  name: string;
  linked_at: number;
}

export async function signInMethods(request: Request, env: Env, url: URL): Promise<Response | null> {
  const { pathname } = url;
  if (pathname !== "/me/sign-in" && !pathname.startsWith("/me/sign-in/")) return null;
  const route = `${request.method} ${pathname}`;
  const unlink = request.method === "DELETE" ? /^\/me\/sign-in\/(github|email)$/.exec(pathname)?.[1] : undefined;
  const known = ["GET /me/sign-in", "POST /me/sign-in/github", "POST /me/sign-in/email", "POST /me/sign-in/email/verify"].includes(route);
  if (!known && !unlink) return error(404, "not found");

  const s = await session(request, env, { renew: request.method === "GET" });
  if (!s) return error(401, "not signed in");
  if (request.method === "GET") return Response.json({ methods: await methods(env, s.accountId) });
  // A blocked account keeps what it has: it could otherwise hand itself to
  // a new sign-in the block did not reach.
  if (s.blocked) return error(403, "the account is blocked");

  switch (route) {
    case "POST /me/sign-in/github":
      return linkGitHub(request, env, s);
    case "POST /me/sign-in/email":
      return askLinkCode(request, env, s);
    case "POST /me/sign-in/email/verify":
      return linkEmail(request, env, s);
  }
  return remove(env, s, unlink as Provider);
}

/// The account's sign-in methods, GitHub first.
export async function methods(env: Env, accountId: number): Promise<Method[]> {
  const rows = await env.DB.prepare(
    `SELECT i.provider, CASE WHEN i.provider = 'github' THEN a.login ELSE i.subject END AS name, i.linked_at
     FROM identities i JOIN accounts a ON a.id = i.account_id WHERE i.account_id = ?`,
  )
    .bind(accountId)
    .all<Method>();
  return rows.results.sort((a, b) => PROVIDERS.indexOf(a.provider) - PROVIDERS.indexOf(b.provider));
}

async function linkGitHub(request: Request, env: Env, s: Session): Promise<Response> {
  if (!(await signInAllowed(request, env))) return error(429, "too many sign-ins, try again in a minute");
  const body = await readBody(request);
  if (body === "too big") return error(413, "the body is too big");
  const token = body?.["access_token"];
  if (typeof token !== "string" || token.length === 0 || token.length > MAX_ACCESS_TOKEN) return error(400, 'expected {"access_token": "..."}');
  const user = await verifyAndRevoke(token, env);
  if (user instanceof Response) return user;
  const subject = String(user.id);
  return link(env, s, "github", subject, user.login, (db) => [
    // The login the account shows is GitHub's while GitHub is linked, as at a
    // GitHub sign-in. Its verified address is kept when the account has none.
    db.prepare("UPDATE accounts SET login = ?2, email = COALESCE(email, ?3) WHERE id = ?1 AND changes() = 1").bind(s.accountId, user.login, user.email ?? null),
  ]);
}

async function askLinkCode(request: Request, env: Env, s: Session): Promise<Response> {
  const address = await askedAddress(request, env);
  if (address instanceof Response) return address;
  return mailCode(env, address, s.accountId, linkCodeMail);
}

async function linkEmail(request: Request, env: Env, s: Session): Promise<Response> {
  if (!(await signInAllowed(request, env))) return error(429, "too many sign-ins, try again in a minute");
  const body = await readBody(request);
  if (body === "too big") return error(413, "the body is too big");
  const address = normalAddress(body?.["email"]);
  const rawCode = body?.["code"];
  const code = typeof rawCode === "string" ? rawCode.replace(/\s/g, "") : null;
  if (!address || code === null || !CODE.test(code)) return error(400, 'expected {"email": "...", "code": "123456"}');
  if (!(await useCode(env, address, code, s.accountId))) return error(401, WRONG_CODE);
  return link(env, s, "email", address, address, (db) => [
    // An account made by GitHub sign-in has GitHub's address, or none.
    db.prepare("UPDATE accounts SET email = COALESCE(email, ?2) WHERE id = ?1 AND changes() = 1").bind(s.accountId, address),
  ]);
}

/// Links an identity whose owner was checked already, unless it signs in to
/// another account or the account has one of its provider. `then` is the
/// write that goes with the link, in the same transaction. A batch runs its
/// statements in order on one connection, so each of the chain asks
/// `changes()`, the rows the one before it wrote: the record is written only
/// if the link was, and `then` only if the record was.
async function link(
  env: Env,
  s: Session,
  provider: Provider,
  subject: string,
  name: string,
  then: (db: D1Database) => D1PreparedStatement[],
): Promise<Response> {
  const db = env.DB;
  const t = now();
  const before = await addresses(env, s.accountId);
  const results = await db.batch([
    db
      .prepare(
        `INSERT INTO identities (provider, subject, account_id, linked_at) SELECT ?1, ?2, ?3, ?4
         WHERE NOT EXISTS (SELECT 1 FROM identities WHERE provider = ?1 AND (subject = ?2 OR account_id = ?3))`,
      )
      .bind(provider, subject, s.accountId, t),
    db
      .prepare("INSERT INTO sign_in_changes (account_id, at, action, provider, subject, session_id) SELECT ?1, ?2, 'link', ?3, ?4, ?5 WHERE changes() = 1")
      .bind(s.accountId, t, provider, subject, s.id),
    ...then(db),
    db.prepare("SELECT account_id FROM identities WHERE provider = ?1 AND (subject = ?2 OR account_id = ?3)").bind(provider, subject, s.accountId),
  ]);
  if (results[0]!.meta.changes === 0) {
    const holders = (results.at(-1)!.results as { account_id: number }[]).map((r) => r.account_id);
    const what = provider === "github" ? "GitHub account" : "address";
    // Asked again for what is linked already: nothing changed.
    if (await isLinked(env, s.accountId, provider, subject)) return Response.json({ methods: await methods(env, s.accountId) });
    if (holders.some((id) => id !== s.accountId)) {
      return error(409, `that ${what} signs in to another Arakun account; accounts are never joined, so remove it there first`);
    }
    return error(409, `the account has ${provider === "github" ? "a GitHub account" : "an address"} already; remove it first`);
  }
  await tell(env, "link", provider, name, union(before, await addresses(env, s.accountId)));
  return Response.json({ methods: await methods(env, s.accountId) });
}

async function isLinked(env: Env, accountId: number, provider: Provider, subject: string): Promise<boolean> {
  const row = await env.DB.prepare("SELECT 1 FROM identities WHERE provider = ? AND subject = ? AND account_id = ?").bind(provider, subject, accountId).first();
  return row !== null;
}

/// Unlinks the account's method of this provider, unless it is the last.
async function remove(env: Env, s: Session, provider: Provider): Promise<Response> {
  const db = env.DB;
  const t = now();
  const gone = await db
    .prepare("SELECT i.subject, CASE WHEN i.provider = 'github' THEN a.login ELSE i.subject END AS name FROM identities i JOIN accounts a ON a.id = i.account_id WHERE i.account_id = ? AND i.provider = ?")
    .bind(s.accountId, provider)
    .first<{ subject: string; name: string }>();
  if (!gone) return error(404, `the account has no ${provider === "github" ? "GitHub" : "email"} sign-in`);
  const before = await addresses(env, s.accountId);
  // The checks are in each statement, so two removals at once cannot take
  // both: the one that runs second finds one method left.
  const others = "(SELECT count(*) FROM identities WHERE account_id = ?1 AND provider != ?2) > 0";
  const results = await db.batch([
    db
      .prepare(
        `INSERT INTO sign_in_changes (account_id, at, action, provider, subject, session_id)
         SELECT ?1, ?4, 'unlink', ?2, ?3, ?5 WHERE ${others}
         AND EXISTS (SELECT 1 FROM identities WHERE account_id = ?1 AND provider = ?2 AND subject = ?3)`,
      )
      .bind(s.accountId, provider, gone.subject, t, s.id),
    db.prepare(`DELETE FROM identities WHERE account_id = ?1 AND provider = ?2 AND subject = ?3 AND ${others}`).bind(s.accountId, provider, gone.subject),
    // Without GitHub the account shows its address, not a login that is no
    // longer its own.
    db
      .prepare(
        `UPDATE accounts SET login = (SELECT subject FROM identities WHERE account_id = ?1 AND provider = 'email')
         WHERE id = ?1 AND ?2 = 'github' AND NOT EXISTS (SELECT 1 FROM identities WHERE account_id = ?1 AND provider = 'github')
         AND EXISTS (SELECT 1 FROM identities WHERE account_id = ?1 AND provider = 'email')`,
      )
      .bind(s.accountId, provider),
  ]);
  if (results[1]!.meta.changes === 0) return error(409, "that is the account's last sign-in method; add another before you remove it");
  // The address removed hears of it too.
  await tell(env, "unlink", provider, gone.name, union(before, await addresses(env, s.accountId)));
  return Response.json({ methods: await methods(env, s.accountId) });
}

/// The addresses the account can be mailed at: the one it signs in with and
/// the one GitHub gave, in lower case.
async function addresses(env: Env, accountId: number): Promise<string[]> {
  const rows = await env.DB.prepare(
    `SELECT subject AS address FROM identities WHERE account_id = ?1 AND provider = 'email'
     UNION SELECT lower(email) FROM accounts WHERE id = ?1 AND email IS NOT NULL`,
  )
    .bind(accountId)
    .all<{ address: string }>();
  return rows.results.map((r) => r.address);
}

function union(a: string[], b: string[]): string[] {
  return [...new Set([...a, ...b])];
}

/// Mails each address about the change. The change stands if a mail fails:
/// it is recorded, and undoing it would leave the person who made it unsure
/// what happened.
async function tell(env: Env, action: "link" | "unlink", provider: Provider, name: string, to: string[]): Promise<void> {
  if (!env.EMAIL || !env.EMAIL_FROM) return;
  for (const address of to) {
    try {
      await env.EMAIL.send(changeMail(env.EMAIL_FROM, address, action, provider, name));
    } catch (e) {
      console.error("mailing a sign-in change failed:", (e as { code?: unknown }).code ?? (e as Error).message);
    }
  }
}

export function changeMail(from: string, to: string, action: "link" | "unlink", provider: Provider, name: string) {
  const method = provider === "github" ? `the GitHub account ${name}` : `the address ${name}`;
  const done = action === "link" ? `${method} was added to your Arakun account. It can sign in to the account now.` : `${method} was removed from your Arakun account. It no longer signs in to the account.`;
  const subject = action === "link" ? "A sign-in method was added to your Arakun account" : "A sign-in method was removed from your Arakun account";
  const lead = done.charAt(0).toUpperCase() + done.slice(1);
  const warn = "If you did not do this, sign in to Arakun, open Settings, then Account: remove what you do not know, and sign out the devices you do not know.";
  const text = [lead, "", warn].join("\n");
  const html = `<!doctype html>
<html><body style="margin:0;padding:32px 16px;background:#ffffff;color:#0a0a0a;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif">
<div style="max-width:420px;margin:0 auto">
<p style="font-size:15px;margin:0 0 16px">${escapeHtml(lead)}</p>
<p style="font-size:13px;color:#737373;margin:0">${escapeHtml(warn)}</p>
</div></body></html>`;
  return { from: { email: from, name: "Arakun" }, to, subject, text, html };
}

export function linkCodeMail(from: string, to: string, code: string) {
  return mailWithCode(from, to, code, {
    subject: `${code} is your code to add this address to Arakun`,
    lead: "Your code to add this address to your Arakun account is",
    use: "Type it in Arakun, where you asked for it. It works once, for 10 minutes.",
    ignore: "If you did not ask for it, ignore this email and give the code to nobody: with it, another account could sign in with your address.",
  });
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}
