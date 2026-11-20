#!/usr/bin/env python3
"""Benchmarks Arakun: how fast the app is, and how well Claude works in it.

    tools/bench/bench.py perf  [--bin B] [--runs N] [--only a,b] [--label L] [--out F]
    tools/bench/bench.py agent [--bin B] [--model M] [--runner arakun,claude]
                               [--tasks a,b] [--repeat N] [--timeout S] [--out F]
    tools/bench/bench.py compare <before.json> <after.json> [--markdown]
    tools/bench/bench.py tasks

perf starts a test Arakun (`arakun dev start`) on a fixed scratch project with
ZED_MEASUREMENTS=1, so GPUI logs how long each frame took to draw. It then plays
scenarios through `arakun dev` (palette, sidebar, zoom, a terminal flood, a long
file scrolled) and reads, per scenario, the frames from the log and the CPU time
and memory of the process. Linux runs it on Xvfb; macOS in an unfocused window.

agent copies each task in tools/bench/tasks/ to a scratch git repository and
gives its prompt to Claude Code, then runs the task's hidden check. The arakun
runner starts the session in a test Arakun (`arakun agent new --model claude/M`)
and reads tokens and cost from its usage log; the claude runner runs `claude -p`
on its own, as the baseline without Arakun. Both runs bill the Claude account or
API key that Claude Code is logged in with.

Results go to target/bench/<kind>-<time>.json; compare prints the change
between two of them and exits 1 when perf got worse past the threshold.
"""

import argparse
import datetime
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TASKS = Path(__file__).resolve().parent / "tasks"
FRAME = re.compile(rb"frame duration: ([0-9.]+)(ns|\xc2\xb5s|us|ms|s)\b")
UNIT_MS = {b"ns": 1e-6, "µs".encode(): 1e-3, b"us": 1e-3, b"ms": 1.0, b"s": 1000.0}
# One frame at 60 Hz: a frame slower than this is a dropped one.
BUDGET_MS = 1000 / 60


def log(msg):
    print(msg, file=sys.stderr, flush=True)


# ---------------------------------------------------------------- test Arakun


class Dev:
    """A test Arakun driven through `arakun dev`."""

    def __init__(self, binary, cwd, env=None):
        self.bin = str(binary)
        self.cwd = str(cwd)
        self.env = {**os.environ, **(env or {})}
        self.root = None

    def run(self, *args, check=True, timeout=600):
        p = subprocess.run(
            [self.bin, "dev", *map(str, args)],
            cwd=self.cwd,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        if check and p.returncode != 0:
            raise RuntimeError(f"arakun dev {' '.join(map(str, args))}: {p.stderr.strip() or p.stdout.strip()}")
        return p.stdout.strip()

    def start(self, size="1440x900"):
        self.run("stop", check=False)
        self.root = Path(self.run("socket")).parent
        self.run("start", ".", "--size", size, timeout=90)

    def stop(self):
        self.run("stop", check=False)

    @property
    def log_file(self):
        return self.root / "arakun.log"

    def pid(self):
        return json.loads((self.root / "instance.json").read_text())["arakun"]


def frames_between(log_file, start, end):
    """Frame times (ms) GPUI logged between two byte offsets of the log."""
    with open(log_file, "rb") as f:
        f.seek(start)
        chunk = f.read(end - start)
    return [float(n) * UNIT_MS[u] for n, u in FRAME.findall(chunk)]


def log_size(log_file):
    try:
        return log_file.stat().st_size
    except FileNotFoundError:
        return 0


def cpu_seconds(pid):
    """User plus system CPU time of a process."""
    stat = Path(f"/proc/{pid}/stat")
    if stat.exists():
        fields = stat.read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    secs = 0.0
    for part in out.replace("-", ":").split(":"):
        secs = secs * 60 + float(part or 0)
    return secs


def rss_mb(pid):
    status = Path(f"/proc/{pid}/status")
    if status.exists():
        for line in status.read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) / 1024
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out or 0) / 1024


def pct(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    k = (len(s) - 1) * p / 100
    lo, hi = int(k), min(int(k) + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def frame_stats(frames):
    return {
        "frames": len(frames),
        "frame_mean_ms": round(statistics.fmean(frames), 3) if frames else 0.0,
        "frame_p50_ms": round(pct(frames, 50), 3),
        "frame_p95_ms": round(pct(frames, 95), 3),
        "frame_p99_ms": round(pct(frames, 99), 3),
        "frame_max_ms": round(max(frames), 3) if frames else 0.0,
        "frames_over_budget": sum(f > BUDGET_MS for f in frames),
    }


# ---------------------------------------------------------------- perf


def make_fixture(path):
    """A small project that is the same on every run, so runs compare."""
    path.mkdir(parents=True, exist_ok=True)
    (path / "README.md").write_text("# bench\n\nA scratch project for tools/bench.\n")
    lines = []
    for i in range(6000):
        lines.append(f"fn item_{i}(x: u64) -> u64 {{ x.wrapping_mul({i}) ^ 0x{i:04x} }} // line {i}")
    (path / "long.rs").write_text("\n".join(lines) + "\n")
    git = ["git", "-c", "user.name=bench", "-c", "user.email=bench@localhost", "-c", "commit.gpgsign=false"]
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    subprocess.run([*git, "add", "-A"], cwd=path, check=True)
    subprocess.run([*git, "commit", "-qm", "fixture"], cwd=path, check=True)


def scenario_idle(dev):
    time.sleep(10)


def scenario_palette(dev):
    for _ in range(8):
        dev.run("key", "ctrl-k")
        dev.run("type", "settings")
        time.sleep(0.2)
        dev.run("key", "escape", "escape")
        time.sleep(0.2)


def scenario_sidebar(dev):
    for _ in range(10):
        dev.run("key", "ctrl-b")
        time.sleep(0.25)


def scenario_zoom(dev):
    for _ in range(3):
        for key in ["ctrl-=", "ctrl-=", "ctrl-0"]:
            dev.run("key", key)
            time.sleep(0.3)


def scenario_scroll(dev):
    dev.run("open", "long.rs")
    time.sleep(1.0)
    w, h = 1440, 900
    # Positive is down: down the file, then back up.
    for dy in [400] * 30 + [-400] * 30:
        dev.run("scroll", w // 2, h // 2, dy)


def scenario_terminal(dev):
    # About 4 MB of output, then wait for the last line. `arakun run` joins its
    # arguments into one shell line, so the command goes as one string.
    dev.run("run", "--", "seq 1 600000; echo bench-done")
    deadline = time.monotonic() + 60
    while "bench-done" not in dev.run("logs", "-n", "10", check=False):
        if time.monotonic() > deadline:
            raise RuntimeError("the terminal scenario's output never finished")
        time.sleep(0.2)


SCENARIOS = {
    "idle": scenario_idle,
    "palette": scenario_palette,
    "sidebar": scenario_sidebar,
    "zoom": scenario_zoom,
    "scroll": scenario_scroll,
    "terminal": scenario_terminal,
}


def watch_first_frame(log_file, t0, found):
    """Sets found['ms'] when the first frame shows up in a log written after t0."""
    deadline = t0 + 60
    while time.monotonic() < deadline and "ms" not in found:
        try:
            if log_file.stat().st_mtime >= found["wall"] and FRAME.search(log_file.read_bytes()):
                found["ms"] = (time.monotonic() - t0) * 1000
                return
        except FileNotFoundError:
            pass
        time.sleep(0.005)


def perf_run(binary, only, workdir):
    dev = Dev(binary, workdir, {"ZED_MEASUREMENTS": "1"})
    dev.run("stop", check=False)
    dev.root = Path(dev.run("socket")).parent
    found = {"wall": time.time()}
    t0 = time.monotonic()
    watcher = threading.Thread(target=watch_first_frame, args=(dev.log_file, t0, found))
    watcher.start()
    dev.run("start", ".", "--size", "1440x900", timeout=90)
    ready_ms = (time.monotonic() - t0) * 1000
    watcher.join()
    out = {"startup": {"first_frame_ms": round(found.get("ms", 0.0), 1), "ready_ms": round(ready_ms, 1)}}
    try:
        pid = dev.pid()
        time.sleep(2)  # let the first layout and git scan settle
        out["startup"]["rss_mb"] = round(rss_mb(pid), 1)
        for name in only:
            log(f"  {name}")
            a, c, t = log_size(dev.log_file), cpu_seconds(pid), time.monotonic()
            SCENARIOS[name](dev)
            time.sleep(0.5)  # the last frame of the scenario
            b, wall = log_size(dev.log_file), time.monotonic() - t
            used = cpu_seconds(pid) - c
            out[name] = {
                **frame_stats(frames_between(dev.log_file, a, b)),
                "wall_s": round(wall, 2),
                "cpu_s": round(used, 3),
                "cpu_pct": round(100 * used / wall, 1),
                "rss_mb": round(rss_mb(pid), 1),
            }
    finally:
        dev.stop()
    return out


def median_runs(runs):
    """Per scenario and metric, the median over the runs."""
    merged = {}
    for run in runs:
        for scen, metrics in run.items():
            for k, v in metrics.items():
                merged.setdefault(scen, {}).setdefault(k, []).append(v)
    return {s: {k: round(statistics.median(v), 3) for k, v in m.items()} for s, m in merged.items()}


def cmd_perf(args):
    binary = resolve_bin(args.bin)
    only = args.only.split(",") if args.only else list(SCENARIOS)
    bad = [s for s in only if s not in SCENARIOS]
    if bad:
        sys.exit(f"unknown scenario {', '.join(bad)}; pick from {', '.join(SCENARIOS)}")
    runs = []
    with tempfile.TemporaryDirectory(prefix="arakun-bench-") as tmp:
        fixture = Path(tmp) / "project"
        make_fixture(fixture)
        for i in range(args.runs):
            log(f"perf run {i + 1}/{args.runs}")
            runs.append(perf_run(binary, only, fixture))
    result = {**meta("perf", binary), "runs": runs, "summary": median_runs(runs)}
    if args.label:
        result["label"] = args.label
    print_perf(result["summary"])
    save(result, args.out)


def print_perf(summary):
    cols = ["frames", "frame_p50_ms", "frame_p95_ms", "frame_max_ms", "frames_over_budget", "cpu_pct", "rss_mb"]
    if "startup" in summary:
        s = summary["startup"]
        print(f"startup: first frame {s.get('first_frame_ms')} ms, ready {s.get('ready_ms')} ms, {s.get('rss_mb')} MB")
    heads = ["frames", "p50 ms", "p95 ms", "max ms", ">16.7 ms", "cpu %", "rss MB"]
    print(f"{'scenario':<10}" + "".join(f"{h:>10}" for h in heads))
    for scen, m in summary.items():
        if scen != "startup":
            print(f"{scen:<10}" + "".join(f"{m.get(c, 0):>10}" for c in cols))


# ---------------------------------------------------------------- agent


def load_tasks(names):
    tasks = []
    for d in sorted(TASKS.iterdir()):
        if d.is_dir() and (d / "task.json").exists() and (not names or d.name in names):
            tasks.append((d, json.loads((d / "task.json").read_text())))
    missing = set(names or []) - {d.name for d, _ in tasks}
    if missing:
        sys.exit(f"unknown task {', '.join(sorted(missing))}; see `bench.py tasks`")
    return tasks


def prepare(task_dir, work):
    shutil.copytree(task_dir / "repo", work)
    # Claude Code answers these itself, so no permission question stops the run.
    # Only the scratch copy gets it.
    claude = work / ".claude"
    claude.mkdir(exist_ok=True)
    (claude / "settings.local.json").write_text(
        json.dumps({"permissions": {"allow": ["Bash", "Edit", "Write", "Read", "Glob", "Grep"]}}, indent=2)
    )
    git = ["git", "-c", "user.name=bench", "-c", "user.email=bench@localhost", "-c", "commit.gpgsign=false"]
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=work, check=True)
    subprocess.run([*git, "add", "-A"], cwd=work, check=True)
    subprocess.run([*git, "commit", "-qm", "task"], cwd=work, check=True)


def check_task(task_dir, spec, work):
    """Copies the hidden check in and runs it; the agent never saw it."""
    hidden = work / "bench_hidden"
    shutil.rmtree(hidden, ignore_errors=True)
    shutil.copytree(task_dir / "check", hidden)
    try:
        p = subprocess.run(spec["check"], shell=True, cwd=work, capture_output=True, text=True, timeout=300)
    except subprocess.TimeoutExpired:
        return False, "the check ran past 300 s", ""
    diff = subprocess.run(["git", "diff", "--stat", "HEAD"], cwd=work, capture_output=True, text=True).stdout
    return p.returncode == 0, (p.stdout + p.stderr)[-4000:], diff.strip()


def run_in_arakun(binary, model, prompt, work, timeout, out_dir):
    usage = out_dir / "usage.jsonl"
    dev = Dev(binary, work, {"ZED_MEASUREMENTS": "1", "ARAKUN_USAGE_LOG": str(usage)})
    dev.start()
    t0 = time.monotonic()
    try:
        reply = dev.run("agent", "new", "--model", f"claude/{model}", "--mode", "bypassPermissions",
                        "--title", "bench", "--", prompt)
        sid = reply.split()[0]
        a = log_size(dev.log_file)
        wait = subprocess.run([dev.bin, "dev", "agent", "wait", "--timeout", str(timeout), sid],
                              cwd=dev.cwd, env=dev.env, capture_output=True, text=True, timeout=timeout + 60)
        wall = time.monotonic() - t0
        lines = wait.stdout.strip().splitlines() or ["timeout"]
        status = lines[0] if wait.returncode in (0, 1) else "timeout"
        # `agent wait` prints the last reply, or the error, after the status.
        detail = "\n".join(lines[1:])[-2000:] or wait.stderr.strip()[-2000:]
        frames = frames_between(dev.log_file, a, log_size(dev.log_file))
        (out_dir / "transcript.txt").write_text(dev.run("agent", "log", "-n", "500", sid, check=False))
        dev.run("shot", out_dir / "end.png", check=False)
    finally:
        dev.stop()
    turns = [json.loads(l) for l in usage.read_text().splitlines() if l.strip()] if usage.exists() else []
    return {
        "status": status,
        "detail": detail,
        "wall_s": round(wall, 1),
        "agent_ms": sum(t.get("ms", 0) for t in turns),
        "turns": len(turns),
        "input_tokens": sum(t.get("input", 0) for t in turns),
        "output_tokens": sum(t.get("output", 0) for t in turns),
        "cache_read_tokens": sum(t.get("cache_read", 0) for t in turns),
        "cache_write_tokens": sum(t.get("cache_write", 0) for t in turns),
        "cost_usd": round(sum(t.get("cost", 0.0) for t in turns), 4),
        "cost_estimate": any(t.get("cost_estimate") for t in turns),
        "ui_frames": frame_stats(frames),
    }


def run_bare_claude(model, prompt, work, timeout, out_dir):
    claude = shutil.which("claude")
    if not claude:
        raise RuntimeError("the claude runner needs Claude Code's `claude` on PATH")
    t0 = time.monotonic()
    try:
        p = subprocess.run([claude, "-p", "--output-format", "json", "--model", model,
                            "--permission-mode", "bypassPermissions", prompt],
                           cwd=work, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"status": "timeout", "wall_s": float(timeout)}
    wall = time.monotonic() - t0
    (out_dir / "claude.json").write_text(p.stdout)
    try:
        r = json.loads(p.stdout)
    except json.JSONDecodeError:
        (out_dir / "claude.err").write_text(p.stderr)
        return {"status": "error", "wall_s": round(wall, 1)}
    u = r.get("usage", {})
    return {
        "status": "error" if r.get("is_error") else "idle",
        "wall_s": round(wall, 1),
        "agent_ms": r.get("duration_ms", 0),
        "turns": r.get("num_turns", 0),
        "input_tokens": u.get("input_tokens", 0),
        "output_tokens": u.get("output_tokens", 0),
        "cache_read_tokens": u.get("cache_read_input_tokens", 0),
        "cache_write_tokens": u.get("cache_creation_input_tokens", 0),
        "cost_usd": round(r.get("total_cost_usd", 0.0), 4),
    }


def cmd_agent(args):
    runners = args.runner.split(",")
    if set(runners) - {"arakun", "claude"}:
        sys.exit("--runner takes arakun, claude or both")
    binary = resolve_bin(args.bin) if "arakun" in runners else None
    tasks = load_tasks(args.tasks.split(",") if args.tasks else None)
    stamp = now()
    keep = ROOT / "target" / "bench" / f"agent-{stamp}"
    results = []
    for task_dir, spec in tasks:
        for runner in runners:
            for rep in range(args.repeat):
                name = f"{task_dir.name}/{runner}/{rep + 1}"
                log(f"{name}: running")
                out_dir = keep / task_dir.name / runner / str(rep + 1)
                out_dir.mkdir(parents=True, exist_ok=True)
                with tempfile.TemporaryDirectory(prefix="arakun-task-") as tmp:
                    work = Path(tmp) / task_dir.name
                    prepare(task_dir, work)
                    timeout = spec.get("timeout", args.timeout)
                    try:
                        if runner == "arakun":
                            r = run_in_arakun(binary, args.model, spec["prompt"], work, timeout, out_dir)
                        else:
                            r = run_bare_claude(args.model, spec["prompt"], work, timeout, out_dir)
                    except Exception as e:  # one broken run must not lose the others
                        r = {"status": "error", "error": str(e)}
                    passed, output, diff = check_task(task_dir, spec, work)
                    (out_dir / "check.txt").write_text(output)
                    (out_dir / "diff.txt").write_text(diff)
                r.update(task=task_dir.name, runner=runner, rep=rep + 1, passed=passed)
                log(f"{name}: {'pass' if passed else 'FAIL'} ({r.get('status')}, {r.get('wall_s')} s, ${r.get('cost_usd', 0)})")
                if not passed and r.get("status") != "idle" and (r.get("detail") or r.get("error")):
                    log(f"  {(r.get('detail') or r.get('error')).splitlines()[0]}")
                results.append(r)
    result = {**meta("agent", binary), "model": args.model, "results": results,
              "summary": agent_summary(results), "artifacts": str(keep)}
    print_agent(result["summary"])
    save(result, args.out, stamp)


def agent_summary(results):
    by = {}
    for r in results:
        by.setdefault(r["runner"], []).append(r)
    out = {}
    for runner, rs in by.items():
        out[runner] = {
            "runs": len(rs),
            "pass_rate": round(sum(r["passed"] for r in rs) / len(rs), 3),
            "wall_s_median": round(statistics.median(r.get("wall_s", 0) for r in rs), 1),
            "cost_usd_total": round(sum(r.get("cost_usd", 0) for r in rs), 4),
            "output_tokens_median": statistics.median(r.get("output_tokens", 0) for r in rs),
            "turns_median": statistics.median(r.get("turns", 0) for r in rs),
        }
    return out


def print_agent(summary):
    cols = ["runs", "pass_rate", "wall_s_median", "cost_usd_total", "output_tokens_median", "turns_median"]
    print(f"{'runner':<8}" + "".join(f"{c:>22}" for c in cols))
    for runner, m in summary.items():
        print(f"{runner:<8}" + "".join(f"{m[c]:>22}" for c in cols))


# ---------------------------------------------------------------- compare

# Perf metrics where up is worse, and how much noise to forgive.
WORSE_UP = {"frame_p50_ms", "frame_p95_ms", "frame_p99_ms", "frame_mean_ms", "frames_over_budget",
            "cpu_pct", "cpu_s", "rss_mb", "first_frame_ms", "ready_ms"}


def cmd_compare(args):
    a, b = (json.loads(Path(p).read_text()) for p in (args.before, args.after))
    if a["kind"] != b["kind"]:
        sys.exit("compare wants two results of the same kind")
    rows, worse = [], []
    for group in b["summary"]:
        for metric, new in b["summary"][group].items():
            old = a["summary"].get(group, {}).get(metric)
            if not isinstance(new, (int, float)) or not isinstance(old, (int, float)):
                continue
            # From nothing to something (an idle app that starts drawing) counts as worse.
            change = (new - old) / old * 100 if old else (float("inf") if new > old else 0.0)
            flag = ""
            # An idle app should draw nothing, so there any frame is worse.
            up_is_worse = metric in WORSE_UP or (group == "idle" and metric == "frames")
            # Small absolute changes are noise even when the percentage is large.
            if up_is_worse and change > args.threshold and new - old > noise_floor(metric):
                flag = "worse"
                worse.append(f"{group}.{metric}")
            elif up_is_worse and change < -args.threshold and old - new > noise_floor(metric):
                flag = "better"
            rows.append((group, metric, old, new, change, flag))
    # A label names the build that ran; the commit is only the checkout bench.py ran from.
    name = lambda r: r.get("label") or r.get("commit", "?")
    # As a list in Markdown, where two plain lines would run into one.
    dash = "- " if args.markdown else ""
    print(f"{dash}before: {name(a)} ({a.get('time')})\n{dash}after:  {name(b)} ({b.get('time')})\n")
    if args.markdown:
        print("| | metric | before | after | change | |\n|---|---|--:|--:|--:|---|")
        for g, m, o, n, c, f in rows:
            print(f"| {g} | {m} | {o} | {n} | {c:+.1f}% | {f} |")
    else:
        for g, m, o, n, c, f in rows:
            print(f"{g:<10} {m:<22} {o:>12} {n:>12} {c:>+8.1f}%  {f}")
    if worse and a["kind"] == "perf":
        print(f"\nworse past {args.threshold}%: {', '.join(worse)}")
        sys.exit(1)


def noise_floor(metric):
    if metric.endswith("_ms"):
        return 0.5
    if metric == "rss_mb":
        return 5
    if metric == "cpu_pct":
        return 2
    return 1


# ---------------------------------------------------------------- common


def resolve_bin(path):
    if path:
        b = Path(path).resolve()
    else:
        b = next((p for p in [ROOT / "target/release/arakun", ROOT / "target/debug/arakun"] if p.exists()), None)
    if not b or not b.exists():
        sys.exit("no arakun build; run `cargo build --release --locked --no-default-features` or pass --bin")
    if "debug" in b.parts:
        log("note: a debug build; perf numbers from it don't say much about a release")
    return b


def now():
    return datetime.datetime.now().strftime("%Y%m%d-%H%M%S")


def meta(kind, binary):
    git = lambda *a: subprocess.run(["git", *a], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    return {
        "kind": kind,
        "time": datetime.datetime.now().isoformat(timespec="seconds"),
        "commit": git("rev-parse", "--short", "HEAD"),
        "dirty": bool(git("status", "--porcelain")),
        "branch": git("branch", "--show-current"),
        "bin": str(binary) if binary else None,
        "host": {"os": platform.system(), "machine": platform.machine(), "cpus": os.cpu_count()},
    }


def save(result, out, stamp=None):
    path = Path(out) if out else ROOT / "target" / "bench" / f"{result['kind']}-{stamp or now()}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(result, indent=2) + "\n")
    print(f"\nsaved {path}")


def cmd_tasks(_args):
    for d, spec in load_tasks(None):
        print(f"{d.name:<22} {spec['prompt'].splitlines()[0][:90]}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("perf", help="frame times, CPU and memory of a test Arakun")
    p.add_argument("--bin", help="the arakun to test (default: target/release, else target/debug)")
    p.add_argument("--runs", type=int, default=3)
    p.add_argument("--only", help=f"scenarios, comma-separated: {','.join(SCENARIOS)}")
    p.add_argument("--label", help="a name for the build, such as its release tag; compare shows it")
    p.add_argument("--out")
    p.set_defaults(fn=cmd_perf)
    p = sub.add_parser("agent", help="Claude Code on the tasks, in Arakun and bare")
    p.add_argument("--bin")
    p.add_argument("--model", default="sonnet", help="a model Claude Code offers (default: sonnet)")
    p.add_argument("--runner", default="arakun", help="arakun, claude or arakun,claude")
    p.add_argument("--tasks", help="task names, comma-separated (default: all)")
    p.add_argument("--repeat", type=int, default=1)
    p.add_argument("--timeout", type=int, default=900, help="seconds a task may take")
    p.add_argument("--out")
    p.set_defaults(fn=cmd_agent)
    p = sub.add_parser("compare", help="the change between two results")
    p.add_argument("before")
    p.add_argument("after")
    p.add_argument("--threshold", type=float, default=10.0, help="percent worse that fails (default 10)")
    p.add_argument("--markdown", action="store_true")
    p.set_defaults(fn=cmd_compare)
    p = sub.add_parser("tasks", help="list the agent tasks")
    p.set_defaults(fn=cmd_tasks)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
