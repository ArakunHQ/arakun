# Brand tokens and book pages

Copied from the Doop canvas
[Brandbook](https://doop.arakun.dev/c/bnj8ZjYM5d) (tokens and the
`arakun-brand` style guide). Read them fresh with the Doop MCP tool
`get_tokens` (`canvas_id: "bnj8ZjYM5d"`) when they may have changed.

## CSS

Paste this into a page, a social card or a docs site. Dark is the default;
`data-theme="light"` on `<html>` switches to the light book. The app's own
palettes (Graphite, Midnight, Forest, Rosé, Arakun Night) are left out: they
describe the app, not the brand.

```css
:root {
  /* Surfaces and text, dark */
  --color-ground: #0b0b0b;      /* page background */
  --color-raised: #161616;      /* cards, dialogs, menus */
  --color-deep: #050505;        /* stages, wells, code */
  --color-line: #1f1f1f;        /* hairlines and dividers */
  --color-line-strong: #2b2b2b; /* borders, hover fills */
  --color-text: #f2f2f2;        /* body and titles, 17.6:1 */
  --color-text-muted: #a3a3a3;  /* secondary text, 7.8:1 */
  --color-text-faint: #737373;  /* captions, eyebrows, 4.2:1 */

  /* The one accent */
  --color-accent: #ffb547;      /* fills and marks, under ~10% of a surface */
  --color-accent-ink: #ffb547;  /* amber as text or a thin line */
  --color-on-accent: #151515;   /* text on an amber fill */

  /* Fixed brand colors, the same in both themes */
  --color-brand-black: #0b0b0b;
  --color-brand-white: #ffffff;
  --color-brand-amber: #ffb547;
  --color-brand-ink: #151515;   /* logo only: tile and mask */
  --color-brand-paper: #f1efe8; /* logo only: fur */

  /* Status: state only, always with a word */
  --color-success: #3fb950;     /* passed, open PR */
  --color-error: #f85149;       /* failed, closed PR */
  --color-attention: #d29922;   /* running, queued, flaky */
  --color-link: #86a6f2;        /* links, work in progress */
  --color-warn: #fde047;        /* warnings, always with an icon */
  --color-merged: #a371f7;
  --color-draft: #9198a1;
  --color-danger: #e81123;      /* destructive buttons only */

  /* Type */
  --font-sans: "Geist", system-ui, sans-serif;
  --font-mono: "Geist Mono", ui-monospace, monospace;
  --text-xl: 16px;              /* body on pages and docs */
  --text-2xl: 28px;             /* section subheads */
  --text-3xl: 40px;             /* section titles */
  --text-display: 72px;         /* covers and heroes */
  --text-hero: 128px;           /* cover wordmark only */
  --leading-tight: 1.05;
  --leading-snug: 1.3;
  --leading-normal: 1.55;
  --tracking-tight: -0.03em;    /* display and title sizes */
  --tracking-wide: 0.08em;      /* uppercase mono eyebrows */

  /* Shape */
  --radius-sm: 4px;             /* chips, kbd */
  --radius-md: 8px;             /* buttons, inputs */
  --radius-lg: 12px;            /* cards, popovers */
  --radius-xl: 16px;            /* dialogs */
  --radius-tile: 23.5%;         /* app icon tile: 46 of 196 units */
}

[data-theme="light"] {
  --color-ground: #ffffff;
  --color-raised: #f7f7f7;
  --color-deep: #f2f2f2;
  --color-line: #ebebeb;
  --color-line-strong: #dcdcdc;
  --color-text: #0b0b0b;        /* 19.7:1 */
  --color-text-muted: #595959;  /* 7:1 */
  --color-text-faint: #757575;  /* 4.6:1 */
  --color-accent-ink: #995c00;  /* 5.4:1 on white */
  --color-success: #1a7f37;
  --color-error: #cf222e;
  --color-attention: #9a6700;
  --color-link: #3b5fc0;
}
```

Spacing runs on a 4 px grid: 4, 8, 12, 16, 24, 32, 48, 64, 96.

Chart and category colors, in order: blue `#86a6f2`, amber `#ffb547`, green
`#6fd3a0`, violet `#b394f5`, coral `#f5826f`, cyan `#5cc8db`, rose `#ee8fc0`,
sand `#d9c27e`. Line-iso illustrations add paper shade `#dcd9cf` (left walls)
and paper shadow `#c3bfb3` (right walls).

## Book pages

Each page has a public render at `https://doop.arakun.dev/i/<id>.png`, and opens
on the canvas at `https://doop.arakun.dev/c/bnj8ZjYM5d?frame=<id>`.

| Page | Dark | Light |
| --- | --- | --- |
| 01 Cover | `Nxj47aWtQb` | `RybwtRXV2z` |
| 02 Brand | `7CZVqrHf9J` | `urWhucJfP1` |
| 03 Voice | `HGDvd7_Fke` | `5qF8zQnAM9` |
| 04 Logo | `cTEda3rEAr` | `U6RFIOHDpy` |
| 05 Logo usage | `93sCFl2ri6` | `9ZpnNJky-r` |
| 06 Mascot & imagery | `C_Sg9968Rp` | `iQs9yVi4rP` |
| 07 Color | `7NWLIWwnYb` | `S8m33HvddP` |
| 08 Product color | `nwQxWLRzdk` | `5CTBXPhr3W` |
| 09 Typography | `yg7HAYQgpa` | `EO1SHFNdR6` |
| 10 UI & icons | `vBGSggR-8g` | `dfEPeQOzTJ` |
| 11 Applications | `fCJP9ByMQN` | `7WJnv8rjrj` |

Page 08 documents the app's palettes (Arakun Black, Arakun Night and the
others) and its states. The frames "Logo color · six options" and "Background
color · eight options" are explorations, not rules.

## Components on the canvas

When you design on the canvas itself, use its components with `<doop-use>`
instead of drawing them: SectionHeader, Principle, Swatch, TypeRow, RuleCard,
Specimen, AppWindow, Button, StatusPill, NavItem, PageFooter, GroundSample.
