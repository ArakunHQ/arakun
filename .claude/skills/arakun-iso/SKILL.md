---
name: arakun-iso
description: Create, revise and export animated isometric illustrations in Arakun's line-iso style (thin ink outlines, three-tone faces, one accent) as Rhai scene scripts that the Isometric tab plays live. Use when the user asks for an isometric illustration, an animated product diagram, a hero or feature illustration "like illos", or to fix or polish a scene in iso/*.rhai.
---

# Isometric scenes in Arakun

A scene is a `.rhai` script in the project's `iso/` folder. It runs once per
frame with `t` (0..1, the place in the loop) and returns the shapes to paint,
back to front. The Isometric tab (rail → Isometric) lists the folder's scenes
and reloads one the moment it is saved; errors show above the stage with the
line. `arakun iso` renders frames without the window, so you can look at what
you wrote.

New scene and Change in the tab start a session whose first prompt names the
one file to write (`iso/<name>.rhai`). Write exactly that file and keep its
name: the tab selects it as soon as it appears. A prompt that says the scene
is a copy of a built-in one means the file already holds that starter; edit
it in place.

Read `reference.md` (next to this file) for every function before writing.
`iso/billing.rhai` (a coin meter), `iso/cpu.rhai` (a circuit board) and
`iso/ec2.rhai` (a guided tour with captions, drawn-on labels and a spotlight)
are complete examples; copy their structure.

## How to work

1. **Pin down the story first.** One sentence of what the picture explains,
   then the loop as 2–4 beats ("the spreader lifts → work runs in on the bus →
   cores light in turn → all settles"). Each beat is a window of `t`. If the
   user gave no story, propose one and the beats before drawing.
2. **Lay it out in world units on paper.** x runs down-right, y down-left, z
   up. Pick the hero (the one big object) and place it near the middle; the
   supporting parts around it, never crowding it. Write the coordinates of the
   big blocks as `let` constants at the top.
3. **Write the static scene**, still (`t` unused): `arakun iso render
   iso/name.rhai --width 1600` and look at the PNG. Fix composition before any
   motion: balance, empty areas, overlaps, painter order.
4. **Add motion** beat by beat with `keyed`. Then `arakun iso check
   iso/name.rhai` (must say *seamless*) and `arakun iso render iso/name.rhai
   --frames 16` for a sheet of the whole loop. Look at every cell.
5. **Review against the checklist below**, fix, re-render. Repeat until a
   frame could sit next to an illos.dev illustration. Expect 3–6 rounds; say
   what you changed each round.
6. Tell the user the scene is in the Isometric tab (it is already there if
   the tab is open on this project) and `Copy SVG` exports a frame.

## The style

- **Line**: every solid gets a full-ink outline (`ink("edge")`, the default)
  on its silhouette and top. Details (insets, traces, ticks, pin rows) use
  `ink("mute")`. Never use thick or colored outlines on neutral parts.
- **Volume** comes from three flat tones — top, left wall, right wall — and
  nothing else: no gradients, shadows or glows.
- **Accent**: one color, used for what *moves or matters* in the story (the
  coins, the lit cores, the LED). Keep it under ~10% of the picture. Lit
  parts: `.accent()` fill with `ink("accent")`; secondary accent lines
  `accent_mute`.
- **Living things are organic**: build creatures from merged ellipsoids
  with decals (see `reference.md`), never from boxes. Boxes and cylinders
  are for machines and buildings.
- **Radii**: big bodies 10–16, mid parts 4–8, small parts 0.6–3. Sharp boxes
  (`r` 0) only for sheets of paper, pins and fingers.
- **Detail density** sells it: repeat small honest parts (pin rows, dot
  grids, resistors, ticks, screws) with loops. Detail goes on surfaces the
  eye travels across, not on the hero's top.
- **Text** is rare in a loop: one or two `label` callouts in caps, outside
  the object, leader lines level. Never put text on a surface. When the user
  wants it *explained*, make a guided tour instead (see `reference.md`):
  captions per step on a `card`, labels that draw from the words to the
  thing, everything off-topic dimmed, and a `camera` that moves in on what
  each step is about.
- **Style**: the default is flat line-iso (docs, diagrams). When the user
  wants it to look 3D or "like a game", use `style: "soft"` with real colors
  (`.color()`), contact shadows under everything that stands, and props that
  tell the place's story (`iso/raccoon.rhai`).
- **Look**: `paper` (white, ink #2a2a2a, teal) for docs and marketing;
  `arakun` (night blue, amber) to match the app. Set it in `scene()`.

## Motion

- Loops of 3–6 s. End every motion where it began (the frame at `t` 1 equals
  `t` 0) — `arakun iso check` verifies it.
- Easing: `"out"` (strong ease-out) for things arriving and lifting,
  `"in_out"` for travel and settling. Nothing linear except fades.
- Stagger repeated things (coins, cores, packets) by a fixed step; let the
  last ones take a little less (a smaller bounce, a shorter fade).
- Things that fade use a `layer(n)` (one number per object, shared by all its
  parts) so the object fades as one; give every object its own number.
- Things that sink into or rise out of something: `clip_above(z)` at the
  surface, and draw the container's near half after them (`tube_back` →
  contents → `tube_front`; a slot plate → the sheet).
- Hold still moments: give each beat room (≥ 0.1 of the loop) before the next.

## Idle motion: nothing ever stands still

A guided stop (or any quiet moment) must keep moving, on `tau`. Blinking
lights alone are not motion. What motion designers use, from most to least
visible, and how to do it here:

- **The subject breathes**: the thing the step is about floats 1.5–2.5
  units up and down (`0.5 - 0.5 * cos(tau / 3 * 2π)`), or opens like an
  exploded view (layers spread a few units and close).
- **Flows show direction**: cables and borders get `.flow(tau * 2)` so their
  dashes travel the way data goes; particles ride paths with `along`.
- **Mechanisms turn**: fans, disks, wheels rotate (lines from a center at
  angle `tau / period * 2π`, on the face they sit on).
- **Gauges live**: memory, levels and bars fill and drain, each on its own
  rhythm.
- **The camera drifts**: zoom ±1.5% over 9 s and a 2-unit sway over 13.5 s,
  so even a still frame is never a photograph.
- **Lights** come last, as seasoning.

Rules: give the biggest motion to the step's subject and keep the rest
small; give each motion its own period (3, 4.5, 5.4, 9 s...) so they never
beat together, all dividing `seconds` so the video loops; smooth (cos/sin)
curves, no snaps. Check a stop with `arakun iso render scene.rhai --at
<stop> --tau 40 --frames 4`.

## Review checklist

Look at a `--frames 16` sheet and one large frame for each beat:

- [ ] Painter order: nothing far draws over something near (sort big blocks
      back to front: smaller x+y first).
- [ ] No ghosts: parts that sank or left are gone, not peeking through walls.
- [ ] Fading objects fade whole (one `layer` each), no inner lines showing.
- [ ] Nothing pops: things appear by fade, rise or slide, never by a cut.
- [ ] Tiny shapes do not degenerate (a bar thinner than 2·r becomes a dot;
      hide it below that).
- [ ] The accent is only on the story; the rest is ink and tones.
- [ ] The hero reads at thumbnail size (look at the sheet cells).
- [ ] Labels sit off the object, text does not cross lines.
- [ ] Text never fades over text: a caption is gone before the next one
      comes in (crossfaded text turns to mush at the halfway frame). Check
      the frames right at each step change.
- [ ] Marks meant for the viewer (✕, ✓, numbers) are `text` facing the
      camera; a cross drawn in a world plane flattens into two dashes.
- [ ] Nothing important hides behind something that lines up with it on
      screen: in isometric a far object and a near one can share the same
      vertical. Move the far one, or stop it somewhere visible.
- [ ] Parts on a top keep a margin from its rounded corners, and rows of
      details sit centered on what holds them: place them with `centered()`,
      never with hand-picked offsets, and check equal margins on both sides.
- [ ] A guided scene (`steps`): render each stop (`--at (k + hold) / steps`)
      and check each is a good still on its own: the step's point visible,
      caption and labels up, nothing frozen half-way. And no stop is dead:
      what the step is about keeps moving on `tau` (it repeats its little
      cycle, breathes or flows) so the reader sees what changes.
- [ ] With a camera: every label reads whole in every step (none cut at
      the frame, none sitting on the drawing), and the caption card is never
      crossed by the scene's lines.
- [ ] Nothing important stays still for a whole step: give quiet steps a
      little life (lights, a pulse) so the tour never feels frozen.
- [ ] `arakun iso check`: seamless, no empty frames, under ~3 ms a frame.

## Commands

```
arakun iso check  iso/name.rhai                   # errors, seam, cost
arakun iso render iso/name.rhai [--at 0.4] [--width 1600] [--look paper|arakun] [--out f.png]
arakun iso render iso/name.rhai --frames 16       # contact sheet of the loop
arakun iso svg    iso/name.rhai [--at 0.4] [--out f.svg]
arakun iso anim   iso/name.rhai [--fps 240] [--px 0.1] [--out f.svg]   # the loop for a web page: ONE light SVG (SMIL), exact to --px
```

PNGs go to `$TMPDIR/arakun-iso/` unless `--out`; read them to look.
