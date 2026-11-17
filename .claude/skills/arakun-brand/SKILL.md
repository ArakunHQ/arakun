---
name: arakun-brand
description: Apply Arakun's brand book (black and white with one amber accent, the raccoon logo and mascot, Geist type, the plain voice) to anything people see outside the app's own UI code - the README, docs, the site, social cards, release notes, notifications, store pages, Doop frames and marketing images - and to user-facing copy anywhere. Use when the user asks for something "on brand", mentions the logo, mascot, raccoon, brand colors or the brand book, or when you write or review text or visuals that represent Arakun.
---

# The Arakun brand

The brand book is the Doop canvas
[Brandbook](https://doop.arakun.dev/c/bnj8ZjYM5d): eleven pages, in a dark
book and a light one. Its style guide `arakun-brand` and its tokens are the
source of truth. This skill is a copy of them; if the two disagree, the canvas
is right, and this file needs an update.

To read the canvas, use the Doop MCP tools: `get_guidelines` (name
`arakun-brand`), `get_tokens`, `get_frame_screenshot`. Its HTTP API needs a
login, but the asset and render URLs below are public.

In the app itself, colors and sizes come from `src/theme.rs` and `src/ui.rs`;
follow the `ui-change` skill there. This skill covers everything around the app,
and the words in it.

## Color: black, white and one lamp

| Role | Hex | Use |
| --- | --- | --- |
| Black | `#0B0B0B` | The dark ground: pages, the site, social cards, docs. |
| White | `#FFFFFF` | The light ground, and text on black. |
| Amber | `#FFB547` | The one accent. |
| Amber as text on white | `#995C00` | Amber text or thin lines on a light ground (5.4:1). |
| Ink | `#151515` | Logo only: the icon tile and the mask. |
| Paper | `#F1EFE8` | Logo only: the raccoon's fur. |

- Grounds are neutral. No blue, warm or green tint in black or white.
- Amber is the raccoon's eyes, the active item, a live dot and the one primary
  button in a view. Keep it under about 10% of a surface. On a page, black is
  about 70%, white 24% and amber 6%.
- On white, amber may be a fill. As text or a thin line it becomes `#995C00`.
- Text on an amber fill is `#151515`.
- Status colors (passed, failed, running, merged, draft) and the dark and light
  surface grays are in `reference.md`. Use them for status only, never as
  decoration.

## Logo: four ways to show it

The logo, the app icon and the mascot share one face: paper fur, an ink mask
with a thin forehead stripe, amber eyes and an ink nose.

| Version | Where | File |
| --- | --- | --- |
| App icon | Default, anywhere it fits | https://doop.arakun.dev/a/ECUeF5YPUV.svg |
| App icon below 48 px | Favicons, tabs | https://doop.arakun.dev/a/GToC69rXx3.svg |
| Paper mark, dark ground | Bare raccoon on black or ink | https://doop.arakun.dev/a/aLt0-FPXqF.svg |
| Paper mark, light ground | Bare raccoon on white, ink ear insides | https://doop.arakun.dev/a/fz3FLHr1cD.svg |

- On a light ground the raccoon stands straight on white, or sits on its ink
  tile where it needs weight (favicons, print, invoices).
- Lockup: the icon, then "Arakun" in Geist SemiBold at -4% tracking. The gap is
  a quarter of the icon.
- Clear space: a quarter of the tile width on every side.
- One raccoon per composition. Never a small mark next to a big one, on a
  cover, a social card or a footer.
- Don't: recolor the fur, mask or tile; use an ink raccoon as the logo on
  light; stretch or squash it; add glows, gradients, outlines, bevels or
  shadows; invent a fifth version.

The files in `assets/brand/` are older drawings and do not match the book yet.
Use the URLs above for new work. Don't change `assets/brand/` or
`tools/brand-logo.py` unless the user asks.

## Mascot and imagery

- Mascot: https://doop.arakun.dev/a/mHmDydeYcl.svg. The same raccoon as a
  character: white fur on dark grounds, ink fur on light pages. No tile or
  panel. In the app it can blink, sleep and spell itself in ASCII; on pages it
  stays still.
- Screenshots are real captures of the real app, always inside a window frame
  (the `AppWindow` component on the canvas). Never mock the UI.
- Illustrations are line-iso drawings: thin ink outlines, three flat tones per
  solid, one amber part for what moves. No gradients, shadows or glows. Use the
  `arakun-iso` skill to make them.

## Type

- Geist 400, 500 and 600 for everything people read.
- Geist Mono for commands, paths, hex values and uppercase eyebrows, with +8%
  tracking on the eyebrows.
- Titles in SemiBold at -3% tracking. Sizes: 72 px display, 40 px section
  titles, 28 px subheads, 16 px body on pages and docs.

## UI and icons outside the app

- Line icons from `assets/icons`: outlined, 1.75 stroke, rounded caps on a
  24 px grid, drawn at 16-20 px. Never filled, never two-tone.
- Icons sit in the muted text color and turn to the text color on hover. Amber
  only for the active item or a live dot (`#995C00` on white). Never carry
  meaning by color alone; a state is a dot plus a word.
- Corners: 4 px chips, 8 px buttons and inputs, 12 px cards, 16 px dialogs.
- One primary (amber) button per view.
- No GitHub or shield badges, in the README or on the site. State a platform's
  status in plain text.

## Voice: say what happened, then stop

The same rules cover the app, the docs, notifications and the site.

- **Plain words.** Write for someone reading their third language at 2 a.m.
  No jargon you can drop, no idioms.
- **Short sentences.** One idea each. If a sentence needs a semicolon, make it
  two.
- **Facts before feelings.** Who did what, where, and what the user can do
  now. No cheering, no vague apologies.
- **Name the thing.** Real names: `Opus 5.5`, `env/brave-otter`,
  `arakun open`. Never "your AI" or "the thing".

| Do | Don't |
| --- | --- |
| Opus 5.5 finished in env/brave-otter. 3 files changed. | Awesome!! Your AI agent has successfully completed its amazing task! |
| The relay is offline. Your phone reconnects when it is back. | Oops! Something went wrong. Please try again later. |
| Open pull request | Let's ship it! |

A button label names the action it does. A notification says agent, place and
what it needs.

## Applications

Every one comes in dark and light.

- **Social card, 1200 x 630.** Black (or white) ground, "Arakun" as the title,
  the one line "One native window for your coding agents.", the bare raccoon
  cropped at the right edge. Nothing else.
- **README header.** Centred icon, the name, the one line. Nothing else.
- **Install line.** Mono, a faint `$` prompt, the URL in amber, one copyable
  line: `curl -fsSL https://arakun.dev/install | sh`.
- **Phone notification.** The icon carries the brand; the text follows the
  voice: "Opus 5.5 needs an answer in env/brave-otter."
- **Favicon and tab.** The below-48 px icon: wider eyes, chunkier parts.

## Check your work

Before you hand over a visual, look at it (a screenshot, `get_frame_screenshot`,
or the rendered file) and check:

1. One raccoon, one of the four logo versions, unchanged.
2. Black or white ground, no tint. Amber only on the one active or primary
   thing, and `#995C00` for amber text on white.
3. Geist and Geist Mono only, at the sizes above.
4. Real screenshots in a window frame, no mocked UI, no badges.
5. Every sentence passes the voice rules.

To compare with the book, open the page renders:
`https://doop.arakun.dev/i/<frame-id>.png`. Frame ids are listed in
`reference.md`.
