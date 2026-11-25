// What opens over the session: a picture full size, a table or a drawing full
// screen. And the pictures themselves, which through the relay are fetched
// apart and shown from blobs.

import { type ReactNode, useEffect, useState, useSyncExternalStore } from "react";
import { api } from "../../lib/api";
import { DIAGRAMS } from "../../lib/globals";
import { Icon } from "../../lib/icons";
import "./Views.css";

/**
 * The relay serves no images: the computer sends them inside the channel.
 * Through the relay the computer's own `/…` links are fetched apart, once at
 * a time each, and shown from blob URLs; everything else (data:, http, all
 * of it on the computer's own server) loads as written. The blobs go when
 * the screen does.
 */
export class PicStore {
  private got = new Map<string, string>();
  private flying = new Map<string, Promise<string | undefined>>();
  private listeners = new Set<() => void>();
  private alive = true;

  constructor(private relay: boolean) {}

  private fetched(url: string): boolean {
    return this.relay && url.startsWith("/");
  }

  /** The address to show `url` from now; undefined while it is fetched. */
  get = (url: string): string | undefined => (this.fetched(url) ? this.got.get(url) : url);

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  /** Fetches `url` unless it is here or on its way. A failure is tried again on the next call. */
  load = (url: string): Promise<string | undefined> => {
    if (!this.fetched(url)) return Promise.resolve(url);
    const got = this.got.get(url);
    if (got) return Promise.resolve(got);
    const flying = this.flying.get(url);
    if (flying) return flying;
    const p = api(url)
      .then(async (res) => {
        if (!res.ok) return undefined;
        const blob = URL.createObjectURL(await res.blob());
        // The screen went while this was on its way.
        if (!this.alive) {
          URL.revokeObjectURL(blob);
          return undefined;
        }
        this.got.set(url, blob);
        this.emit();
        return blob;
      })
      // The link went away, or the computer with it; the picture stays blank.
      .catch(() => undefined)
      .finally(() => this.flying.delete(url));
    this.flying.set(url, p);
    return p;
  };

  revive(): void {
    this.alive = true;
  }

  dispose(): void {
    this.alive = false;
    for (const u of this.got.values()) URL.revokeObjectURL(u);
    this.got.clear();
    // Pictures still on screen (StrictMode mounts again) ask anew.
    this.emit();
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }
}

/** The screen's pictures, let go of when it goes. */
export function usePics(relay: boolean): PicStore {
  const [store] = useState(() => new PicStore(relay));
  useEffect(() => {
    store.revive();
    return () => store.dispose();
  }, [store]);
  return store;
}

/**
 * A picture of the transcript. It follows the store itself, so a blob that
 * arrives draws this picture only, not the entry around it.
 */
export function Pic({ store, url, alt, onTap }: { store: PicStore; url: string; alt: string; onTap: (url: string, alt: string) => void }) {
  const src = useSyncExternalStore(store.subscribe, () => store.get(url));
  useEffect(() => {
    if (!src) void store.load(url);
  }, [store, url, src]);
  return <img src={src} alt={alt} className="pic" loading="lazy" onClick={() => onTap(url, alt)} />;
}

/** A picture over everything; a tap anywhere closes it. The page never navigates away from the app. */
export function PicView({ src, alt, onClose }: { src: string; alt: string; onClose: () => void }) {
  return (
    <div id="picview" onClick={onClose}>
      <img src={src} alt={alt} />
    </div>
  );
}

/** A table or a drawing full screen, under a bar with its title and Close; the phone can turn. */
export function NodeView({ title, node, onClose }: { title: string; node: Node; onClose: () => void }) {
  return (
    <div id="tableview">
      <div className="tvbar">
        <span>{title}</span>
        <button type="button" className="iconbtn" aria-label="Close" onClick={onClose}>
          <Icon name="close" />
        </button>
      </div>
      {/* A copy of what was on screen, so it is placed as a node. */}
      <div className="tvbody" ref={(el) => void el?.replaceChildren(node)} />
    </div>
  );
}

// diagrams.js builds this HTML itself: every piece of the agent's text goes
// through its own esc() first, and the rest is numbers, its fixed palette and
// its own icons. So this is our markup, not the agent's, and the one place
// the screen sets HTML.
function Diagram({ html, onOpen }: { html: string; onOpen: (title: string, node: Node) => void }) {
  // The footer: "Code" swaps the drawing for its source and back, "Open"
  // shows the drawing full screen.
  const tapped = (ev: React.MouseEvent) => {
    const target = ev.target as Element;
    const card = target.closest<HTMLElement>(".diagram");
    if (!card) return;
    const code = target.closest<HTMLElement>(".dgcode");
    if (code) {
      const on = card.classList.toggle("showcode");
      code.setAttribute("aria-pressed", String(on));
      const label = code.querySelector("span");
      if (label) label.textContent = on ? code.dataset.back || "" : "Code";
      return;
    }
    const view = card.querySelector(".dgview");
    if (target.closest(".dgopen") && view) onOpen(card.dataset.title || "Diagram", view.cloneNode(true));
  };
  return <div style={{ display: "contents" }} onClick={tapped} dangerouslySetInnerHTML={{ __html: html }} />;
}

/** A fenced block drawn as a chart or flowchart, or null to leave it as code. */
export function drawDiagram(lang: string, code: string, onOpen: (title: string, node: Node) => void) {
  let html: string | null = null;
  try {
    html = DIAGRAMS.render(lang, code);
  } catch {
    // diagrams.js did not load: the block stays code.
    return null;
  }
  return html ? <Diagram html={html} onOpen={onOpen} /> : null;
}

/** The bar on top of a sheet: Back, its title and what it is about. */
export function SheetHeader({ title, sub, onBack, children }: { title: string; sub?: string; onBack: () => void; children?: ReactNode }) {
  return (
    <div className="hdr">
      <button type="button" className="iconbtn" aria-label="Back" onClick={onBack}>
        <Icon name="back" />
      </button>
      <div className="grow">
        <div className="title">{title}</div>
        {sub ? <div className="sub">{sub}</div> : null}
      </div>
      {children}
    </div>
  );
}
