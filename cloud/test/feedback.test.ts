// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Feedback from the app, filed in Linear as customer requests.
import { createExecutionContext, env, waitOnExecutionContext } from "cloudflare:test";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { parse } from "../src/feedback";
import worker from "../src/index";

/// What the fake Linear was asked, one GraphQL body per call.
let calls: { query: string; variables: Record<string, any> }[] = [];
let keys: (string | null)[] = [];
/// The labels the fake Linear knows: the team's, the workspace's and another team's.
const LABELS = [
  { id: "l-bug", name: "Bug", team: null },
  { id: "l-feature", name: "Feature", team: null },
  { id: "l-improvement", name: "Improvement", team: { id: "team-arakun" } },
  { id: "l-area-app", name: "area: app", team: { id: "team-arakun" } },
  { id: "l-area-other", name: "area: app", team: { id: "team-website" } },
];
let outage: number | null = null;

beforeEach(() => {
  calls = [];
  keys = [];
  outage = null;
  const fallback = globalThis.fetch;
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    if (req.url !== "https://api.linear.app/graphql") return fallback(input, init);
    keys.push(req.headers.get("Authorization"));
    const body = (await req.json()) as { query: string; variables: Record<string, any> };
    calls.push(body);
    if (outage !== null) return Response.json({ errors: [{ message: "down" }] }, { status: outage });
    if (body.query.includes("issueLabels")) {
      const names: string[] = body.variables.names;
      return Response.json({ data: { issueLabels: { nodes: LABELS.filter((l) => names.includes(l.name)) } } });
    }
    if (body.query.includes("issueCreate")) return Response.json({ data: { issueCreate: { success: true, issue: { id: "issue-1" } } } });
    if (body.query.includes("customerUpsert")) return Response.json({ data: { customerUpsert: { success: true, customer: { id: "cust-1" } } } });
    if (body.query.includes("customerNeedCreate")) return Response.json({ data: { customerNeedCreate: { success: true } } });
    return Response.json({ errors: [{ message: "unknown query" }] }, { status: 400 });
  });
});

const GOOD = {
  kind: "bug",
  title: "The tab name resets",
  description: "Bug · user feedback · Arakun 0.22.0 · linux\n\nWhat's on your mind?\nThe tab name resets.",
  request: "Bug · user feedback · Arakun 0.22.0 · linux\n\nWhat's on your mind?\nThe tab name resets in /home/ann/x.",
  from: "Arakun user",
  email: "Ann@Example.com",
};

/// Posts feedback as one client IP and waits for the work after the answer.
async function send(body: unknown, ip: string, bindings: Partial<typeof env> = {}): Promise<Response> {
  const ctx = createExecutionContext();
  const req = new Request("https://teams.test/feedback", {
    method: "POST",
    headers: { "Content-Type": "application/json", "CF-Connecting-IP": ip },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
  const res = await worker.fetch(req, { ...env, ...bindings }, ctx);
  await waitOnExecutionContext(ctx);
  return res;
}

describe("feedback to Linear", () => {
  it("opens an issue with the scrubbed text and a customer request with the rest", async () => {
    const res = await send(GOOD, "10.0.0.1");
    expect(res.status).toBe(202);
    expect(keys.every((k) => k === "lin_api_test")).toBe(true);
    const [labels, issue, customer, need] = calls;
    expect(labels!.variables.names).toEqual(["Bug", "area: app"]);
    // The other team's label of the same name is not used.
    expect(issue!.variables.input).toEqual({
      teamId: "team-arakun",
      title: "The tab name resets",
      description: `${GOOD.description}\n\nFrom: Arakun user`,
      labelIds: ["l-bug", "l-area-app"],
    });
    expect(JSON.stringify(issue)).not.toContain("example.com");
    expect(JSON.stringify(issue)).not.toContain("/home/ann");
    expect(customer!.variables.input).toEqual({ externalId: "ann@example.com", name: "ann@example.com" });
    expect(need!.variables.input).toEqual({ issueId: "issue-1", body: GOOD.request, customerId: "cust-1" });
    expect(calls).toHaveLength(4);
  });

  it("files feedback without an email with no customer", async () => {
    expect((await send({ ...GOOD, kind: "ux", email: null }, "10.0.0.2")).status).toBe(202);
    expect(calls.map((c) => c.query.match(/(issueLabels|issueCreate|customerUpsert|customerNeedCreate)/)![1])).toEqual([
      "issueLabels",
      "issueCreate",
      "customerNeedCreate",
    ]);
    expect(calls[1]!.variables.input.labelIds).toEqual(["l-improvement", "l-area-app"]);
    expect(calls[2]!.variables.input).toEqual({ issueId: "issue-1", body: GOOD.request });
  });

  it("gives other no type label", async () => {
    expect((await send({ ...GOOD, kind: "other" }, "10.0.0.3")).status).toBe(202);
    expect(calls[0]!.variables.names).toEqual(["area: app"]);
  });

  it("answers before Linear and survives it being down", async () => {
    outage = 500;
    const res = await send(GOOD, "10.0.0.4");
    expect(res.status).toBe(202);
    expect(calls).toHaveLength(1);
  });

  it("refuses bad feedback and support questions", async () => {
    expect((await send({ ...GOOD, kind: "support" }, "10.0.0.5")).status).toBe(400);
    expect((await send("not json", "10.0.0.5")).status).toBe(400);
    expect((await send({ ...GOOD, title: " " }, "10.0.0.5")).status).toBe(400);
    expect(calls).toHaveLength(0);
  });

  it("is off without a key", async () => {
    expect((await send(GOOD, "10.0.0.6", { LINEAR_API_KEY: undefined })).status).toBe(503);
    expect(calls).toHaveLength(0);
  });

  it("takes a few a minute from one IP", async () => {
    const answers = [];
    for (let i = 0; i < 6; i++) answers.push((await send({ ...GOOD, email: null }, "10.0.0.7")).status);
    expect(answers.slice(0, 5)).toEqual([202, 202, 202, 202, 202]);
    expect(answers[5]).toBe(429);
  });

  it("only takes POST", async () => {
    const ctx = createExecutionContext();
    const res = await worker.fetch(new Request("https://teams.test/feedback"), env, ctx);
    expect(res.status).toBe(404);
  });
});

describe("parse", () => {
  it("drops a follow-up answer that is no email and keeps titles on one line", () => {
    const f = parse({ ...GOOD, email: "later", title: "two\nlines" });
    expect(f).toMatchObject({ email: null, title: "two lines" });
  });

  it("checks lengths", () => {
    expect(typeof parse({ ...GOOD, title: "x".repeat(201) })).toBe("string");
    expect(typeof parse({ ...GOOD, request: "" })).toBe("string");
    expect(typeof parse({ ...GOOD, from: 5 })).toBe("string");
    expect(typeof parse({ ...GOOD, email: 5 })).toBe("string");
  });
});
