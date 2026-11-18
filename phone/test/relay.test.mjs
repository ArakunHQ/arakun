// phone/relay.js joining the pieces of a message the computer split for the
// relay (`say` in src/phone/relay.rs), against the pieces Rust wrote
// (vectors/part.json). Run with `node --test phone/test/`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
for (const file of ["e2e.js", "relay.js"]) vm.runInThisContext(readFileSync(join(here, "..", file), "utf8"), { filename: file });
const { E2E, RELAY } = globalThis;
const vectors = JSON.parse(readFileSync(join(here, "vectors", "part.json"), "utf8"));

test("the pieces the computer makes join into its message", () => {
  for (const c of vectors.cases) {
    const joined = RELAY.joiner();
    const got = c.parts.map((p) => joined(JSON.parse(p).part));
    assert.ok(got.slice(0, -1).every((m) => m === null), `chunk ${c.chunk}`);
    assert.deepEqual(got.at(-1), JSON.parse(vectors.message), `chunk ${c.chunk}`);
  }
  assert.ok(vectors.cases[0].parts.length > 1);
});

/// What the computer does: the message's bytes in pieces of `size`.
function pieces(msg, size) {
  const bytes = E2E.utf8(JSON.stringify(msg));
  const out = [];
  for (let at = 0; at < bytes.length; at += size) out.push({ body: E2E.b64url(bytes.subarray(at, at + size)), more: at + size < bytes.length });
  return out;
}

test("the pieces of a big message join into the message", () => {
  // Letters of more than one byte, and a piece size that ends inside them.
  const msg = { type: "entries", text: "é€".repeat(300_000) };
  const parts = pieces(msg, 160 * 1024 + 1);
  assert.ok(parts.length > 5);
  const joined = RELAY.joiner();
  for (const part of parts.slice(0, -1)) assert.equal(joined(part), null);
  assert.deepEqual(joined(parts.at(-1)), msg);
});

test("the next message starts from nothing", () => {
  const joined = RELAY.joiner();
  for (const msg of [{ a: "x".repeat(10) }, { b: 2 }]) {
    const got = pieces(msg, 4).map(joined);
    assert.deepEqual(got.at(-1), msg);
    assert.ok(got.slice(0, -1).every((m) => m === null));
  }
});
