#!/usr/bin/env node
// Makes an operator's authenticator key: `node scripts/totp-secret.mjs <github_id> <login>`.
// It prints the entry for ADMIN_TOTP_SECRETS and a link to open or turn into a
// QR code for the operator's authenticator app. Give both only to that
// operator, and keep neither in a file, a chat or the repository.

import { randomBytes } from "node:crypto";

const LOGIN = /^[A-Za-z0-9-]{1,39}$/;
const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

const [idText, login] = process.argv.slice(2);
const githubId = Number(idText);
if (!/^\d+$/.test(idText ?? "") || !Number.isSafeInteger(githubId) || githubId < 1 || !LOGIN.test(login ?? "")) {
  console.error("usage: node scripts/totp-secret.mjs <github_id> <login>");
  process.exit(2);
}

function base32(bytes) {
  let out = "";
  let bits = 0;
  let value = 0;
  for (const b of bytes) {
    value = (value << 8) | b;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out += BASE32[(value >> bits) & 31];
    }
  }
  if (bits > 0) out += BASE32[(value << (5 - bits)) & 31];
  return out;
}

// 160 bits, as RFC 4226 recommends; 20 bytes are 32 base32 characters exactly.
const key = base32(randomBytes(20));
const label = encodeURIComponent(`Arakun admin:${login}`);
console.log(`ADMIN_TOTP_SECRETS entry: ${githubId}:${key}`);
console.log(`Authenticator app link:   otpauth://totp/${label}?secret=${key}&issuer=${encodeURIComponent("Arakun admin")}`);
