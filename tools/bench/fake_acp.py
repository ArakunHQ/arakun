#!/usr/bin/env python3
"""An ACP agent that never finishes its turn, for the perf suite's `working`
scenario. On a prompt it streams a thought and starts a tool call, then
writes a reply a word at a time and holds the turn open, so the session shows
its looping motion (the beacon, the spinner, the shimmer) and fades streamed
text in for as long as the scenario watches. Costs nothing: it calls no model.
"""

import json
import sys
import threading
import time

LOCK = threading.Lock()
WORDS = ("The tests fail because the parser drops the last line of a file that "
         "has no newline at its end, so the fix reads the rest of the buffer too. ").split()


def send(msg):
    with LOCK:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def update(sid, u):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": u}})


def write(sid):
    """About 20 words a second, as a model writes."""
    i = 0
    while True:
        time.sleep(0.05)
        update(sid, {"sessionUpdate": "agent_message_chunk",
                     "content": {"type": "text", "text": WORDS[i % len(WORDS)] + " "}})
        i += 1


def main():
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        method, mid = msg.get("method"), msg.get("id")
        if mid is None or method is None:
            continue
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})
        elif method == "session/new":
            send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "bench"}})
        elif method == "session/prompt":
            sid = msg["params"]["sessionId"]
            update(sid, {"sessionUpdate": "agent_thought_chunk",
                         "content": {"type": "text", "text": "Thinking about the change…"}})
            update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run the tests",
                         "kind": "execute", "status": "in_progress"})
            threading.Thread(target=write, args=(sid,), daemon=True).start()
            # No reply: the turn stays open until the app stops.
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {}})


if __name__ == "__main__":
    main()
