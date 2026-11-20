import { randomBytes } from "node:crypto";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { convertV4MiniflareOptions, Miniflare } from "miniflare";

export const privateToken = () => randomBytes(32).toString("hex");
export async function localControl(state, token, offlineBox) {
  const compiled = await build({
    entryPoints: [fileURLToPath(new URL("../src/index.ts", import.meta.url))],
    bundle: true, external: ["cloudflare:workers"], format: "esm", write: false,
  });
  return new Miniflare(convertV4MiniflareOptions({
    host: "127.0.0.1", port: 0, cf: false, compatibilityDate: "2026-11-01",
    modules: true, script: compiled.outputFiles[0].text, bindings: { BETA_TOKEN: token },
    resourcePersistencePath: resolve(state, "control"),
    durableObjects: { BETA: { className: "BudgetMachine", useSQLite: true } },
    r2Buckets: ["WORKSPACE_R2"], serviceBindings: { OFFLINE_BOX: offlineBox },
    outboundService: () => new Response(null, { status: 403 }),
  }));
}

export function ui(mf, token, event, body) {
  return mf.dispatchFetch(`http://ui/workspaces/fixture${event ? "/" + event : ""}`, {
    method: event ? "POST" : "GET", body,
    headers: { authorization: `Bearer ${token}`, ...(event === "start" && { "x-budget-seconds": "30" }) },
  });
}
