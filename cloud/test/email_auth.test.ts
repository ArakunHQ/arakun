import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { MIN_NEW_ACCOUNT_ID } from "../src/auth";
import { newCode, normalAddress } from "../src/email_auth";
import { as, askCode, call, freshIp, github, lastCode, mail, nowSeconds, roomInWindow, signIn, signInByEmail, verifyCode } from "./helpers";

function webVerify(email: string, code: string, headers: Record<string, string> = {}): Promise<Response> {
  return SELF.fetch("https://teams.test/auth/email/web", {
    method: "POST",
    headers: { "Content-Type": "application/json", "CF-Connecting-IP": freshIp(), Origin: "https://teams.test", ...headers },
    body: JSON.stringify({ email, code }),
  });
}

describe("asking for a sign-in code", () => {
  it("mails a six-digit code from the sign-in address and keeps only its hash", async () => {
    const res = await askCode("ada@example.com");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ sent: true, expires_in: 600 });
    expect(mail.sent).toHaveLength(1);
    const sent = mail.sent[0]!;
    expect(sent.from).toEqual({ email: "sign-in@arakun.dev", name: "Arakun" });
    expect(sent.to).toBe("ada@example.com");
    const code = lastCode("ada@example.com");
    expect(sent.subject).toBe(`${code} is your Arakun sign-in code`);
    expect(sent.html).toContain(code);
    // A code, not a link.
    expect(sent.text).not.toMatch(/https?:/);

    const row = await env.DB.prepare("SELECT * FROM email_codes").first<Record<string, unknown>>();
    expect(row).toMatchObject({ address: "ada@example.com", tries: 0, sends: 1 });
    expect(JSON.stringify(row)).not.toContain(code);
  });

  it("answers the same whether the address has an account or not", async () => {
    await signInByEmail("known@example.com");
    const known = await askCode("known@example.com");
    const unknown = await askCode("unknown@example.com");
    expect(known.status).toBe(unknown.status);
    expect(await known.text()).toBe(await unknown.text());
  });

  it("keeps the address in lower case without spaces around it", async () => {
    await askCode("  Ada@Example.COM ");
    expect(mail.sent[0]!.to).toBe("ada@example.com");
  });

  it("refuses what is not an address, before mailing anything", async () => {
    for (const email of [undefined, 12, "", "ada", "ada@", "@example.com", "ada@example", "a b@example.com", "ada@exa_mple.com", `${"a".repeat(250)}@example.com`]) {
      const res = await askCode(email);
      expect(res.status, String(email)).toBe(400);
    }
    expect(mail.sent).toEqual([]);
  });

  it("refuses throwaway mail services, under their subdomains too, and the operator's own list", async () => {
    await env.CONFIG.put("blocked_email_domains", JSON.stringify(["burner.test"]));
    for (const email of ["x@mailinator.com", "x@eu.mailinator.com", "x@burner.test"]) {
      const res = await askCode(email);
      expect(res.status, email).toBe(400);
      expect(await res.json()).toEqual({ error: "that address is from a throwaway mail service; use one you keep" });
    }
    expect(mail.sent).toEqual([]);
    expect((await askCode("x@notmailinator.com")).status).toBe(200);
  });

  it("ignores an operator list that is not a list", async () => {
    await env.CONFIG.put("blocked_email_domains", "{not json");
    expect((await askCode("x@example.com")).status).toBe(200);
  });

  it("mails one address at most five codes an hour, and more once the hour is over", async () => {
    for (let i = 0; i < 5; i++) expect((await askCode("ada@example.com")).status).toBe(200);
    const res = await askCode("ada@example.com");
    expect(res.status).toBe(429);
    expect(mail.sent).toHaveLength(5);
    // The last code still works.
    const code = lastCode("ada@example.com");

    await env.DB.prepare("UPDATE email_codes SET window_start = window_start - 3600").run();
    expect((await askCode("ada@example.com")).status).toBe(200);
    expect(await env.DB.prepare("SELECT sends FROM email_codes").first("sends")).toBe(1);
    expect((await verifyCode("ada@example.com", code)).status).toBe(401);
    expect((await verifyCode("ada@example.com", lastCode("ada@example.com"))).status).toBe(200);
  });

  it("mails three codes a minute to one client IP", async () => {
    await roomInWindow(60);
    const ip = freshIp();
    for (let i = 0; i < 3; i++) expect((await askCode(`p${i}@example.com`, ip)).status).toBe(200);
    expect((await askCode("p3@example.com", ip)).status).toBe(429);
    expect((await askCode("p3@example.com")).status).toBe(200);
  });

  it("clears the rows of addresses whose code and hour are both over", async () => {
    await askCode("old@example.com");
    await env.DB.prepare("UPDATE email_codes SET window_start = window_start - 3600, expires_at = expires_at - 3600").run();
    await askCode("new@example.com");
    const rows = await env.DB.prepare("SELECT address FROM email_codes").all<{ address: string }>();
    expect(rows.results.map((r) => r.address)).toEqual(["new@example.com"]);
  });

  it("answers 502 when the mail cannot go, without the code or the address in the log", async () => {
    mail.failure = "E_DELIVERY_FAILED";
    const errors: unknown[][] = [];
    const original = console.error;
    console.error = (...args: unknown[]) => errors.push(args);
    try {
      const res = await askCode("ada@example.com");
      expect(res.status).toBe(502);
    } finally {
      console.error = original;
    }
    expect(errors.flat().join(" ")).toContain("E_DELIVERY_FAILED");
    expect(errors.flat().join(" ")).not.toContain("ada@example.com");
  });
});

describe("signing in with a code", () => {
  it("makes an account named by the address, linked to it, with a session", async () => {
    const got = await signInByEmail("ada@example.com");
    expect(got.account_id).toBeGreaterThanOrEqual(MIN_NEW_ACCOUNT_ID);
    expect(got.login).toBe("ada@example.com");
    expect(got.plan).toBe("free");
    expect(got.session).toMatch(/^[A-Za-z0-9_-]{43}$/);

    const account = await env.DB.prepare("SELECT login, email FROM accounts WHERE id = ?").bind(got.account_id).first();
    expect(account).toEqual({ login: "ada@example.com", email: "ada@example.com" });
    const identities = await env.DB.prepare("SELECT provider, subject FROM identities WHERE account_id = ?").bind(got.account_id).all();
    expect(identities.results).toEqual([{ provider: "email", subject: "ada@example.com" }]);

    const me = await call("GET", "/me", as(got.session));
    expect(await me.json()).toMatchObject({ account_id: got.account_id, login: "ada@example.com" });
    // The session is named by the address, unless the app names it.
    const sessions = await (await call("GET", "/sessions", as(got.session))).json<{ name: string; kind: string }[]>();
    expect(sessions).toMatchObject([{ name: "ada@example.com", kind: "app" }]);
  });

  it("signs in to the same account the next time, in any case of the address", async () => {
    const first = await signInByEmail("ada@example.com");
    const second = await signInByEmail("ADA@example.com", { name: "Ada's phone", kind: "ios" });
    expect(second.account_id).toBe(first.account_id);
    const sessions = await (await call("GET", "/sessions", as(second.session))).json<{ name: string; kind: string }[]>();
    // Made in the same second, so listed in no set order.
    expect(sessions.map((s) => [s.name, s.kind]).sort()).toEqual([
      ["Ada's phone", "ios"],
      ["ada@example.com", "app"],
    ]);
  });

  it("signs in to the GitHub account whose verified address it is, which keeps its login", async () => {
    github.emails = [{ email: "Octo@Example.com", primary: true, verified: true }];
    const gh = await signIn(1001, "octocat");
    const got = await signInByEmail("octo@example.com");
    expect(got.account_id).toBe(gh.account_id);
    expect(got.login).toBe("octocat");
    const account = await env.DB.prepare("SELECT login, email FROM accounts WHERE id = ?").bind(gh.account_id).first();
    expect(account).toEqual({ login: "octocat", email: "Octo@Example.com" });
    // And with GitHub again, the same account.
    expect((await signIn(1001, "octocat")).account_id).toBe(gh.account_id);
  });

  it("makes a new account when two accounts have the address, rather than pick one", async () => {
    github.emails = [{ email: "shared@example.com", primary: true, verified: true }];
    const a = await signIn(1001, "one");
    const b = await signIn(1002, "two");
    const got = await signInByEmail("shared@example.com");
    expect([a.account_id, b.account_id]).not.toContain(got.account_id);
    expect(got.login).toBe("shared@example.com");
  });

  it("makes a new account when the matching account has another address linked", async () => {
    github.emails = [{ email: "new@example.com", primary: true, verified: true }];
    const gh = await signIn(1001, "octocat");
    // Linked by its old address, before GitHub's changed.
    await env.DB.prepare("INSERT INTO identities (provider, subject, account_id, linked_at) VALUES ('email', 'old@example.com', ?, 0)").bind(gh.account_id).run();
    const got = await signInByEmail("new@example.com");
    expect(got.account_id).not.toBe(gh.account_id);
  });

  it("takes the code with spaces in it", async () => {
    await askCode("ada@example.com");
    const code = lastCode("ada@example.com");
    expect((await verifyCode("ada@example.com", ` ${code.slice(0, 3)} ${code.slice(3)} `)).status).toBe(200);
  });

  it("takes a code once", async () => {
    await askCode("ada@example.com");
    const code = lastCode("ada@example.com");
    expect((await verifyCode("ada@example.com", code)).status).toBe(200);
    const again = await verifyCode("ada@example.com", code);
    expect(again.status).toBe(401);
    expect(await again.json()).toEqual({ error: "the code is wrong or has run out; ask for a new one" });
  });

  it("takes five tries, and after five wrong ones not even the right code", async () => {
    await askCode("ada@example.com");
    const code = lastCode("ada@example.com");
    const wrong = code === "000000" ? "000001" : "000000";
    for (let i = 0; i < 5; i++) expect((await verifyCode("ada@example.com", wrong)).status).toBe(401);
    expect((await verifyCode("ada@example.com", code)).status).toBe(401);
    // A new code has its own tries.
    await askCode("ada@example.com");
    expect((await verifyCode("ada@example.com", lastCode("ada@example.com"))).status).toBe(200);
  });

  it("refuses a code after ten minutes", async () => {
    await askCode("ada@example.com");
    await env.DB.prepare("UPDATE email_codes SET expires_at = ?").bind(nowSeconds()).run();
    expect((await verifyCode("ada@example.com", lastCode("ada@example.com"))).status).toBe(401);
  });

  it("refuses the code before a new one, and a code of another address", async () => {
    await askCode("ada@example.com");
    const first = lastCode("ada@example.com");
    await askCode("ada@example.com");
    const second = lastCode("ada@example.com");
    if (first !== second) expect((await verifyCode("ada@example.com", first)).status).toBe(401);
    await askCode("bob@example.com");
    if (lastCode("bob@example.com") !== second) expect((await verifyCode("bob@example.com", second)).status).toBe(401);
  });

  it("answers 401 for an address that asked for no code, as for a wrong one", async () => {
    const res = await verifyCode("nobody@example.com", "123456");
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "the code is wrong or has run out; ask for a new one" });
  });

  it("refuses a body without an address and a six-digit code, or with a bad name or kind", async () => {
    await askCode("ada@example.com");
    const code = lastCode("ada@example.com");
    expect((await verifyCode("ada@example.com", "12345")).status).toBe(400);
    expect((await verifyCode("ada@example.com", "abcdef")).status).toBe(400);
    expect((await verifyCode("not an address", code)).status).toBe(400);
    expect((await verifyCode("ada@example.com", code, { kind: "cloud" })).status).toBe(400);
    expect((await verifyCode("ada@example.com", code, { name: "" })).status).toBe(400);
    // None of them was a try.
    expect(await env.DB.prepare("SELECT tries FROM email_codes").first("tries")).toBe(0);
  });

  it("answers 429 after ten sign-ins a minute from one IP", async () => {
    await roomInWindow(60);
    const ip = freshIp();
    for (let i = 0; i < 10; i++) expect((await verifyCode("ada@example.com", "123456", {}, ip)).status).not.toBe(429);
    expect((await verifyCode("ada@example.com", "123456", {}, ip)).status).toBe(429);
  });
});

describe("signing in on the page with a code", () => {
  it("sets the session cookie and answers with no token", async () => {
    await askCode("ada@example.com");
    const res = await webVerify("ada@example.com", lastCode("ada@example.com"), { "User-Agent": "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" });
    expect(res.status).toBe(204);
    expect(await res.text()).toBe("");
    const cookie = res.headers.get("Set-Cookie")!;
    expect(cookie).toMatch(/^__Host-arakun_session=[A-Za-z0-9_-]{43}; HttpOnly; Secure; SameSite=Lax; Path=\/; Max-Age=15552000$/);
    const token = /=([^;]+)/.exec(cookie)![1]!;
    const sessions = await (await call("GET", "/sessions", as(token))).json<{ name: string; kind: string }[]>();
    expect(sessions).toMatchObject([{ name: "iPhone", kind: "phone" }]);
  });

  it("refuses a request from another origin, before the code is tried", async () => {
    await askCode("ada@example.com");
    const code = lastCode("ada@example.com");
    expect((await webVerify("ada@example.com", code, { Origin: "https://evil.test" })).status).toBe(403);
    const none = await SELF.fetch("https://teams.test/auth/email/web", { method: "POST", body: JSON.stringify({ email: "ada@example.com", code }) });
    expect(none.status).toBe(403);
    expect((await webVerify("ada@example.com", code)).status).toBe(204);
  });

  it("ends the session the browser held before, of the same account", async () => {
    await askCode("ada@example.com");
    const first = await webVerify("ada@example.com", lastCode("ada@example.com"));
    const old = /=([^;]+)/.exec(first.headers.get("Set-Cookie")!)![1]!;
    await askCode("ada@example.com");
    const second = await webVerify("ada@example.com", lastCode("ada@example.com"), { Cookie: `__Host-arakun_session=${old}` });
    expect(second.status).toBe(204);
    expect((await call("GET", "/me", as(old))).status).toBe(401);
  });
});

describe("codes and addresses", () => {
  it("draws codes of six digits", () => {
    for (let i = 0; i < 200; i++) expect(newCode()).toMatch(/^[0-9]{6}$/);
  });

  it("normalizes addresses", () => {
    expect(normalAddress(" A.B+c@Example.ORG ")).toBe("a.b+c@example.org");
    expect(normalAddress("a@b@example.org")).toBeNull();
    expect(normalAddress("a@example..org")).toBeNull();
  });
});
