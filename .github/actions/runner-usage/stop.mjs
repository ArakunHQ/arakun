// Stops the sampler and reports the summary twice: a readable line for people
// and a JSON line for .github/scripts/runner-sizing.mjs, in one notice
// annotation titled "Runner usage".
import { existsSync, readFileSync } from "node:fs";
import os from "node:os";

const file = process.env.STATE_file;
const pid = Number(process.env.STATE_pid);
try { if (pid) process.kill(pid); } catch { /* already gone */ }

const samples = file && existsSync(file)
  ? readFileSync(file, "utf8").split("\n").filter(Boolean).flatMap((l) => { try { return [JSON.parse(l)]; } catch { return []; } })
  : [];
if (samples.length < 2) {
  console.log("::warning title=Runner usage::too few samples to summarize");
  process.exit(0);
}

const cpus = os.cpus().length;
const memTotal = os.totalmem();
// Each sample covers the time since the one before it (the first: 2 s).
let last = 0, seconds = 0, busySum = 0, ioSum = 0, memPeak = 0;
// Seconds spent in each 10% band of CPU use: [0-10%), [10-20%), ... [90-100%].
const hist = Array(10).fill(0);
for (const s of samples) {
  const dt = Math.max(s.t - last, 0) || 2;
  last = s.t;
  seconds += dt;
  busySum += s.busy * dt;
  ioSum += s.io * dt;
  memPeak = Math.max(memPeak, s.mem);
  hist[Math.min(Math.floor(s.busy * 10), 9)] += dt;
}

const round = (x, d = 3) => Number(x.toFixed(d));
const data = {
  v: 1,
  cpus,
  mem_total: memTotal,
  mem_peak: memPeak,
  seconds: round(seconds, 1),
  busy_avg: round(busySum / seconds),
  io_avg: round(ioSum / seconds),
  busy_hist: hist.map((x) => round(x, 1)),
};

const gib = (b) => (b / 2 ** 30).toFixed(1);
const pct = (x) => `${Math.round(x * 100)}%`;
const human =
  `${cpus} CPUs: ${pct(data.busy_avg)} busy on average, ${pct(hist[9] / seconds)} of the time at 90%+; ` +
  `memory peak ${gib(memPeak)} of ${gib(memTotal)} GiB; ${Math.round(seconds)} s sampled`;

console.log(human);
// Workflow commands escape % as %25 and newlines as %0A.
const escape = (m) => m.replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
console.log(`::notice title=Runner usage::${escape(`${human}\n${JSON.stringify(data)}`)}`);
