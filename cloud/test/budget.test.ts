import { test } from "node:test";
import assert from "node:assert/strict";
import { HOUR, MONTH_LIMIT, reservation, STEP_LIMIT, TASK_LIMIT, transition } from "../src/budget.ts";

test("a lease cannot spend beyond the remaining task, month or UTC month boundary", () => {
  const now = Date.UTC(2026, 10, 15);
  assert.equal(reservation(now, 0, 0), STEP_LIMIT);
  assert.equal(reservation(now, MONTH_LIMIT - 30_000, 0), 30_000);
  assert.equal(reservation(now, 0, TASK_LIMIT - 12_000), 12_000);
  assert.equal(reservation(now, MONTH_LIMIT + HOUR, 0), 0);
  assert.equal(reservation(Date.UTC(2026, 11, 1) - 7000, 0, 0), 7000);
});

test("no CI success, duplicate, or human acknowledgement starts compute", () => {
  assert.equal(transition("waiting_ci", "ci-green"), "done");
  assert.equal(transition("needs_decision", "ack"), "done");
  for (const phase of ["running", "waiting_ci", "needs_decision", "done", "interrupted"] as const)
    assert.equal(transition(phase, "start"), "unchanged");
  assert.equal(transition("running", "ci-red"), "conflict");
  assert.equal(transition("needs_decision", "ci-red"), "unchanged");
  assert.equal(transition("new", "ci-red"), "conflict");
  assert.equal(transition("waiting_ci", "ci-red"), "test");
  assert.equal(transition("interrupted", "retry", true), "test");
  assert.equal(transition("interrupted", "retry", false), "review");
  assert.equal(transition("waiting_ci", "retry", true), "conflict");
});
