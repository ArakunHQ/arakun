// Photos picked on the phone: drawn small and re-encoded as JPEG so what goes
// over the socket is a few hundred kilobytes, then cut into pieces small
// enough for a relay frame; the computer saves each one as a pasted image.

import { activeLink } from "./api";
import type { ClientMessage } from "./model";

export interface Shot {
  name: string;
  /** The JPEG as base64, without the data: prefix. */
  b64: string;
  /** A small data: URL for the chip. */
  thumb: string;
}

/** The relay's frames are small: each piece carries this much of the base64 at most. */
export const SHOT_PIECE = 160 * 1024;
const LONGEST_SIDE = 1568;

/** Sends `shot` as upload `id` of `session`, in pieces. */
export function sendShot(shot: Shot, id: string, session: string, send: (m: ClientMessage) => void = (m) => activeLink()?.send(m)): void {
  const of = Math.max(1, Math.ceil(shot.b64.length / SHOT_PIECE));
  for (let i = 0; i < of; i++) {
    send({ type: "upload", session, upload: id, name: shot.name, part: i, of, data: shot.b64.slice(i * SHOT_PIECE, (i + 1) * SHOT_PIECE) });
  }
}

/** `file` made small enough to send, or null when it is not a picture that decodes. */
export async function readShot(file: File): Promise<Shot | null> {
  if (!file || !/^image\//.test(file.type)) return null;
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    const scale = Math.min(1, LONGEST_SIDE / Math.max(img.width, img.height));
    const w = Math.max(1, Math.round(img.width * scale));
    const h = Math.max(1, Math.round(img.height * scale));
    const c = document.createElement("canvas");
    c.width = w;
    c.height = h;
    c.getContext("2d")?.drawImage(img, 0, 0, w, h);
    const blob = await new Promise<Blob | null>((res) => c.toBlob(res, "image/jpeg", 0.82));
    if (!blob) return null;
    const b64 = await new Promise<string>((res) => {
      const r = new FileReader();
      r.onload = () => res(String(r.result).split(",")[1] || "");
      r.onerror = () => res("");
      r.readAsDataURL(blob);
    });
    if (!b64) return null;
    return { name: file.name || "photo.jpg", b64, thumb: c.toDataURL("image/jpeg", 0.3) };
  } finally {
    URL.revokeObjectURL(url);
  }
}
