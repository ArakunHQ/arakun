// The shapes in PROTOCOL.md and the signature checks on them. The Worker and the
// channel Durable Object both use these. Signatures are ECDSA P-256 with
// SHA-256, raw r||s (64 bytes), which is what WebCrypto makes and checks.

import { ULID, b64url, fromB64url, isObject, utf8 } from "./util";

/// base64url of 16 bytes.
export const DEVICE_ID = /^[A-Za-z0-9_-]{22}$/;
/// A message's ct, decoded.
export const MAX_MESSAGE_CT = 64 * 1024;
/// A Sealed channel name's ct, decoded: 100 characters of up to 4 bytes, and the tag.
const MAX_NAME_CT = 1024;
const AES_GCM_TAG = 16;

/// base64url of the first 16 bytes of SHA-256(sig_pub).
export async function deviceIdOf(sigPub: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", sigPub as BufferSource));
  return b64url(digest.slice(0, 16));
}

/// An uncompressed SEC1 P-256 point: 65 bytes starting with 4.
function isPoint(raw: Uint8Array | null): raw is Uint8Array {
  return raw !== null && raw.length === 65 && raw[0] === 4;
}

/// The verify key for a device's sig_pub, or null if it is not a P-256 point.
export async function sigKey(sigPub: string): Promise<CryptoKey | null> {
  const raw = fromB64url(sigPub);
  if (!isPoint(raw)) return null;
  try {
    return await crypto.subtle.importKey("raw", raw as BufferSource, { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
  } catch {
    return null;
  }
}

/// Whether enc_pub is a P-256 point an ECDH key can be made of.
export async function isEncKey(encPub: string): Promise<boolean> {
  const raw = fromB64url(encPub);
  if (!isPoint(raw)) return false;
  try {
    await crypto.subtle.importKey("raw", raw as BufferSource, { name: "ECDH", namedCurve: "P-256" }, false, []);
    return true;
  } catch {
    return false;
  }
}

export async function verify(key: CryptoKey, sig: string, text: string): Promise<boolean> {
  const raw = fromB64url(sig);
  if (!raw || raw.length !== 64) return false;
  return crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, raw as BufferSource, utf8(text) as BufferSource);
}

export function deviceSigned(githubId: number, sigPub: string, encPub: string): string {
  return `arakun-teams/v1/device|${githubId}|${sigPub}|${encPub}`;
}

function bytesOf(v: unknown, min: number, max: number): boolean {
  if (typeof v !== "string") return false;
  const raw = fromB64url(v);
  return raw !== null && raw.length >= min && raw.length <= max;
}

function isEpoch(v: unknown): v is number {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 1;
}

export interface Envelope {
  v: 1;
  team_id: string;
  epoch: number;
  device_id: string;
  from: string;
  eph_pub: string;
  nonce: string;
  ct: string;
  sig: string;
}

/// An envelope for `teamId` with every field in shape, rebuilt with only the
/// fields we know so what is stored is what was checked. A string says why not.
export function parseEnvelope(v: unknown, teamId: string): Envelope | string {
  if (!isObject(v) || v["v"] !== 1) return 'an envelope is an object with "v": 1';
  if (v["team_id"] !== teamId) return "an envelope is for another team";
  if (!isEpoch(v["epoch"])) return "an envelope's epoch is a positive integer";
  for (const field of ["device_id", "from"]) {
    if (typeof v[field] !== "string" || !DEVICE_ID.test(v[field] as string)) return `an envelope's ${field} is not a device ID`;
  }
  if (!isPoint(typeof v["eph_pub"] === "string" ? fromB64url(v["eph_pub"]) : null)) return "an envelope's eph_pub is not a P-256 public key";
  if (!bytesOf(v["nonce"], 12, 12)) return "an envelope's nonce is 12 bytes";
  // AES-256-GCM of a 32-byte team key.
  if (!bytesOf(v["ct"], 32 + AES_GCM_TAG, 32 + AES_GCM_TAG)) return "an envelope's ct is 48 bytes";
  if (!bytesOf(v["sig"], 64, 64)) return "an envelope's sig is 64 bytes";
  const e = v as unknown as Envelope;
  return {
    v: 1,
    team_id: e.team_id,
    epoch: e.epoch,
    device_id: e.device_id,
    from: e.from,
    eph_pub: e.eph_pub,
    nonce: e.nonce,
    ct: e.ct,
    sig: e.sig,
  };
}

export function envelopeSigned(e: Envelope): string {
  return `arakun-teams/v1/envelope|${e.team_id}|${e.epoch}|${e.device_id}|${e.eph_pub}|${e.nonce}|${e.ct}`;
}

export interface Sealed {
  v: 1;
  epoch: number;
  nonce: string;
  ct: string;
}

/// A Sealed channel name, rebuilt with only its fields, or why not.
export function parseSealedName(v: unknown): Sealed | string {
  if (!isObject(v) || v["v"] !== 1) return 'name_ct is a Sealed object with "v": 1';
  if (!isEpoch(v["epoch"])) return "name_ct's epoch is a positive integer";
  if (!bytesOf(v["nonce"], 12, 12)) return "name_ct's nonce is 12 bytes";
  if (!bytesOf(v["ct"], AES_GCM_TAG, MAX_NAME_CT)) return `name_ct's ct is ${AES_GCM_TAG} to ${MAX_NAME_CT} bytes`;
  return { v: 1, epoch: v["epoch"], nonce: v["nonce"] as string, ct: v["ct"] as string };
}

export interface WireMessage {
  id: string;
  device_id: string;
  epoch: number;
  nonce: string;
  ct: string;
  sig: string;
}

/// A message as a client sent it, rebuilt with only its fields, or why not.
export function parseMessage(v: unknown): WireMessage | string {
  if (!isObject(v)) return "message is an object";
  if (typeof v["id"] !== "string" || !ULID.test(v["id"])) return "id is a ULID";
  if (typeof v["device_id"] !== "string" || !DEVICE_ID.test(v["device_id"])) return "device_id is not a device ID";
  if (!isEpoch(v["epoch"])) return "epoch is a positive integer";
  if (!bytesOf(v["nonce"], 12, 12)) return "nonce is 12 bytes";
  if (!bytesOf(v["ct"], AES_GCM_TAG, MAX_MESSAGE_CT)) return `ct is ${AES_GCM_TAG} bytes to 64 KiB`;
  if (!bytesOf(v["sig"], 64, 64)) return "sig is 64 bytes";
  return {
    id: v["id"],
    device_id: v["device_id"],
    epoch: v["epoch"],
    nonce: v["nonce"] as string,
    ct: v["ct"] as string,
    sig: v["sig"] as string,
  };
}

export function messageSigned(teamId: string, channelId: string, m: WireMessage): string {
  return `arakun-teams/v1/message|${teamId}|${channelId}|${m.id}|${m.epoch}|${m.nonce}|${m.ct}`;
}
