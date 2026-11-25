#!/usr/bin/env node
// Prints a panel session cookie for a local run, so screenshots need no GitHub
// OAuth App: `node scripts/dev-cookie.mjs 1001 octo`. It signs with
// SESSION_SECRET from .dev.vars, as signSession in worker/session.ts does.
// The Worker still checks the ID against ADMIN_GITHUB_IDS. This file is in no
// build: the Worker has no way in without GitHub.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const SESSION_COOKIE = "__Host-arakun_admin";
const SESSION_SECONDS = 12 * 60 * 60;
const LOGIN = /^[A-Za-z0-9-]{1,39}$/;

const [idText, login] = process.argv.slice(2);
const githubId = Number(idText);
if (!/^\d+$/.test(idText ?? "") || !Number.isSafeInteger(githubId) || githubId < 1 || !LOGIN.test(login ?? "")) {
  console.error("usage: node scripts/dev-cookie.mjs <github_id> <login>");
  process.exit(2);
}

const varsPath = fileURLToPath(new URL("../.dev.vars", import.meta.url));
let secret;
for (const line of readFileSync(varsPath, "utf8").split("\n")) {
  const match = /^\s*SESSION_SECRET\s*=\s*(.*?)\s*$/.exec(line);
  if (match) secret = match[1].replace(/^(["'])(.*)\1$/, "$2");
}
if (!secret) {
  console.error(`no SESSION_SECRET in ${varsPath}`);
  process.exit(1);
}

function b64url(bytes) {
  return Buffer.from(bytes).toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const now = Math.floor(Date.now() / 1000);
const payload = b64url(new TextEncoder().encode(JSON.stringify({ github_id: githubId, login, exp: now + SESSION_SECONDS })));
const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(payload));
console.log(`${SESSION_COOKIE}=${payload}.${b64url(new Uint8Array(mac))}`);
