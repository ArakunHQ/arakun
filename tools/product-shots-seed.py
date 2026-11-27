#!/usr/bin/env python3
"""Makes the mock data for tools/product-shots.sh.

    tools/product-shots-seed.py PROJECT SEED

PROJECT is an empty folder. It becomes a small TypeScript project, brave-otter,
with a short git history and the uncommitted edits of one Claude Code session.

SEED gets what a test Arakun starts from: state.json, with PROJECT as its only
project and the session open, and data/arakun/sessions/<id>.json, the session's
transcript. It also gets mock-acp.py, a stand-in for Claude Code's ACP adapter
to put in ARAKUN_CLAUDE_ACP: a restored session connects to its agent, and
without one the shot shows an offer to install it. Its edits are the ones left in PROJECT, so the session, the Changes
list and the diffs agree. `arakun dev start --seed SEED` reads this layout. To run a
test Arakun on it by hand, copy state.json to SEED/data/arakun/ and start it
with ARAKUN_STATE=SEED/data/arakun/state.json and XDG_DATA_HOME=SEED/data:
its files are next to its state.

Everything here is made up: no real person, path, host or account.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

AUTHOR = ("Sam Rivera", "sam@example.com")

README = """\
# brave-otter

A small link shortener. `POST /shorten` takes a URL and returns a short code;
`GET /:code` redirects to it.

```sh
npm install
npm run dev
```
"""

PACKAGE = """\
{
  "name": "brave-otter",
  "version": "0.3.0",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "tsx watch src/server.ts",
    "test": "vitest run"
  },
  "dependencies": {
    "hono": "^4.6.0"
  },
  "devDependencies": {
    "tsx": "^4.19.0",
    "typescript": "^5.6.0",
    "vitest": "^2.1.0"
  }
}
"""

GITIGNORE = "node_modules/\ndist/\n"

STORE = """\
const links = new Map<string, string>();

const ALPHABET = "abcdefghijkmnopqrstuvwxyz23456789";

export function shorten(url: string): string {
  let code = "";
  for (let i = 0; i < 6; i++) {
    code += ALPHABET[Math.floor(Math.random() * ALPHABET.length)];
  }
  links.set(code, url);
  return code;
}

export function resolve(code: string): string | undefined {
  return links.get(code);
}
"""

SERVER_V1 = """\
import { Hono } from "hono";
import { resolve, shorten } from "./store";

export const app = new Hono();

app.post("/shorten", async (c) => {
  const { url } = await c.req.json<{ url?: string }>();
  if (!url || !URL.canParse(url)) {
    return c.json({ error: "a valid url is required" }, 400);
  }
  return c.json({ code: shorten(url) }, 201);
});

app.get("/:code", (c) => {
  const url = resolve(c.req.param("code"));
  return url ? c.redirect(url, 302) : c.notFound();
});
"""

SERVER_V2 = """\
import { Hono } from "hono";
import { rateLimit } from "./rateLimit";
import { resolve, shorten } from "./store";

export const app = new Hono();

app.post("/shorten", rateLimit({ limit: 20, windowMs: 60_000 }), async (c) => {
  const { url } = await c.req.json<{ url?: string }>();
  if (!url || !URL.canParse(url)) {
    return c.json({ error: "a valid url is required" }, 400);
  }
  return c.json({ code: shorten(url) }, 201);
});

app.get("/:code", (c) => {
  const url = resolve(c.req.param("code"));
  return url ? c.redirect(url, 302) : c.notFound();
});
"""

RATE_LIMIT = """\
import type { MiddlewareHandler } from "hono";

type Options = { limit: number; windowMs: number };

/** A fixed window per client address, kept in memory. */
export function rateLimit({ limit, windowMs }: Options): MiddlewareHandler {
  const hits = new Map<string, { count: number; resetAt: number }>();

  return async (c, next) => {
    const key = c.req.header("x-forwarded-for") ?? "local";
    const now = Date.now();
    let entry = hits.get(key);
    if (!entry || entry.resetAt <= now) {
      entry = { count: 0, resetAt: now + windowMs };
      hits.set(key, entry);
    }
    entry.count++;

    c.header("RateLimit-Limit", String(limit));
    c.header("RateLimit-Remaining", String(Math.max(0, limit - entry.count)));
    if (entry.count > limit) {
      c.header("Retry-After", String(Math.ceil((entry.resetAt - now) / 1000)));
      return c.json({ error: "too many requests" }, 429);
    }
    await next();
  };
}
"""

SERVER_TEST = """\
import { describe, expect, it } from "vitest";
import { app } from "../src/server";

describe("POST /shorten", () => {
  it("returns a code for a valid url", async () => {
    const res = await app.request("/shorten", {
      method: "POST",
      body: JSON.stringify({ url: "https://example.com" }),
    });
    expect(res.status).toBe(201);
  });

  it("rejects a missing url", async () => {
    const res = await app.request("/shorten", { method: "POST", body: "{}" });
    expect(res.status).toBe(400);
  });
});
"""

RATE_LIMIT_TEST = """\
import { Hono } from "hono";
import { describe, expect, it } from "vitest";
import { rateLimit } from "../src/rateLimit";

function appWith(limit: number) {
  const app = new Hono();
  app.get("/", rateLimit({ limit, windowMs: 60_000 }), (c) => c.text("ok"));
  return app;
}

describe("rateLimit", () => {
  it("lets requests through up to the limit", async () => {
    const app = appWith(2);
    expect((await app.request("/")).status).toBe(200);
    expect((await app.request("/")).status).toBe(200);
  });

  it("answers 429 with Retry-After past the limit", async () => {
    const app = appWith(1);
    await app.request("/");
    const res = await app.request("/");
    expect(res.status).toBe(429);
    expect(res.headers.get("Retry-After")).toBe("60");
  });

  it("counts each client on its own", async () => {
    const app = appWith(1);
    const from = (ip: string) => ({ headers: { "x-forwarded-for": ip } });
    await app.request("/", from("10.0.0.1"));
    expect((await app.request("/", from("10.0.0.2"))).status).toBe(200);
  });
});
"""

README_V2 = README + """
`POST /shorten` allows 20 requests a minute per client and answers `429` with
a `Retry-After` header past that.
"""

TEST_OUTPUT = """\

 RUN  v2.1.4 /brave-otter

 ✓ test/rateLimit.test.ts (3 tests) 12ms
 ✓ test/server.test.ts (2 tests) 9ms

 Test Files  2 passed (2)
      Tests  5 passed (5)
   Start at  14:02:31
   Duration  412ms
"""

# The history, oldest first: (message, files) with files as path -> text.
COMMITS = [
    ("init", {"README.md": README, ".gitignore": GITIGNORE, "package.json": PACKAGE}),
    ("feat: store short links in memory", {"src/store.ts": STORE}),
    ("feat: add the shorten and redirect routes", {"src/server.ts": SERVER_V1}),
    ("test: cover POST /shorten", {"test/server.test.ts": SERVER_TEST}),
]

# What the session changed, left uncommitted: path -> (before or None, after).
EDITS = {
    "src/rateLimit.ts": (None, RATE_LIMIT),
    "src/server.ts": (SERVER_V1, SERVER_V2),
    "test/rateLimit.test.ts": (None, RATE_LIMIT_TEST),
    "README.md": (README, README_V2),
}

PROMPT = (
    "Add rate limiting to POST /shorten: 20 requests a minute per client, "
    "429 with Retry-After past that. Cover it with tests."
)

SUMMARY = """\
Done. `POST /shorten` now allows 20 requests a minute per client.

- **`src/rateLimit.ts`**: a small Hono middleware with a fixed window per \
client address. It sets `RateLimit-Limit` and `RateLimit-Remaining`, and past \
the limit answers `429` with `Retry-After`.
- **`src/server.ts`**: the route uses it; redirects stay unlimited.
- **`test/rateLimit.test.ts`**: the limit, the `429` and that clients count \
apart. All 5 tests pass.

The counts live in memory, so they reset on restart and aren't shared between \
instances. If you run more than one, a store like Redis would be the next step."""


def git(project, *args, when=None):
    env = dict(os.environ)
    env.update(
        GIT_AUTHOR_NAME=AUTHOR[0],
        GIT_AUTHOR_EMAIL=AUTHOR[1],
        GIT_COMMITTER_NAME=AUTHOR[0],
        GIT_COMMITTER_EMAIL=AUTHOR[1],
    )
    if when is not None:
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = f"@{when} +0000"
    subprocess.run(["git", "-C", str(project), *args], check=True, env=env,
                   stdout=subprocess.DEVNULL)


def write(project, path, text):
    file = project / path
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_text(text)


def lines(text):
    return text.count("\n")


def changed(before, after):
    """Lines added and removed, as a diff counts them."""
    if before is None:
        return lines(after), 0
    a, b = before.splitlines(), after.splitlines()
    # Enough for these edits: they only insert or replace whole lines.
    import difflib
    add = rem = 0
    for op, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b).get_opcodes():
        if op in ("replace", "delete"):
            rem += i2 - i1
        if op in ("replace", "insert"):
            add += j2 - j1
    return add, rem


def make_repo(project, now):
    git(project, "init", "-q", "-b", "main")
    # A few days of history, the last commit an hour ago.
    for i, (message, files) in enumerate(COMMITS):
        for path, text in files.items():
            write(project, path, text)
        git(project, "add", "-A")
        when = now - 3600 - (len(COMMITS) - 1 - i) * 86400
        git(project, "commit", "-qm", message, when=when)
    for path, (_, after) in EDITS.items():
        write(project, path, after)


def transcript(project, now):
    cwd = str(project)
    tid = iter(f"toolu_{n:02d}" for n in range(1, 100))

    def tool(kind, title, name, **extra):
        call = {
            "id": next(tid), "title": title, "kind": kind, "status": "completed",
            "content": [], "locations": [], "raw_input": None, "raw_output": None,
            "added": 0, "removed": 0, "tool_name": name,
        }
        call.update(extra)
        return {"type": "tool", **call}

    def read(path):
        return tool("read", f"Read {path}", "Read", locations=[f"{cwd}/{path}"])

    def edit(path):
        before, after = EDITS[path]
        add, rem = changed(before, after)
        name = "Write" if before is None else "Edit"
        return tool(
            "edit", f"{name} {path}", name,
            content=[{"type": "diff", "path": f"{cwd}/{path}",
                      "old_text": before, "new_text": after}],
            locations=[f"{cwd}/{path}"], added=add, removed=rem,
        )

    def plan(done):
        steps = [
            "Read the routes and the existing tests",
            "Write a rate limit middleware",
            "Use it on POST /shorten",
            "Test the limit, the 429 and per-client counts",
            "Run the tests",
        ]
        return {"type": "plan", "entries": [
            {"content": s, "priority": "medium",
             "status": "completed" if i < done else
                       "in_progress" if i == done else "pending"}
            for i, s in enumerate(steps)
        ]}

    entries = [
        {"type": "user", "text": PROMPT, "at": now - 240, "elapsed_ms": 94_000,
         "tokens": 48_210},
        {"type": "thought", "text": "Hono app, in-memory store. A middleware on "
         "the one route keeps redirects free. Fixed window per address is "
         "enough here; note the multi-instance caveat."},
        plan(0),
        read("src/server.ts"),
        read("src/store.ts"),
        read("test/server.test.ts"),
        tool("search", "grep \"hono\" package.json", "Grep",
             raw_output="\"hono\": \"^4.6.0\""),
        {"type": "agent", "text": "The app is a single Hono router with no "
         "middleware yet. I'll add a small fixed-window limiter keyed on the "
         "client address and put it on `POST /shorten` only."},
        plan(1),
        edit("src/rateLimit.ts"),
        plan(2),
        edit("src/server.ts"),
        plan(3),
        edit("test/rateLimit.test.ts"),
        edit("README.md"),
        plan(4),
        tool("execute", "npm test", "Bash",
             raw_input={"command": "npm test", "description": "Run the tests"},
             terminal_output=TEST_OUTPUT, exit_code=0),
        plan(5),
        {"type": "agent", "text": SUMMARY},
    ]
    usage = {
        "context_used": 48_210, "context_size": 200_000, "cost": [0.41, "USD"],
        "cost_estimate": True, "input_tokens": 41_870, "output_tokens": 6_340,
        "cache_read_tokens": 126_400, "cache_write_tokens": 18_900,
        "thinking_tokens": 1_210, "model": "claude-opus-5-5",
    }
    return {
        "version": 1,
        "meta": {"agent": "claude", "session_id": None,
                 "title": "Rate limit POST /shorten", "cwd": cwd,
                 "created": now - 240, "updated": now - 146},
        "entries": entries,
        "usage": usage,
    }


def state(project, sid, t):
    path = str(project)
    add = sum(e.get("added", 0) for e in t["entries"])
    rem = sum(e.get("removed", 0) for e in t["entries"])
    session = {"id": sid, "title": t["meta"]["title"], "agent": "claude",
               "updated": t["meta"]["updated"], "added": add, "removed": rem,
               "meta": t["meta"], "settled": True}
    return {
        "projects": [{
            "path": path, "logo": 4906624, "art": 0, "setup": None, "notes": [],
            "monos": [], "automations": [], "active_env": path,
            "envs": [{"path": path, "branch": "main", "is_main": True,
                      "agent": "claude", "settings": [], "sessions": [session],
                      "open": [sid], "active_tab": 0}],
        }],
        "active_project": 0, "dock_open": False, "dock_height": 200,
        "rail_open": True, "panel_open": True, "arcade": False,
        "last_agent": "claude",
        # As a new install starts: sessions in the sidebar tree (every option
        # left out is Focused's), at the width the tree is drawn for.
        "tabs": {}, "rail_w": 248,
    }


# Answers the handshake a restored session makes, then stays idle, so the
# session reads as finished. No model is behind it.
MOCK_ACP = """\
#!/usr/bin/env python3
import json, sys

for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if "id" not in msg or "method" not in msg:
        continue
    method = msg["method"]
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"loadSession": False}}
    elif method == "session/new":
        result = {"sessionId": "mock"}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
"""


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__.strip().splitlines()[2].strip())
    project, seed = Path(sys.argv[1]).resolve(), Path(sys.argv[2])
    if any(project.iterdir()):
        sys.exit(f"product-shots-seed: {project} is not empty")
    now = int(time.time())
    make_repo(project, now)
    t = transcript(project, now)
    # The app names sessions by the time they started, in hex millis.
    sid = f"{(now - 240) * 1000:x}-0"
    sessions = seed / "data" / "arakun" / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    (sessions / f"{sid}.json").write_text(json.dumps(t))
    (seed / "state.json").write_text(json.dumps(state(project, sid, t)))
    acp = seed / "mock-acp.py"
    acp.write_text(MOCK_ACP)
    acp.chmod(0o755)


if __name__ == "__main__":
    main()
