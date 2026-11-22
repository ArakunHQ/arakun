---
name: bench
description: Benchmark Arakun with tools/bench/bench.py - frame times, CPU and memory of a test Arakun across scripted scenarios (perf), and how well Claude Code does coding tasks inside Arakun against bare `claude -p` (agent) - then compare a change against main and report the difference. Use when the user asks to benchmark, profile or measure Arakun, check a perf change for regressions, or see how Claude performs in Arakun.
---

# Benchmark Arakun

`tools/bench/bench.py` has two suites. Both write JSON to `target/bench/`, and
`compare` turns two results into a table.

| Suite | What it runs | What it measures |
| --- | --- | --- |
| `perf` | A test Arakun (`arakun dev`) on a fixed scratch project, with `ZED_MEASUREMENTS=1` so GPUI logs each frame's draw time. Scenarios: idle, palette, sidebar, zoom, scroll, terminal, working (a fake agent, `fake_acp.py`, holds a session at work in a focused window) | Time to first frame, frames drawn, p50/p95/p99/max frame time, frames over 16.7 ms, CPU % and memory |
| `agent` | Each task in `tools/bench/tasks/` copied to a scratch git repo, its prompt given to Claude Code, then a hidden check run | Pass rate, time, turns, tokens, cost. Runner `arakun` runs the session in a test Arakun; `claude` runs bare `claude -p` as the baseline |

## Perf: check a change for regressions

Measure with a release build. Debug builds are several times slower and say
little about what users see.

1. Build the branch: `cargo build --release --locked --no-default-features`.
2. Build main in a worktree of its own, so your checkout stays as it is:
   ```sh
   git worktree add /tmp/arakun-main origin/main
   (cd /tmp/arakun-main && cargo build --release --locked --no-default-features)
   ```
3. Run both, one after the other on the same machine, never in parallel:
   ```sh
   tools/bench/bench.py perf --bin /tmp/arakun-main/target/release/arakun --out target/bench/main.json
   tools/bench/bench.py perf --out target/bench/branch.json
   tools/bench/bench.py compare target/bench/main.json target/bench/branch.json --markdown
   ```
   `compare` exits 1 when a metric where up is worse grew more than 10 % (`--threshold`)
   and past a noise floor (0.5 ms, 5 MB, 2 % CPU).
4. Something worse? Run that scenario again with `--only <name> --runs 5` before you
   believe it. Xvfb renders on the CPU, so the absolute numbers are higher than on a
   desktop GPU. Compare runs from the same machine only.
5. Remove the worktree: `git worktree remove /tmp/arakun-main`.

`--runs` (default 3) restarts the test Arakun each time. The summary is the median
per metric, and every run is kept under `runs` in the JSON.

To add a scenario, write a `scenario_<name>(dev)` in `bench.py` that drives the app
with `dev.run("key", …)`, `dev.run("type", …)`, `dev.run("scroll", …)` or any
`arakun dev` command, and add it to `SCENARIOS`. The run frames it from the log
offsets around it, so keep it self-contained and leave the app as it found it.

## Perf: compare two releases

`tools/bench/releases.sh [BEFORE [AFTER]]` downloads the Linux builds of two releases
with `gh`, checks them against `SHA256SUMS` and its signature (it needs `minisign`
for that; a release from before signing has no signature and runs on its checksum
alone, with a warning), runs `perf` on one after the other and writes `<tag>.json` and `comparison.md` to `target/bench/releases/`. Without
tags it takes the latest release and the one before it; a nightly's tag works too. It
takes `--runs`, `--only` and `--threshold`, and exits 1 like `compare`.

The **Benchmark releases** workflow runs the same script on a CI runner, on demand:

```sh
gh workflow run bench-releases.yml -f before=v0.10.0 -f after=v0.11.1
gh run watch          # the table is the run's summary; the JSON is the bench-releases artifact
```

A release from before `arakun dev` had the commands a scenario uses fails in that
scenario; leave it out with `--only` (the workflow's `scenarios` input).

## Agent: Claude in Arakun

The agent suite calls the Claude account or API key Claude Code is logged in
with, and costs money. Say so and get the user's go-ahead before you run it, and
start small: one task, the cheapest model.

```sh
tools/bench/bench.py tasks                                   # list them
tools/bench/bench.py agent --tasks fix-bug --model haiku     # one cheap smoke run
tools/bench/bench.py agent --runner arakun,claude --repeat 3 # Arakun against bare Claude Code
```

- Everything a run left behind goes to `target/bench/agent-<time>/<task>/<runner>/<n>/`:
  `transcript.txt` (the session as `arakun agent log` prints it), `diff.txt`, `check.txt`
  (the hidden check's output) and `end.png` (the window when the session ended).
  Read `check.txt` and `transcript.txt` before you explain a failure.
- `status` other than `idle` means the run didn't finish: `needs_input` (a permission
  question got through), `error` or `timeout`.
- For the `arakun` runner, `ui_frames` holds the frame times while the session ran,
  that is how smooth the app stayed while it streamed.
- Agent runs vary a lot. Use `--repeat 3` or more before you claim a difference
  between runners or models, and give the pass rate with the number of runs.

A task is a folder in `tools/bench/tasks/<name>/` with:

- `task.json`: `prompt`, `check` (a shell command run at the repo root; exit 0 is a
  pass) and `timeout` in seconds.
- `repo/`: the starting files. It becomes a fresh git repo with one commit.
- `check/`: the hidden tests, copied to `bench_hidden/` only after the agent is done.

Before you add a task, check that its hidden tests fail on `repo/` as it is and pass
on a fix you wrote yourself. Keep tasks small and in Python with the standard
library, so the check runs anywhere.

## Report

Lead with what changed and whether it matters. Give perf results as the `compare`
table, or as a ```chart bar chart of p95 frame time per scenario, before and after.
Give agent results as pass rate, median time and total cost per runner. Name the
build (commit, release or debug) and the machine, and say which runs you skipped.
