"""Run against a local rehearsal serving the pinned #272 image, never GitHub."""
from concurrent.futures import ThreadPoolExecutor
import json
import time
import uuid

from budget import request


job = "check-" + uuid.uuid4().hex[:12]


def phase(event, expected, seconds=None):
    status, data = request(job, event, seconds)
    assert status == 200 and data["task"]["phase"] == expected, (status, data)
    return data["task"]


first = phase("start", "waiting_ci", 90)
assert first["checkpoint"]["pr"] == 272 and first["limit_ms"] == 90000
time.sleep(1)
assert phase("start", "waiting_ci") == first
with ThreadPoolExecutor() as pool:
    second = pool.submit(phase, "ci-red", "needs_decision")
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline and not request(job)[1]["computeReserved"]:
        time.sleep(.05)
    assert request(job + "-other", "start")[0] == 409
    verified = second.result()
assert verified["checkpoint"]["checks"] == "passed" and verified["runs"] == 2
assert phase("ci-red", "needs_decision") == verified
done = phase("ack", "done")
assert done["used"] == verified["used"]
status, green = request(job + "-green", "start")
assert status == 200
status, done_green = request(job + "-green", "ci-green")
assert status == 200 and done_green["task"]["used"] == green["task"]["used"] and not done_green["computeReserved"]
print(json.dumps({"job": job, "head": done["checkpoint"]["head"], "starts": done["runs"],
                  "typecheck_ms": first["used"], "test_ms": done["used"] - first["used"],
                  "combined_ms": done["used"], "idle_ms_charged": 0}))
