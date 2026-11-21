// Only use via workspace-demo.sh, with a new private HOME and no host keys.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { localControl, privateToken, ui } from "./workspace-local.mjs";

const [state, imageName] = process.argv.slice(2);
const fixture = await readFile(new URL("../runner/workspace-fixture.py", import.meta.url), "utf8");
const token = privateToken();

async function docker(args, input = "") {
  return new Promise((ok, fail) => {
    const p = execFile("docker", ["--host", "unix:///var/run/docker.sock", ...args],
      { timeout: 45_000, maxBuffer: 4096 }, (err, stdout) => {
        if (err) fail(new Error(`local docker ${args[0]} failed (${err.code ?? err.signal})`));
        else ok(stdout);
      });
    p.stdin.on("error", () => {}); // The daemon can reject a run before reading stdin.
    p.stdin.end(input);
  });
}

// Resolve a cached immutable ID, never pull or create volumes from image metadata.
const [image, volumes] = (await docker(["image", "inspect", "--format",
  '{{.Id}} {{len (or (index .Config "Volumes") "")}}', "--", imageName])).trim().split(" ");
assert.equal(volumes, "0", "executor image declares volumes");
assert.match(image, /^sha256:[0-9a-f]{64}$/);
const runs = [];
const active = new Set();
async function box(req) {
  const step = new URL(req.url).pathname.slice(1), lease = req.headers.get("x-lease");
  if (!/^[a-f0-9-]{36}$/.test(lease)) return new Response(null, { status: 400 });
  const name = `arakun-ws-${lease}`;
  if (step === "stopped") {
    if (active.has(name)) return new Response(null, { status: 409 });
    const remaining = await docker(["ps", "-aq", "--filter", `name=^/${name}$`]);
    return new Response(null, { status: remaining.trim() ? 409 : 204 });
  }
  const input = await req.text();
  const seconds = Math.floor((Number(req.headers.get("x-deadline")) - Date.now()) / 1000);
  if (!["capture", "restore"].includes(step) || !Number.isInteger(seconds) || seconds < 1 || seconds > 30 ||
      active.has(name) || Buffer.byteLength(input) > 512)
    return new Response(null, { status: 400 });
  active.add(name);
  try {
    const cid = join(state, lease + ".cid");
    const output = await docker(["run", "--rm", "--pull=never", "-i", "--cidfile", cid, "--name", name,
      "--network", "none", "--read-only", "--no-healthcheck", "--log-driver", "none",
      "--user", "65534:65534", "--cap-drop", "ALL",
      "--security-opt", "no-new-privileges", "--pids-limit", "32", "--cpus", "0.5",
      "--memory", "96m", "--memory-swap", "96m", "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=8m,mode=1777",
      "--workdir", "/tmp", "--entrypoint", "/usr/bin/timeout", image, "-s", "KILL", `${seconds}s`,
      "python3", "-I", "-c", fixture, step], input);
    const id = (await readFile(cid, "utf8")).trim();
    assert.match(id, /^[a-f0-9]{64}$/);
    assert.equal((await docker(["ps", "-aq", "--no-trunc", "--filter", `id=${id}`])).trim(), "");
    runs.push({ step, id, removed: true });
    return new Response(output);
  } finally { active.delete(name); }
}

let mf;
async function call(event) {
  const r = await ui(mf, token, event);
  const data = await r.json();
  assert.equal(r.status, 200, JSON.stringify(data));
  assert.equal(data.computeReserved, false);
  return data.task;
}
try {
  mf = await localControl(state, token, box);
  const a = await call("start");
  assert.equal(a.phase, "paused");
  assert.equal(runs.length, 1);
  await mf.dispose();
  mf = await localControl(state, token, box);
  // The UI has only a receipt. Neither the old executor nor the original
  // workerd process exists before the restore request is submitted.
  assert.deepEqual(await call(), a);
  const b = await call("resume");
  assert.equal(b.phase, "done");
  assert.equal(b.runs, 2);
  assert.equal(b.checkpoint.restored.sha256, a.checkpoint.sha256);
  assert.deepEqual(await call("resume"), b);
  assert.deepEqual(runs.map(r => r.step), ["capture", "restore"]);
  assert.notEqual(runs[0].id, runs[1].id);
  console.log(JSON.stringify({ runs, checkpoint: b.checkpoint, captureLeaseMs: a.used,
    restoreLeaseMs: b.used - a.used, runtimeRestarted: true }, null, 2));
} finally {
  await mf?.dispose();
}
