// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The community widget index, reports and install counts, and the operator's
// routes that list, sign and pull widgets.
import { env } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";
import { ADMIN, call } from "./helpers";

const JSON_HEADERS = { "Content-Type": "application/json" };
const ADMIN_JSON = { ...ADMIN, ...JSON_HEADERS };
const COMMIT = "9f2e1c4".padEnd(40, "0");
const HASH = "a".repeat(64);

function listing(extra: Record<string, unknown> = {}) {
  return {
    name: "Deploy board",
    about: "Vercel deploys and their checks, live",
    license: "MIT",
    repository: "https://github.com/mrvn/deploy-board",
    commit: COMMIT,
    hash: HASH,
    files: { "widget.toml": "[widget]", "main.rhai": "fn view(s, z) {}", "sample.json": "{}" },
    ...extra,
  };
}

async function list(id: string, version: string, extra: Record<string, unknown> = {}): Promise<Response> {
  return call("PUT", `/admin/widgets/${id}/${version}`, ADMIN_JSON, JSON.stringify(listing(extra)));
}

interface Index {
  widgets: {
    id: string;
    author: string;
    installs: number;
    pulled: string | null;
    versions: { version: string; signature: string | null; pulled: string | null }[];
    files: Record<string, string> | null;
  }[];
}

async function index(): Promise<Index> {
  const res = await call("GET", "/widgets");
  expect(res.status).toBe(200);
  return (await res.json()) as Index;
}

function from(ip: string) {
  return { ...JSON_HEADERS, "CF-Connecting-IP": ip };
}

beforeEach(async () => {
  await env.DB.batch(["widget_versions", "widgets", "widget_reports", "admin_log"].map((t) => env.DB.prepare(`DELETE FROM ${t}`)));
});

describe("widget index", () => {
  it("lists a version with its files, and a signed one carries its signature", async () => {
    expect((await list("mrvn/deploy-board", "0.4.0")).status).toBe(200);
    const signed = await list("mrvn/deploy-board", "0.4.1", { signature: "untrusted comment: sig\nRUQ..." });
    expect(await signed.json()).toEqual({ id: "mrvn/deploy-board", version: "0.4.1", verified: true });
    const [w] = (await index()).widgets;
    expect(w!.id).toBe("mrvn/deploy-board");
    expect(w!.author).toBe("mrvn");
    expect(w!.versions.map((v) => v.version)).toEqual(["0.4.1", "0.4.0"]);
    expect(w!.versions[0]!.signature).toContain("RUQ");
    expect(w!.files).toEqual({ version: "0.4.1", "widget.toml": "[widget]", "main.rhai": "fn view(s, z) {}", "sample.json": "{}" });
  });

  it("a pulled version stays listed with its reason and gives up its files", async () => {
    await list("mrvn/deploy-board", "0.4.0");
    await list("mrvn/deploy-board", "0.4.1");
    const pulled = await call("PUT", "/admin/widgets/mrvn/deploy-board/0.4.1/pull", ADMIN_JSON, JSON.stringify({ reason: "sends tokens elsewhere" }));
    expect(pulled.status).toBe(204);
    let [w] = (await index()).widgets;
    expect(w!.versions[0]).toMatchObject({ version: "0.4.1", pulled: "sends tokens elsewhere" });
    expect(w!.files?.version).toBe("0.4.0");
    // The whole widget pulled: no version to show.
    await call("PUT", "/admin/widgets/mrvn/deploy-board/pull", ADMIN_JSON, JSON.stringify({ reason: "malware" }));
    [w] = (await index()).widgets;
    expect(w!.pulled).toBe("malware");
    expect(w!.files).toBeNull();
    expect((await call("DELETE", "/admin/widgets/mrvn/deploy-board/pull", ADMIN)).status).toBe(204);
    [w] = (await index()).widgets;
    expect(w!.pulled).toBeNull();
  });

  it("refuses a listing that is not a widget", async () => {
    expect((await list("Mrvn/deploy-board", "0.4.1")).status).toBe(400);
    expect((await list("mrvn/deploy-board", "v0.4.1")).status).toBe(400);
    expect((await list("mrvn/deploy-board", "0.4.1", { repository: "https://evil.example/x/y" })).status).toBe(400);
    expect((await list("mrvn/deploy-board", "0.4.1", { commit: "main" })).status).toBe(400);
    expect((await list("mrvn/deploy-board", "0.4.1", { files: { "widget.toml": "" } })).status).toBe(400);
    expect((await call("PUT", "/admin/widgets/mrvn/deploy-board/0.4.1", JSON_HEADERS, JSON.stringify(listing()))).status).toBe(401);
    expect((await index()).widgets).toEqual([]);
  });

  it("a report waits for a person, and closing it is logged", async () => {
    await list("mrvn/deploy-board", "0.4.1");
    const sent = await call("POST", "/widgets/report", from("10.1.0.1"), JSON.stringify({ id: "mrvn/deploy-board", version: "0.4.1", reason: "misleading" }));
    expect(sent.status).toBe(202);
    const bad = await call("POST", "/widgets/report", from("10.1.0.1"), JSON.stringify({ id: "mrvn/deploy-board", version: "0.4.1", reason: "spite" }));
    expect(bad.status).toBe(400);
    const unknown = await call("POST", "/widgets/report", from("10.1.0.1"), JSON.stringify({ id: "mrvn/other", version: "0.4.1", reason: "broken" }));
    expect(unknown.status).toBe(404);
    const res = await call("GET", "/admin/widgets/reports", ADMIN);
    const { reports } = (await res.json()) as { reports: { id: number; widget_id: string; reason: string; closed_at: number | null }[] };
    expect(reports).toHaveLength(1);
    expect(reports[0]).toMatchObject({ widget_id: "mrvn/deploy-board", reason: "misleading", closed_at: null });
    expect((await call("PUT", `/admin/widgets/reports/${reports[0]!.id}/close`, ADMIN)).status).toBe(204);
    expect((await call("PUT", `/admin/widgets/reports/${reports[0]!.id}/close`, ADMIN)).status).toBe(404);
    const log = await env.DB.prepare("SELECT action FROM admin_log ORDER BY id").all<{ action: string }>();
    expect(log.results.map((r) => r.action)).toEqual(["widget.list", "widget.report.close"]);
  });

  it("counts an install of a listed widget only", async () => {
    await list("mrvn/deploy-board", "0.4.1");
    expect((await call("POST", "/widgets/installed", from("10.1.0.2"), JSON.stringify({ id: "mrvn/deploy-board" }))).status).toBe(204);
    expect((await call("POST", "/widgets/installed", from("10.1.0.2"), JSON.stringify({ id: "mrvn/nope" }))).status).toBe(404);
    expect((await index()).widgets[0]!.installs).toBe(1);
  });

  it("an unknown /widgets path is a JSON 404, not the page", async () => {
    const res = await call("GET", "/widgets/nothing");
    expect(res.status).toBe(404);
    expect(res.headers.get("Content-Type")).toContain("application/json");
  });
});
