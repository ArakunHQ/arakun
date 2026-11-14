// Starts sampler.mjs detached, so it keeps sampling after this step ends. The
// post step (stop.mjs) stops it and summarizes; the runner kills it anyway when
// the job ends.
import { spawn } from "node:child_process";
import { appendFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const dir = process.env.RUNNER_TEMP;
const file = join(dir, `runner-usage-${process.pid}.jsonl`);
const sampler = fileURLToPath(new URL("sampler.mjs", import.meta.url));
const child = spawn(process.execPath, [sampler, file], {
  detached: true,
  stdio: "ignore",
  windowsHide: true,
});
child.unref();

// Handed to the post step as state.
appendFileSync(process.env.GITHUB_STATE, `file=${file}\npid=${child.pid}\n`);
console.log(`sampling CPU and memory every 2 s into ${file}`);
