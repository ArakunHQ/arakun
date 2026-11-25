import { describe, expect, it } from "vitest";

// The page's colors and corners come from tokens.css, as the app's come from
// theme and ui (src/design_lint.rs). This fails on a hand-written one.

// Every .css and .tsx under src/, as text, but tokens.css and the tests.
const FILES = import.meta.glob(["../**/*.{css,tsx}", "!./tokens.css", "!../**/*.test.tsx"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;

// Comments may name a color to say why; only code counts.
function code(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " ")).replace(/^\s*\/\/.*$/gm, "");
}

const HEX = /(?<![\w-])#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})(?![\w-])/g;
const FUNC = /\b(?:rgba?|hsla?)\(/g;
const RADIUS = /(?:^|[\s;{])((?:border-(?:[a-z]+-)*radius)\s*:\s*([^;}]+))|borderRadius\s*:\s*([^,}]+)/g;
const RADIUS_OK = /^(?:var\(--radius-[\w-]+\)|50%|999px|0|inherit|\/)$/;

/** What is hand-written in one file, as "line: text". */
export function lint(text: string): string[] {
  const out: string[] = [];
  code(text)
    .split("\n")
    .forEach((line, i) => {
      const at = `${i + 1}: `;
      for (const m of line.matchAll(HEX)) out.push(at + m[0]);
      for (const m of line.matchAll(FUNC)) out.push(at + m[0]);
      for (const m of line.matchAll(RADIUS)) {
        const value = (m[2] ?? m[3] ?? "").replace(/!important/, "").replace(/["'`]/g, "").trim();
        const parts = value.replace(/\//g, " / ").split(/\s+/).filter(Boolean);
        if (!parts.length || parts.some((p) => !RADIUS_OK.test(p))) out.push(at + (m[1] ?? m[0]).trim());
      }
    });
  return out;
}

describe("design tokens", () => {
  it("catches a hand-written color or corner", () => {
    expect(lint(".a { color: #fff; background: rgba(0, 0, 0, 0.5); border-radius: 6px; }")).toEqual([
      "1: #fff",
      "1: rgba(",
      "1: border-radius: 6px",
    ]);
    expect(lint('<span style={{ color: "#f38020", borderRadius: "3px" }} />')).toEqual(["1: #f38020", '1: borderRadius: "3px"']);
    expect(lint(".a { border-radius: var(--radius-xl) var(--radius-xl) var(--radius-sm) 0; }")).toEqual([]);
    expect(lint(".a { border-radius: 50%; } .b { border-radius: 999px; } #picview { color: var(--color-text); }")).toEqual([]);
    expect(lint("/* amber text on white is #995C00 */\n// rgb( in a comment")).toEqual([]);
  });

  it("is the only place colors and corners are written", () => {
    const names = Object.keys(FILES);
    // A glob that finds nothing would pass: make sure it sees the page.
    expect(names).toContain("../main.tsx");
    expect(names).toContain("./base.css");
    const found = names.flatMap((f) => lint(FILES[f]).map((x) => `${f}:${x}`));
    expect(found).toEqual([]);
  });
});
