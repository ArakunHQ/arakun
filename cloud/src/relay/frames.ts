// What goes over the sockets. Everything is binary with a type byte in front:
// hello and data frames the relay never looks inside, control frames it speaks
// itself (JSON). The computer's socket carries many phones, so its frames add a
// channel number after the type; a phone's frames have none. Channel 0 is the
// relay talking to the computer.

export const HELLO = 0x01;
export const DATA = 0x02;
export const CONTROL = 0x03;

export interface PcFrame {
  type: number;
  channel: number;
  payload: Uint8Array;
}

export interface PhoneFrame {
  type: number;
  payload: Uint8Array;
}

export function pcFrame(type: number, channel: number, payload: Uint8Array): ArrayBuffer {
  const out = new Uint8Array(5 + payload.length);
  out[0] = type;
  new DataView(out.buffer).setUint32(1, channel);
  out.set(payload, 5);
  return out.buffer;
}

export function parsePcFrame(bytes: Uint8Array): PcFrame | null {
  if (bytes.length < 5) return null;
  const type = bytes[0]!;
  if (type !== HELLO && type !== DATA && type !== CONTROL) return null;
  const channel = new DataView(bytes.buffer, bytes.byteOffset).getUint32(1);
  return { type, channel, payload: bytes.subarray(5) };
}

export function phoneFrame(type: number, payload: Uint8Array): ArrayBuffer {
  const out = new Uint8Array(1 + payload.length);
  out[0] = type;
  out.set(payload, 1);
  return out.buffer;
}

export function parsePhoneFrame(bytes: Uint8Array): PhoneFrame | null {
  if (bytes.length < 1) return null;
  const type = bytes[0]!;
  if (type !== HELLO && type !== DATA && type !== CONTROL) return null;
  return { type, payload: bytes.subarray(1) };
}

/// A control message to the computer (channel 0) or from it.
export function pcControl(msg: object): ArrayBuffer {
  return pcFrame(CONTROL, 0, utf8(JSON.stringify(msg)));
}

/// A control message to a phone.
export function phoneControl(msg: object): ArrayBuffer {
  return phoneFrame(CONTROL, utf8(JSON.stringify(msg)));
}

export function parseJson(payload: Uint8Array): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(new TextDecoder().decode(payload));
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

export function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

export function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

export function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromB64url(s: string): Uint8Array | null {
  if (typeof s !== "string" || /[^A-Za-z0-9_-]/.test(s)) return null;
  try {
    const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

export async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes as BufferSource));
}
