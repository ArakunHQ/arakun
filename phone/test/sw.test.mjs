// phone/public/sw.js: a push for the session a visible window is looking at
// shows no notification, at the new address (`#/s/<id>`) and the old one
// (`#s=<id>`). Run with `node --test phone/test/`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "..", "public", "sw.js"), "utf8");

/// The worker with `windows` open; gives back how many notifications a push
/// for `session` showed.
async function push(windows, session, extra = {}, all = false) {
  const on = {};
  const shown = [];
  const self = {
    addEventListener: (type, fn) => (on[type] = fn),
    skipWaiting() {},
    clients: { matchAll: async () => windows, claim: async () => {} },
    registration: { showNotification: async (title, opts) => shown.push({ title, opts }) },
  };
  vm.runInNewContext(source, { self }, { filename: "sw.js" });
  let done;
  on.push({ data: { json: () => ({ session, title: "t", body: "b", ...extra }) }, waitUntil: (p) => (done = p) });
  await done;
  return all ? shown : shown.length;
}

const win = (url, visibilityState = "visible") => ({ url, visibilityState });

test("no notification for the session a window shows at the new address", async () => {
  assert.equal(await push([win("https://h/#/s/abc")], "abc"), 0);
});

test("no notification for the session a window shows at the old address", async () => {
  assert.equal(await push([win("https://h/#s=abc")], "abc"), 0);
});

test("no notification for the session a window shows under a sheet", async () => {
  assert.equal(await push([win("https://h/#/s/abc?sheet=changes")], "abc"), 0);
});

test("a notification when the window shows another screen", async () => {
  assert.equal(await push([win("https://h/#/inbox")], "abc"), 1);
});

test("a notification when the window shows another session that starts the same", async () => {
  assert.equal(await push([win("https://h/#/s/abcd")], "abc"), 1);
  assert.equal(await push([win("https://h/#s=abcd")], "abc"), 1);
});

test("a notification when the window is hidden", async () => {
  assert.equal(await push([win("https://h/#/s/abc", "hidden")], "abc"), 1);
});

test("the body starts with the environment and turn length on its own line", async () => {
  const [n] = await push([], "abc", { env: "app", secs: 252 }, true);
  assert.equal(n.opts.body, "app \u00b7 4m 12s\nb");
  const [short] = await push([], "abc", { env: "app", secs: 42 }, true);
  assert.equal(short.opts.body, "app \u00b7 42s\nb");
});

test("the environment alone, and no subtitle without one", async () => {
  const [a] = await push([], "abc", { env: "app" }, true);
  assert.equal(a.opts.body, "app\nb");
  const [b] = await push([], "abc", { secs: 90 }, true);
  assert.equal(b.opts.body, "b");
  const [old] = await push([], "abc", {}, true);
  assert.equal(old.opts.body, "b");
});
