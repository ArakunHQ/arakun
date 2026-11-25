// What opens over the session: a picture full size, a table or a drawing full
// screen. And the pictures themselves, which through the relay are fetched
// apart and shown from blobs.

import { type ReactNode, useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import { api } from "../../lib/api";
import { DIAGRAMS } from "../../lib/globals";
import { Icon } from "../../lib/icons";

/**
 * The relay serves no images: the computer sends them inside the channel.
 * `src(url)` gives the address to show a picture from: as written for data:,
 * http and everything when served by the computer itself; a blob URL for the
 * computer's own `/…` links through the relay, or undefined while it is
 * fetched. The blobs go when the screen does.
 */
export function usePics(relay: boolean) {
  const pics = useRef(new Map<string, string>());
  const asked = useRef(new Set<string>());
  const [, bump] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    const kept = pics.current;
    const tried = asked.current;
    return () => {
      for (const u of kept.values()) URL.revokeObjectURL(u);
      kept.clear();
      tried.clear();
    };
  }, []);

  const fetchPic = useCallback(async (url: string): Promise<string | undefined> => {
    const got = pics.current.get(url);
    if (got) return got;
    try {
      const res = await api(url);
      if (!res.ok) return undefined;
      const blob = URL.createObjectURL(await res.blob());
      pics.current.set(url, blob);
      bump();
      return blob;
    } catch {
      // The link went away, or the computer with it; the picture stays blank.
      return undefined;
    }
  }, []);

  const src = useCallback(
    (url: string): string | undefined => {
      if (!relay || !url.startsWith("/")) return url;
      const got = pics.current.get(url);
      if (got) return got;
      // Asked once per address; the answer draws the screen again.
      if (!asked.current.has(url)) {
        asked.current.add(url);
        setTimeout(() => void fetchPic(url), 0);
      }
      return undefined;
    },
    [relay, fetchPic],
  );

  /** The address to show full size, fetched first if need be. */
  const full = useCallback(
    async (url: string): Promise<string | undefined> => (!relay || !url.startsWith("/") ? url : fetchPic(url)),
    [relay, fetchPic],
  );

  return useMemo(() => ({ src, full }), [src, full]);
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
