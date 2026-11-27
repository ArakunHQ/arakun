// The model credentials of a cloud machine (cloud_machine.ts): the owner's own
// API key for the agents that run there. An Anthropic API key, Amazon Bedrock
// or Google Vertex AI for Claude Code, one of the three at a time, and an
// OpenAI API key next to it.
//
// Only API keys. A plan's sign-in (claude.ai, ChatGPT) never comes through
// here: the owner signs in inside the machine, with the agent's own flow, and
// the Worker neither sees nor keeps that token.
//
// Where a key lives. The machine's object keeps it sealed: AES-GCM under
// CLOUD_MODEL_KEY, with the machine's label as additional data, so a key
// copied to another machine's object does not open. It goes into the container
// on every start and when it changes, as variables for machine/model-env,
// which writes them under /run/arakun for root alone. machine/as-agent gives
// them to each agent it starts. Sleep removes /run/arakun before the snapshot,
// and takes no snapshot when that fails, so no snapshot holds a key.
//
// Format: v1.<base64url iv>.<base64url ciphertext>, of the JSON of a `Model`.

import { b64url, fromB64url, isObject, utf8, type Env } from "./util";

/// Whose key it is. The first three are ways to run Claude Code, and a
/// machine keeps one of them.
export const PROVIDERS = ["anthropic", "bedrock", "vertex", "openai"] as const;
export type Provider = (typeof PROVIDERS)[number];
const CLAUDE: readonly Provider[] = ["anthropic", "bedrock", "vertex"];

/// Where model-env writes the Vertex credentials, and Google's libraries read
/// them (machine/model-env has the same path).
export const VERTEX_FILE = "/run/arakun/vertex.json";
/// What model-env reads: the variables for the agents, one `NAME=value` a
/// line, and the Vertex credentials file.
export const MODEL_VAR = "ARAKUN_CLOUD_MODEL";
export const VERTEX_VAR = "ARAKUN_CLOUD_VERTEX";

/// One provider's credentials as the agent needs them.
export interface Model {
  /// The agent's environment variables.
  env: Record<string, string>;
  /// Vertex only: the credentials file's JSON, on one line.
  file?: string;
}

/// What the object stores for one provider.
export interface Sealed {
  sealed: string;
  /// Unix seconds when it was set.
  at: number;
}
export type Stored = Partial<Record<Provider, Sealed>>;

export function isProvider(text: string): text is Provider {
  return (PROVIDERS as readonly string[]).includes(text);
}

/// The providers a new one replaces: the other ways to run Claude Code.
export function replacedBy(provider: Provider): Provider[] {
  return CLAUDE.includes(provider) ? CLAUDE.filter((p) => p !== provider) : [];
}

// A key or token: printable ASCII without spaces, so it fits a line of the
// variables file and nothing in it reads as a second line.
const SECRET = /^[\x21-\x7e]{8,4096}$/;
const REGION = /^[a-z0-9-]{1,32}$/;
const PROJECT = /^[a-z0-9][a-z0-9.:-]{0,62}$/;
const MAX_FILE = 16 * 1024;

function secret(v: unknown): string | null {
  return typeof v === "string" && SECRET.test(v) ? v : null;
}

/// The credentials in a request's body, or what is wrong with it.
export function parseModel(provider: Provider, body: unknown): Model | string {
  if (!isObject(body)) return "expected a JSON object";
  switch (provider) {
    case "anthropic": {
      const key = secret(body["api_key"]);
      return key ? { env: { ANTHROPIC_API_KEY: key } } : "api_key must be 8 to 4096 characters without spaces";
    }
    case "openai": {
      const key = secret(body["api_key"]);
      return key ? { env: { OPENAI_API_KEY: key } } : "api_key must be 8 to 4096 characters without spaces";
    }
    case "bedrock": {
      const region = body["region"];
      if (typeof region !== "string" || !REGION.test(region)) return "region must be an AWS region, such as us-east-1";
      const env: Record<string, string> = { CLAUDE_CODE_USE_BEDROCK: "1", AWS_REGION: region };
      if (body["api_key"] !== undefined) {
        const key = secret(body["api_key"]);
        if (!key) return "api_key must be 8 to 4096 characters without spaces";
        if (body["access_key_id"] !== undefined || body["secret_access_key"] !== undefined) return "give api_key or an access key, not both";
        return { env: { ...env, AWS_BEARER_TOKEN_BEDROCK: key } };
      }
      const id = secret(body["access_key_id"]);
      const key = secret(body["secret_access_key"]);
      if (!id || !key) return "give api_key, or access_key_id and secret_access_key";
      env["AWS_ACCESS_KEY_ID"] = id;
      env["AWS_SECRET_ACCESS_KEY"] = key;
      if (body["session_token"] !== undefined) {
        const token = secret(body["session_token"]);
        if (!token) return "session_token must be 8 to 4096 characters without spaces";
        env["AWS_SESSION_TOKEN"] = token;
      }
      return { env };
    }
    case "vertex": {
      const region = body["region"];
      if (typeof region !== "string" || !REGION.test(region)) return "region must be a Google Cloud region, such as us-east5 or global";
      const project = body["project_id"];
      if (typeof project !== "string" || !PROJECT.test(project)) return "project_id must be a Google Cloud project ID";
      let credentials = body["credentials"];
      if (typeof credentials === "string") {
        try {
          credentials = JSON.parse(credentials);
        } catch {
          credentials = null;
        }
      }
      if (!isObject(credentials) || typeof credentials["type"] !== "string") return "credentials must be the JSON of a Google credentials file";
      // One line, whatever the file looked like.
      const file = JSON.stringify(credentials);
      if (utf8(file).length > MAX_FILE) return "credentials are too large";
      return {
        env: { CLAUDE_CODE_USE_VERTEX: "1", CLOUD_ML_REGION: region, ANTHROPIC_VERTEX_PROJECT_ID: project, GOOGLE_APPLICATION_CREDENTIALS: VERTEX_FILE },
        file,
      };
    }
  }
}

/// The key's bytes, or null when the secret is not 32 bytes of base64url.
function keyBytes(env: Env): Uint8Array | null {
  const raw = fromB64url(env.CLOUD_MODEL_KEY ?? "");
  return raw && raw.length === 32 ? raw : null;
}

/// Whether CLOUD_MODEL_KEY can seal. Without it no key is taken.
export function keyUsable(env: Env): boolean {
  return keyBytes(env) !== null;
}

async function key(env: Env): Promise<CryptoKey | null> {
  const raw = keyBytes(env);
  if (!raw) return null;
  return crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]);
}

function aad(label: string, provider: Provider): Uint8Array {
  return utf8(`${label}/${provider}`);
}

export async function seal(env: Env, label: string, provider: Provider, model: Model): Promise<string> {
  const k = await key(env);
  if (!k) throw new Error("CLOUD_MODEL_KEY is not 32 bytes of base64url");
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const body = await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: aad(label, provider) }, k, utf8(JSON.stringify(model)));
  return `v1.${b64url(iv)}.${b64url(new Uint8Array(body))}`;
}

/// The credentials, or null for anything that is not ours, was changed,
/// belongs to another machine or provider, or was sealed under another key.
export async function open(env: Env, label: string, provider: Provider, sealed: string): Promise<Model | null> {
  const parts = sealed.split(".");
  if (parts.length !== 3 || parts[0] !== "v1") return null;
  const iv = fromB64url(parts[1]!);
  const body = fromB64url(parts[2]!);
  if (!iv || iv.length !== 12 || !body) return null;
  const k = await key(env);
  if (!k) return null;
  try {
    const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv, additionalData: aad(label, provider) }, k, body);
    const model: unknown = JSON.parse(new TextDecoder().decode(plain));
    return isObject(model) && isObject(model["env"]) ? (model as unknown as Model) : null;
  } catch {
    return null;
  }
}

/// What model-env gets for the stored credentials. A variable is left out when
/// it would be empty, and model-env then removes its file. Credentials that do
/// not open are left out and logged; the agent then says it is not signed in.
export async function modelVars(env: Env, label: string, stored: Stored): Promise<Record<string, string>> {
  const lines: string[] = [];
  let file = "";
  for (const provider of PROVIDERS) {
    const entry = stored[provider];
    if (!entry) continue;
    const model = await open(env, label, provider, entry.sealed);
    if (!model) {
      console.error(`cloud machine ${label}: the ${provider} credentials do not open`);
      continue;
    }
    for (const [name, value] of Object.entries(model.env)) lines.push(`${name}=${value}`);
    if (model.file) file = model.file;
  }
  return { ...(lines.length > 0 ? { [MODEL_VAR]: lines.join("\n") } : {}), ...(file ? { [VERTEX_VAR]: file } : {}) };
}
