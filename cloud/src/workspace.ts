// The fixture has one immutable Git base and exactly two allowed dirty files.
// No executor-supplied path, bucket key, free-form text or archive is accepted.
export const BASE = "0ec59da6152c203b4da9fa3dea299700239fa56a";
const prefix = "fixture/v1/";
export const MAX_BYTES = 512;
export const WORKSPACE_LIMIT_MS = 30_000;
const enc = new TextEncoder();
const check = (ok: unknown) => { if (!ok) throw new Error("invalid workspace checkpoint"); };

export async function bounded(body: ReadableStream<Uint8Array> | null) {
  if (!body) throw new Error("missing body");
  const reader = body.getReader(), buf = new Uint8Array(MAX_BYTES);
  let offset = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (offset + value.length > buf.length) {
      await reader.cancel();
      throw new Error("oversize body");
    }
    buf.set(value, offset);
    offset += value.length;
  }
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(buf.subarray(0, offset));
}

export function validate(raw: string) {
  check(enc.encode(raw).length <= MAX_BYTES);
  const { v, base, files } = JSON.parse(raw);
  const edit = files?.["tracked.txt"], note = files?.["note.txt"];
  check(v === 1 && base === BASE &&
    typeof edit === "string" && /^base line\nedited: [0-9a-f]{32}\n$/.test(edit) &&
    typeof note === "string" && /^new: [0-9a-f]{32}\n$/.test(note));
  // Canonical bytes reject extra/duplicate keys, including secret paths.
  check(raw === JSON.stringify({ v: 1, base: BASE, files: { "tracked.txt": edit, "note.txt": note } }));
}

async function digest(raw: string) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", enc.encode(raw)))]
    .map(n => n.toString(16).padStart(2, "0")).join("");
}
type Pointer = { v: number; base: string; sha256: string; bytes: number };
async function load(bucket: R2Bucket, ref: Pointer) {
  check(ref?.v === 1 && ref.base === BASE && /^[0-9a-f]{64}$/.test(ref.sha256) &&
    Number.isInteger(ref.bytes) && ref.bytes > 0 && ref.bytes <= MAX_BYTES);
  const object = await bucket.get(prefix + ref.sha256);
  if (!object || object.size !== ref.bytes) throw new Error("missing or incomplete checkpoint");
  const raw = await bounded(object.body);
  check(enc.encode(raw).length === ref.bytes && await digest(raw) === ref.sha256);
  validate(raw);
  return raw;
}

type Exchange = (step: string, body: string) => Promise<Response>;
export async function workspaceStep(step: "capture" | "restore", prior: string | null, bucket: R2Bucket, exchange: Exchange) {
  async function run(body: string) {
    const response = await exchange(step, body);
    if (!response.ok) throw new Error("workspace executor failed");
    return bounded(response.body);
  }
  if (step === "capture") {
    check(prior === null);
    const raw = await run("");
    validate(raw);
    const sha256 = await digest(raw);
    const ref: Pointer = { v: 1, base: BASE, sha256, bytes: enc.encode(raw).length };
    await bucket.put(prefix + sha256, raw, { sha256 });
    await load(bucket, ref); // Publish the SQLite pointer only after read-back.
    return JSON.stringify(ref);
  }
  const ref: Pointer = JSON.parse(prior ?? "null");
  const raw = await load(bucket, ref); // A bad object must not boot executor B.
  const proof = JSON.parse(await run(raw));
  check(proof.before === "clean" && proof.after === " M tracked.txt\n?? note.txt\n" &&
    proof.sha256 === ref.sha256);
  return JSON.stringify({ ...ref, restored: { before: proof.before, after: proof.after, sha256: proof.sha256 } });
}
