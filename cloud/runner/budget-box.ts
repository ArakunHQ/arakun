// An offline rehearsal on one public commit: the teams Worker as merged in
// #272. No model, GitHub token, user-supplied commands or source uploads.
const record = { pr: 272, head: "7f0cf45530f4b488d263418e647e8c62788c1c4c" };
const teams = "/app/teams";

async function step(req: Request): Promise<Response> {
  const url = new URL(req.url);
  if (req.method === "GET" && url.pathname === "/ready") return new Response("ready");
  if (req.method !== "POST") return new Response(null, { status: 405 });
  if (url.pathname === "/review") {
    const check = Bun.spawn(["node", "node_modules/typescript/bin/tsc", "--noEmit"], { cwd: teams, stdout: "ignore", stderr: "inherit" });
    if (await check.exited !== 0) return new Response(null, { status: 422 });
    return Response.json({ ...record, checks: "awaiting-ci" });
  }
  if (url.pathname === "/test") {
    const prior = await req.json() as { pr: number; head: string; checks: string };
    if (prior.pr !== record.pr || prior.head !== record.head || prior.checks !== "awaiting-ci") return new Response(null, { status: 409 });
    const child = Bun.spawn(["node", "node_modules/vitest/vitest.mjs", "run", "--maxWorkers=1"], { cwd: teams, stdout: 2, stderr: "inherit" });
    if (await child.exited !== 0) return new Response(null, { status: 422 });
    return Response.json({ ...record, checks: "passed" });
  }
  return new Response(null, { status: 404 });
}

if (process.argv[2] === "--offline") {
  const body = await Bun.stdin.text();
  const result = await step(new Request(`http://box/${process.argv[3]}`, { method: "POST", body }));
  if (!result.ok) process.exit(1);
  console.log(await result.text());
} else {
  Bun.serve({ port: 8080, fetch: step });
}

export {};
