"""Operate only the loopback rehearsal; credentials never go into argv."""
import argparse
import http.client
import json
import os
from pathlib import Path
import re
import sys


def request(task, event=None, budget_seconds=None):
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,39}", task):
        raise ValueError("invalid task name")
    state_home = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))
    state = Path(os.environ.get("BETA_STATE_DIR", state_home / "arakun-cloud-rehearsal"))
    token = (state / "beta.env").read_text().strip().removeprefix("BETA_TOKEN=")
    conn = http.client.HTTPConnection("127.0.0.1", int(os.environ.get("BETA_PORT", "18747")), timeout=135)
    try:
        path = "/tasks/" + task + ("/" + event if event else "")
        headers = {"Authorization": "Bearer " + token}
        if budget_seconds is not None:
            headers["x-budget-seconds"] = str(budget_seconds)
        conn.request("POST" if event else "GET", path, headers=headers)
        response = conn.getresponse()
        raw = response.read()
        try:
            return response.status, json.loads(raw)
        except ValueError:
            return response.status, {"error": f"local runtime returned HTTP {response.status}; check task status"}
    finally:
        conn.close()


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("task")
    cli.add_argument("event", nargs="?", choices=["start", "ci-green", "ci-red", "ack", "retry"])
    cli.add_argument("--budget-seconds", type=int)
    args = cli.parse_args()
    status, result = request(args.task, args.event, args.budget_seconds)
    print(json.dumps(result, indent=2))
    sys.exit(0 if status == 200 else 1)
