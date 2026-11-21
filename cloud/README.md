# Cloud sessions rehearsal

A local rehearsal of the two hard parts of running agent sessions on Cloudflare
Containers: a compute budget that a stuck or abandoned session cannot exceed, and
recovering uncommitted work after the machine that held it is gone.

Nothing here is deployed, and it does not change the app, the relay or Teams. It
runs on your computer with workerd, a Durable Object with SQLite storage, and
disposable local Docker containers. It never sends a prompt or uses a model seat.

## The compute budget

A Durable Object (`src/index.ts`) hands out one lease at a time and keeps the
accounts in SQLite. Its limits are in `src/budget.ts`:

- 20 hours of compute per UTC month,
- up to 2 hours per task,
- up to 2 minutes per step of this rehearsal.

Booting and stopping the machine count as compute. Waiting for CI or for a person
holds no lease. A durable alarm stops the machine at the end of the lease, even if
the Worker crashed in between. Duplicate events do not run a step again, and a
green CI event finishes a task without starting a machine. Failed work keeps its
last completed checkpoint, and `retry` reruns the interrupted step within the
original budget. A checkpoint on `/tasks` is a step receipt, not a backup of the
workspace or of a process.

The workload is the teams Worker as merged in #272 (`7f0cf45`): a type-check, then
its test suite when a red CI event arrives. Requests take no host, repository,
command or credential, and there is no public webhook.

Requires Linux, Docker, Node 22.18 or newer, Python 3 and OpenSSL.

```sh
npm ci
bash tools/budget-dev.sh                       # listens on 127.0.0.1:18747
python3 tools/budget.py my-task start --budget-seconds 90
python3 tools/budget.py my-task                # status, never boots a machine
python3 tools/budget.py my-task ci-red         # a failed CI notification
python3 tools/budget.py my-task ack            # a person saw the result
python3 tools/budget-smoke.py                  # end-to-end check
```

`budget-dev.sh` builds the image from `runner/budget-box.ts` and the committed
`teams/` tree only, never from a worktree, `.dev.vars` or `.git`. It keeps its
bearer token and state in `BETA_STATE_DIR` (default
`~/.local/state/arakun-cloud-rehearsal`). Containers run with no network, a
read-only root, 0.5 CPU and 4 GiB of memory.

## Recovering a dirty workspace

```sh
bash tools/workspace-demo.sh [image]
```

The demo uses a cached image with Git, Python 3 and `/usr/bin/timeout`, no
credentials and no declared volumes (default `swebench-sandbox:latest`). It never
builds or pulls one. It runs in a new temporary directory with an empty home.

1. Executor A creates a clean Git fixture, edits `tracked.txt` without staging it,
   creates `note.txt` and a decoy `.env`. It sends only those two files, not the
   checkout. Docker removes A, and the demo checks it is gone before going on.
2. The Worker accepts only the two expected paths and formats, stores one object
   in R2, reads it back and only then saves its SHA-256, size and base commit in
   SQLite. The demo restarts workerd on the same storage.
3. Before booting executor B, the Worker checks the object against SQLite. B
   creates the same clean base, applies the two files and hashes them again from
   disk. It succeeds only with the same SHA-256, exactly ` M tracked.txt` and
   `?? note.txt` in `git status`, and no `.env`. Asking to resume again does not
   boot a third executor.

A missing, truncated, altered or oversized object cannot boot B, and a failed
write or read-back saves no pointer. This is a fixed fixture, not a backup of any
repository, a secret scanner or a process snapshot. Edits made after the last
checkpoint are lost.

## Measured

On one Linux host, with local Docker:

- Budget smoke on #272: type-check 1.2 s, tests 13.0 s, 0 ms charged while
  waiting. At the [published rate](https://developers.cloudflare.com/containers/platform/pricing/)
  for standard-1 at full CPU ($0.074016 per hour), 14.2 s is about $0.0003. The
  lease times include Docker starting and stopping, but not building the image.
  They are not Cloudflare timings or an invoice: they exclude storage, egress,
  the control plane and inference.
- Workspace demo: A's lease took 575 ms and B's 304 ms. The 190-byte delta had the
  same SHA-256 before and after.

## Before a cloud test

`wrangler.jsonc` has no route, preview URL, production binding or secret, and
nothing here has been deployed or billed. Wrangler's local container transport
timed out on the first host, so `budget-dev.sh` runs the same image through a
Miniflare service binding instead (`BETA_NATIVE_LOCAL=1` tries Wrangler's).

Still to measure on Cloudflare: cold boot, checkpoint and restore, a release
build on standard-1, and a real model-backed task with its own spend limit. The
20-hour allowance limits compute this code accounts for, not the provider's
invoice, and it does not limit egress.
