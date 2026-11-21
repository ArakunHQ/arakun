// Real workerd/SQLite and Docker. The private service binding substitutes
// only for Cloudflare's container transport, which is not billed locally.
import { execFile, spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { build } from "esbuild";
import { convertV4MiniflareOptions, Miniflare } from "miniflare";

const exec = promisify(execFile);
const [state, port, image] = process.argv.slice(2);
const token = (await readFile(resolve(state, "beta.env"), "utf8")).trim().split("=")[1];
const compiled = await build({ entryPoints: ["src/index.ts"], bundle: true, external: ["cloudflare:workers"], format: "esm", write: false });
const active = new Set();
async function runBox(req) {
  const lease = req.headers.get("x-lease");
  if (!/^[a-f0-9-]{36}$/.test(lease)) return new Response(null, { status: 400 });
  const name = `arakun-budget-${lease}`;
  const route = new URL(req.url).pathname;
  if (route === "/stopped") {
    if (active.has(name)) return new Response(null, { status: 409 });
    // Also works after restarting this harness. Do not release a lease when
    // Docker is unavailable: an unknown container isn't a stopped container.
    const { stdout } = await exec("docker", ["ps", "-q", "--filter", `name=^/${name}$`]);
    return new Response(null, { status: stdout.trim() ? 409 : 204 });
  }
  if (route !== "/review" && route !== "/test") return new Response(null, { status: 404 });
  const checkpoint = await req.text();
  const deadline = Number(req.headers.get("x-deadline"));
  const seconds = Math.floor((deadline - Date.now()) / 1000);
  if (!Number.isInteger(seconds) || seconds < 1 || seconds > 120 || active.has(name))
    return new Response(null, { status: 409 });
  active.add(name);
  try {
    // No mounted volumes or credentials. The deadline is also enforced
    // inside the container if the harness itself exits.
    const child = spawn("docker", ["run", "--rm", "-i", "--name", name, "--network", "none",
      "--cpus", "0.5", "--memory", "4g", "--memory-swap", "4g", "--pids-limit", "64",
      "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
      "--tmpfs", "/tmp:rw,nosuid,nodev,size=256m,mode=1777",
      "--tmpfs", "/app/teams/node_modules/.vite:rw,nosuid,nodev,size=32m,mode=1777",
      "--tmpfs", "/app/teams/node_modules/.vite-temp:rw,nosuid,nodev,size=32m,mode=1777",
      "--entrypoint", "timeout", image, "-s", "KILL", `${seconds}s`, "bun", "runner/budget-box.ts", "--offline", route.slice(1)],
    { stdio: ["pipe", "pipe", "inherit"] });
    child.stdin.on("error", () => {}); // Docker can fail before reading the checkpoint.
    child.stdin.end(checkpoint);
    const chunks = [];
    child.stdout.on("data", b => chunks.push(b));
    const code = await new Promise((res, rej) => { child.on("error", rej); child.on("close", res); });
    return code === 0 ? new Response(Buffer.concat(chunks)) : new Response(null, { status: 503 });
  } finally { active.delete(name); }
}

const mf = new Miniflare(convertV4MiniflareOptions({
  host: "127.0.0.1", port: Number(port), compatibilityDate: "2026-11-01",
  modules: true, script: compiled.outputFiles[0].text, bindings: { BETA_TOKEN: token },
  durableObjects: { BETA: { className: "BudgetMachine", useSQLite: true } },
  resourcePersistencePath: resolve(state, "local-objects"), serviceBindings: { OFFLINE_BOX: runBox },
}));
console.log(`Budget rehearsal: ${await mf.ready}`);
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, async () => { await mf.dispose(); process.exit(0); });
