import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { BASE, MAX_BYTES, validate, workspaceStep } from "../src/workspace.ts";
import { localControl, privateToken, ui } from "../tools/workspace-local.mjs";

const files = { "tracked.txt": `base line\nedited: ${"a".repeat(32)}\n`, "note.txt": `new: ${"b".repeat(32)}\n` };
const pack = f => JSON.stringify({ v: 1, base: BASE, files: f });
const valid = pack(files);
const sha256 = createHash("sha256").update(valid).digest("hex");
const proof = { before: "clean", after: " M tracked.txt\n?? note.txt\n", sha256 };

test("only the canonical complete fixture delta passes", () => {
  validate(valid);
  for (const bad of [
    "{}", valid + "\n", valid.slice(0, -1), valid.replace(BASE, "1".repeat(40)),
    valid.replace('"v":1', '"v":0,"v":1'), valid.replace('"v":1', '"v":2'),
    pack({ "tracked.txt": files["tracked.txt"] }), pack({ ...files, ".env": "decoy" }),
    pack({ ...files, "../note.txt": "escape" }), pack({ ...files, "note.txt": "API_KEY=decoy" }),
    pack({ ...files, "tracked.txt": "x".repeat(MAX_BYTES) }),
  ]) assert.throws(() => validate(bad));
});

test("refuse a storage write failure, corrupt read-back or overlong executor stream", async () => {
  const capture = () => Promise.resolve(new Response(valid));
  await assert.rejects(workspaceStep("capture", null, { put() { throw Error("full"); } }, capture), /full/);
  await assert.rejects(workspaceStep("capture", null, {
    put: () => ({}), get: () => ({ size: valid.length, body: new Response(valid.replace("aa", "cc")).body }),
  }, capture), /invalid workspace/);
  await assert.rejects(workspaceStep("capture", null, {}, () => Promise.resolve(new Response("x".repeat(MAX_BYTES + 1)))),
    /oversize/);
});

test("SQLite never advances on partial capture, bad R2 data, or an unverified restore", async t => {
  const state = await mkdtemp(join(tmpdir(), "arakun-ws-test-"));
  const token = privateToken();
  let capture = pack({ ...files, ".env": "must not be stored" }), returned = { ...proof, sha256: "0".repeat(64) };
  const restores = [];
  const mf = await localControl(state, token, async req => {
    switch (new URL(req.url).pathname) {
      case "/stopped": return new Response(null, { status: 204 });
      case "/capture": return new Response(capture);
      case "/restore": restores.push(await req.text()); return Response.json(returned);
      default: return new Response(null, { status: 404 });
    }
  });
  t.after(async () => { await mf.dispose(); await rm(state, { force: true, recursive: true }); });
  const bucket = await mf.getR2Bucket("WORKSPACE_R2");
  async function call(event, code, phase) {
    const response = await ui(mf, token, event);
    assert.equal(response.status, code);
    const view = await response.json();
    assert.equal(view.task.phase, phase);
    assert.equal(view.computeReserved, false);
    return view.task.checkpoint;
  }
  assert.equal((await ui(mf, "wrong", "start")).status, 401);
  assert.equal((await ui(mf, token, "start", "upload")).status, 400);
  assert.equal((await mf.dispatchFetch("http://ui/workspaces/fixture/start", {
    method: "POST", headers: { authorization: `Bearer ${token}`, "x-budget-seconds": "31" },
  })).status, 400);
  assert.equal((await ui(mf, token, "resume")).status, 409);
  assert.equal(await call("start", 503, "interrupted"), null);
  assert.equal((await bucket.list()).objects.length, 0);
  capture = valid;
  const ref = await call("retry", 200, "paused");
  assert.deepEqual(ref, { v: 1, base: BASE, sha256, bytes: valid.length });
  const key = (await bucket.list()).objects[0].key;
  assert.equal(await (await bucket.get(key)).text(), valid);
  await bucket.delete(key);
  assert.deepEqual(await call("resume", 503, "interrupted"), ref);
  for (const bad of [valid.slice(0, -3), valid.replace("aa", "cc"), "x".repeat(MAX_BYTES + 1)]) {
    await bucket.put(key, bad);
    assert.deepEqual(await call("retry", 503, "interrupted"), ref);
  }
  assert.equal(restores.length, 0, "bad storage must not boot the second executor");
  await bucket.put(key, valid);
  assert.deepEqual(await call("retry", 503, "interrupted"), ref);
  returned = proof;
  const completed = await call("retry", 200, "done");
  assert.deepEqual(completed, { ...ref, restored: proof });
  assert.deepEqual(await call("resume", 200, "done"), completed);
  assert.deepEqual(restores, [valid, valid]);
  // The original receipt API must not see this workspace, even at the same name.
  assert.equal((await mf.dispatchFetch("http://ui/tasks/fixture", {
    headers: { authorization: `Bearer ${token}` },
  })).status, 404);
});
