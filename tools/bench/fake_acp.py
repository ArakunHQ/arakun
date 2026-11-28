#!/usr/bin/env python3
"""An ACP agent for the perf suite. Costs nothing: it calls no model.

What it does on a prompt depends on a word in the prompt:

- no word (the `working` scenario): streams a thought and starts a tool call,
  then writes a reply a word at a time and holds the turn open, so the session
  shows its looping motion (the beacon, the spinner, the shimmer) and fades
  streamed text in for as long as the scenario watches.
- `bench-edits` (`transcript`): first a long transcript of finished edits, each
  with a diff of a whole file, then, once the project has a `.bench-stream`
  file, the same open, streaming reply.
- `bench-ask` (`permission`): the same edits, then an edit of a large file that
  asks for permission and waits, with no reply streaming.
- `bench-done` (the `large` startup's seed): the edits and a long reply, then
  the turn ends, so the session can be saved and copied.
"""

import json
import os
import sys
import threading

LOCK = threading.Lock()
WORDS = ("The tests fail because the parser drops the last line of a file that "
         "has no newline at its end, so the fix reads the rest of the buffer too. ").split()
# Edits in the long transcript, and the lines of the file each one rewrites.
EDITS = 40
LINES = 600
# The file the permission card shows: the diff a card draws gets slower with it.
ASK_LINES = 3000
# In the project: `bench-edits` streams its reply once this file is there.
GO = ".bench-stream"


def send(msg):
    with LOCK:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def update(sid, u):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": u}})


def text(sid, kind, s):
    update(sid, {"sessionUpdate": kind, "content": {"type": "text", "text": s}})


def source(n, lines, edited):
    """A Rust file of `lines` lines; `edited` changes every 25th, as a real
    edit spread over a file would."""
    out = []
    for i in range(lines):
        body = f"x.wrapping_mul({i + n}) ^ 0x{i:04x}"
        if edited and i % 25 == 7:
            body = f"x.rotate_left({i % 31}).wrapping_add({n})"
        out.append(f"fn item_{n}_{i}(x: u64) -> u64 {{ {body} }}")
    return "\n".join(out) + "\n"


def edit(sid, i, lines, status, cwd):
    path = f"{cwd}/src/module_{i:02}.rs"
    call = {"sessionUpdate": "tool_call", "toolCallId": f"edit-{i}", "title": f"Edit src/module_{i:02}.rs",
            "kind": "edit", "status": status, "locations": [{"path": path}],
            "content": [{"type": "diff", "path": path, "oldText": source(i, lines, False),
                         "newText": source(i, lines, True)}]}
    update(sid, call)
    return call


def edits(sid, cwd):
    text(sid, "agent_thought_chunk", "Each module needs the same fix; going through them in turn.")
    for i in range(EDITS):
        text(sid, "agent_message_chunk", f"Now module {i}. ")
        edit(sid, i, LINES, "completed", cwd)


def write(sid, stop, go=None):
    """About 20 words a second, as a model writes, until the turn is cancelled.
    With `go`, a path, it starts once that file exists (or after 30 s)."""
    if go:
        for _ in range(300):
            if os.path.exists(go) or stop.wait(0.1):
                break
        try:
            os.remove(go)
        except OSError:
            pass
    i = 0
    while not stop.wait(0.05):
        text(sid, "agent_message_chunk", WORDS[i % len(WORDS)] + " ")
        i += 1


def main():
    # The open turn: its request ID, the event that stops its writer, the writer.
    turn = None
    cwd = "."
    asks = 0
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        method, mid = msg.get("method"), msg.get("id")
        if method == "session/cancel":
            # A notification, so it has no ID: end the open turn as cancelled.
            if turn:
                turn[1].set()
                if turn[2]:
                    turn[2].join()  # no chunk after the turn's end
                send({"jsonrpc": "2.0", "id": turn[0], "result": {"stopReason": "cancelled"}})
                turn = None
            continue
        if mid is None or method is None:
            continue  # a notification, or the app's answer to a permission request
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})
        elif method == "session/new":
            cwd = msg.get("params", {}).get("cwd") or cwd
            send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "bench"}})
        elif method == "session/prompt":
            sid = msg["params"]["sessionId"]
            prompt = " ".join(b.get("text", "") for b in msg["params"].get("prompt", []) if isinstance(b, dict))
            stop, writer = threading.Event(), None
            if "bench-done" in prompt:
                edits(sid, cwd)
                for i in range(2000):
                    text(sid, "agent_message_chunk", WORDS[i % len(WORDS)] + " ")
                send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
                continue
            if "bench-ask" in prompt:
                edits(sid, cwd)
                call = edit(sid, EDITS, ASK_LINES, "pending", cwd)
                asks += 1
                del call["sessionUpdate"]
                send({"jsonrpc": "2.0", "id": f"ask-{asks}", "method": "session/request_permission", "params": {
                    "sessionId": sid, "toolCall": call, "options": [
                        {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}})
            else:
                go = None
                if "bench-edits" in prompt:
                    edits(sid, cwd)
                    # The bench expands the last edits first, then makes this
                    # file, so the reply doesn't push them away meanwhile.
                    go = os.path.join(cwd, GO)
                else:
                    text(sid, "agent_thought_chunk", "Thinking about the change…")
                    update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run the tests",
                                 "kind": "execute", "status": "in_progress"})
                writer = threading.Thread(target=write, args=(sid, stop, go), daemon=True)
                writer.start()
            # No reply: the turn stays open until the app cancels it or stops.
            turn = (mid, stop, writer)
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {}})


if __name__ == "__main__":
    main()
