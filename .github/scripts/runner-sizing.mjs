// Weekly runner sizing report (.github/workflows/runner-sizing.yml).
//
// Reads the "Runner usage" annotations that .github/actions/runner-usage leaves
// on every job it runs in, groups the successful jobs of the last DAYS days by
// workflow, job and runner label, and estimates for each group what a runner
// with half or double the CPUs would do to its duration and cost.
//
// The model, per 10% band of CPU use (time-weighted over all sampled runs):
// - Half the CPUs: a stretch that kept a share u of the CPUs busy needs at least
//   2u of the smaller runner, so it takes max(1, 2u) times as long. u is the
//   band's upper edge, which errs towards "downsizing is slower".
// - Double the CPUs: only time at 90%+ can get faster, and at best twice as
//   fast. A best case: it assumes the work there would spread over more CPUs.
// Depot's memory grows with the CPU count, so halving also halves the memory.
// Cost is CPU-minutes (CPUs x minutes); Depot bills per minute in proportion
// to the CPU count, so ratios between sizes hold whatever the rate.
//
// Env: GITHUB_TOKEN, GITHUB_REPOSITORY, optional GITHUB_API_URL, DAYS (7),
// GITHUB_STEP_SUMMARY (the report is appended there too) and OUT_JSON (path to
// write the raw numbers to).

import { appendFileSync, writeFileSync } from "node:fs";

const API = process.env.GITHUB_API_URL || "https://api.github.com";
const REPO = process.env.GITHUB_REPOSITORY;
const TOKEN = process.env.GITHUB_TOKEN;
const DAYS = Number(process.env.DAYS || 7);
if (!REPO || !TOKEN) throw new Error("GITHUB_REPOSITORY and GITHUB_TOKEN are required");

const MIN_RUNS = 3; // fewer sampled runs than this: no verdict
const MAX_HALF_SLOWDOWN = 1.15; // downsize if half the CPUs costs at most 15% time
const MEM_HEADROOM = 0.75; // the peak must stay under 75% of the smaller memory
const UPSIZE_SATURATED = 0.4; // upsize if 40%+ of the time is at 90%+ CPU...
const UPSIZE_MIN_SECONDS = 180; // ...and the job takes at least 3 minutes

async function gh(path) {
  for (let attempt = 1; ; attempt++) {
    const res = await fetch(`${API}${path}`, {
      headers: { authorization: `Bearer ${TOKEN}`, accept: "application/vnd.github+json", "x-github-api-version": "2022-11-28" },
    });
    if (res.ok) return res.json();
    if (attempt < 3 && (res.status >= 500 || res.status === 429)) {
      await new Promise((r) => setTimeout(r, 2000 * attempt));
      continue;
    }
    throw new Error(`GET ${path}: ${res.status} ${await res.text()}`);
  }
}

async function paged(path, key) {
  const out = [];
  for (let page = 1; ; page++) {
    const body = await gh(`${path}${path.includes("?") ? "&" : "?"}per_page=100&page=${page}`);
    out.push(...body[key]);
    if (body[key].length < 100) return out;
  }
}

// Runs fn over items, at most `limit` at a time.
async function pool(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  await Promise.all(Array.from({ length: limit }, async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i]);
    }
  }));
  return out;
}

const seconds = (a, b) => (new Date(b) - new Date(a)) / 1000;
const quantile = (xs, q) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor(q * s.length))] : NaN;
};

// The JSON line stop.mjs puts under the readable one.
async function usage(jobId) {
  const annotations = await gh(`/repos/${REPO}/check-runs/${jobId}/annotations`);
  const note = annotations.find((a) => a.title === "Runner usage");
  if (!note) return null;
  try {
    const data = JSON.parse(note.message.trim().split("\n").at(-1));
    return data.v === 1 ? data : null;
  } catch {
    return null;
  }
}

// Depot labels end in the CPU count ("depot-ubuntu-24.04-8"); none means 2.
function relabel(label, cpus) {
  if (!label.startsWith("depot-")) return `${cpus} CPUs`;
  return `${label.replace(/-\d+$/, "")}${cpus === 2 ? "" : `-${cpus}`}`;
}

function analyze(group) {
  const durations = group.jobs.map((j) => j.duration);
  const sampled = group.jobs.filter((j) => j.usage);
  const result = {
    workflow: group.workflow,
    job: group.job,
    label: group.label,
    runs: group.jobs.length,
    sampled_runs: sampled.length,
    duration_median: quantile(durations, 0.5),
    duration_p90: quantile(durations, 0.9),
    queue_median: quantile(group.jobs.map((j) => j.queue), 0.5),
  };
  if (!sampled.length) return result;

  const cpus = quantile(sampled.map((j) => j.usage.cpus), 0.5);
  const memTotal = quantile(sampled.map((j) => j.usage.mem_total), 0.5);
  const memPeak = Math.max(...sampled.map((j) => j.usage.mem_peak));
  const sampledSeconds = sampled.reduce((s, j) => s + j.usage.seconds, 0);
  const jobSeconds = sampled.reduce((s, j) => s + j.duration, 0);
  const hist = Array(10).fill(0);
  for (const j of sampled) j.usage.busy_hist.forEach((x, b) => { hist[b] += x; });
  const share = hist.map((x) => x / (sampledSeconds || 1));

  // Factors on the sampled part; the rest (job setup, checkout) stays as is.
  const covered = Math.min(sampledSeconds / (jobSeconds || 1), 1);
  const halfFactor = share.reduce((s, f, b) => s + f * Math.max(1, (2 * (b + 1)) / 10), 0);
  const doubleFactor = 1 - 0.5 * share[9];
  const half = 1 - covered + covered * halfFactor;
  const double = 1 - covered + covered * doubleFactor;

  Object.assign(result, {
    cpus,
    cpu_minutes: sampled.reduce((s, j) => s + (j.usage.cpus * j.duration) / 60, 0) * (group.jobs.length / sampled.length),
    busy_avg: sampled.reduce((s, j) => s + j.usage.busy_avg * j.usage.seconds, 0) / (sampledSeconds || 1),
    io_avg: sampled.reduce((s, j) => s + j.usage.io_avg * j.usage.seconds, 0) / (sampledSeconds || 1),
    saturated: share[9],
    mem_peak: memPeak,
    mem_total: memTotal,
    half_time: half,
    half_cost: 0.5 * half,
    double_time: double,
    double_cost: 2 * double,
  });

  const memFitsHalf = memPeak < MEM_HEADROOM * (memTotal / 2);
  if (sampled.length < MIN_RUNS) result.verdict = `Too few sampled runs (${sampled.length})`;
  else if (memPeak > 0.9 * memTotal) result.verdict = `**Memory-bound**: peak at ${pct(memPeak / memTotal)} of memory; try ${relabel(group.label, cpus * 2)}`;
  else if (cpus > 2 && half <= MAX_HALF_SLOWDOWN && memFitsHalf)
    result.verdict = `**Downsize** to \`${relabel(group.label, cpus / 2)}\`: about ${pct(1 - result.half_cost)} cheaper, ${slower(half)}`;
  else if (share[9] >= UPSIZE_SATURATED && result.duration_median >= UPSIZE_MIN_SECONDS)
    result.verdict = `**Upsize** to \`${relabel(group.label, cpus * 2)}\` might pay: at best ${pct(1 - double)} faster for ${result.double_cost.toFixed(2)}x the cost`;
  else result.verdict = "Right-sized";
  return result;
}

const pct = (x) => `${Math.round(x * 100)}%`;
const slower = (f) => (f <= 1.005 ? "no slower" : `about ${pct(f - 1)} slower`);
const dur = (s) => (s >= 60 ? `${Math.floor(s / 60)}m${String(Math.round(s % 60)).padStart(2, "0")}s` : `${Math.round(s)}s`);
const gib = (b) => `${(b / 2 ** 30).toFixed(1)} GiB`;

function report(results, since, until) {
  const lines = [`# Runner sizing, ${since} to ${until}`, ""];
  const measured = results.filter((r) => r.sampled_runs).sort((a, b) => b.cpu_minutes - a.cpu_minutes);
  const unmeasured = results.filter((r) => !r.sampled_runs).sort((a, b) => b.runs * b.duration_median - a.runs * a.duration_median);

  if (measured.length) {
    lines.push(
      "Successful jobs only, most CPU-minutes first. Half / double: estimated time and cost against today's runner (1.00x = the same).",
      "",
      "| Workflow / job | Runner | Runs | Median (p90) | CPU-min | CPU busy | At 90%+ | Memory peak | Half size | Double size | Verdict |",
      "|---|---|--:|--:|--:|--:|--:|--:|--:|--:|---|",
    );
    for (const r of measured) {
      lines.push(`| ${r.workflow} / ${r.job} | \`${r.label}\` (${r.cpus}) | ${r.runs}${r.sampled_runs < r.runs ? ` (${r.sampled_runs} sampled)` : ""} | ${dur(r.duration_median)} (${dur(r.duration_p90)}) | ${Math.round(r.cpu_minutes)} | ${pct(r.busy_avg)}${r.io_avg >= 0.05 ? ` (+${pct(r.io_avg)} I/O wait)` : ""} | ${pct(r.saturated)} | ${gib(r.mem_peak)} / ${gib(r.mem_total)} | ${r.half_time.toFixed(2)}x time, ${r.half_cost.toFixed(2)}x cost | ${r.double_time.toFixed(2)}x time, ${r.double_cost.toFixed(2)}x cost | ${r.verdict} |`);
    }
    lines.push(
      "",
      "<details><summary>How these are estimated</summary>",
      "",
      "Each job samples CPU and memory every 2 s (`.github/actions/runner-usage`). Half size: a stretch that kept a share u of the CPUs busy takes max(1, 2u) times as long on half of them, and memory halves too. Double size is a best case: only time at 90%+ CPU gets faster, at most twice. Cost is CPUs x minutes, which is how Depot's per-minute price scales. A high I/O wait means more CPUs will not help much. The thresholds are at the top of `.github/scripts/runner-sizing.mjs`.",
      "",
      "</details>",
      "",
    );
  } else {
    lines.push("No job in this period reported usage. Add `uses: ./.github/actions/runner-usage` after `actions/checkout` in the jobs to size.", "");
  }

  if (unmeasured.length) {
    lines.push("## Jobs without usage data", "", "| Workflow / job | Runner | Runs | Median (p90) |", "|---|---|--:|--:|");
    for (const r of unmeasured) lines.push(`| ${r.workflow} / ${r.job} | \`${r.label}\` | ${r.runs} | ${dur(r.duration_median)} (${dur(r.duration_p90)}) |`);
    lines.push("");
  }
  return lines.join("\n");
}

const until = new Date();
const since = new Date(until - DAYS * 86400e3);
const day = (d) => d.toISOString().slice(0, 10);

const runs = await paged(`/repos/${REPO}/actions/runs?created=%3E%3D${since.toISOString()}`, "workflow_runs");
console.log(`${runs.length} workflow runs since ${since.toISOString()}`);

const jobs = (await pool(runs, 8, async (run) => {
  const list = await paged(`/repos/${REPO}/actions/runs/${run.id}/jobs`, "jobs");
  return list
    .filter((j) => j.conclusion === "success" && j.started_at && j.completed_at)
    .map((j) => ({
      id: j.id,
      workflow: run.name,
      job: j.name,
      label: j.labels.join(", ") || "?",
      duration: seconds(j.started_at, j.completed_at),
      queue: Math.max(seconds(j.created_at, j.started_at), 0),
    }));
})).flat();
console.log(`${jobs.length} successful jobs; reading their annotations`);

await pool(jobs, 8, async (j) => { j.usage = await usage(j.id); });

const groups = new Map();
for (const j of jobs) {
  const key = `${j.workflow}\0${j.job}\0${j.label}`;
  if (!groups.has(key)) groups.set(key, { workflow: j.workflow, job: j.job, label: j.label, jobs: [] });
  groups.get(key).jobs.push(j);
}
const results = [...groups.values()].map(analyze);

const markdown = report(results, day(since), day(until));
console.log(markdown);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, markdown);
if (process.env.OUT_JSON) writeFileSync(process.env.OUT_JSON, JSON.stringify({ since, until, results }, null, 2));
