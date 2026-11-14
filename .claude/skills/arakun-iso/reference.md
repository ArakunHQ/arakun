# Scene script reference

Scripts are [Rhai](https://rhai.rs). Numbers can be written as `4` or `4.0`;
mixing them is fine. Two clocks are in scope: `t` (0..1), the story, which
the Guide mode stops at each step; and `tau` (seconds), the ambient clock,
which never stops. Drive what must stay alive at a stop with `tau` (flows,
lights, pulses, a repeating cycle of what the step shows); give each `tau`
cycle a period that divides `seconds`, so the video still loops cleanly
(`arakun iso check` runs with `tau = t * seconds`). The script's last expression is
the shapes to draw, back to front: an array, nested arrays are flattened.

```rhai
scene(#{ name: "Box", about: "One line of what it shows.", seconds: 4.0, look: "paper" });
let n = [];
n.push(slab([0, 0, 0], [120, 80, 8], 10));
n.push(cylinder([60, 40, 8], 14, 20 + 10 * t).accent());
n
```

Functions defined with `fn` cannot see the script's `let` variables; pass
them in. A frame stops after ~2 million script steps (an endless loop is an
error, not a frozen window).

## World

- x runs down-right on screen, y down-left, z up. 1 unit is 1 screen unit
  vertically before the scene is fitted to the stage.
- Visible faces: the top, the **left** wall (the face at the largest y,
  facing down-left) and the **right** wall (largest x, facing down-right).
- Painter's order is the order you push. Push far things (small x + y, low z)
  first.

## Scene

| call | |
|---|---|
| `scene(#{ name, about, seconds, look })` | Title, one-line description, loop length (default 4), `"paper"` or `"arakun"`. |
| `scene(#{ style: "soft" })` | The game-like finish: organic bodies (and their decals) lit with gradients and a sheen, walls darkening toward the ground, soft contact shadows (`shadow`), quieter outlines. Without it, the flat line-iso style. |
| `scene(#{ steps, hold })` | A guided scene: the loop is `steps` equal steps, and the tab gets a **Guide** mode (beside Video) with ← → buttons and keys. → hurries through the rest of the step and plays the next one in full; ← starts the previous step over and plays it to its stop (never in reverse); each stop is `hold` (default 0.85) into its step. Time every step so its story has played and its caption and labels are up at the stop, and nothing important is caught mid-flight there. |

## Solids

| call | |
|---|---|
| `slab(min, size)` / `slab(min, size, r)` | Box from corner `min` `[x,y,z]` with `size` `[w,d,h]`; `r` rounds its vertical edges. |
| `cylinder(base, r, h)` | Upright cylinder standing on `base` (center of its bottom). |
| `tube_back(base, r, inner, h)` | Open-topped tube, the part behind its contents: rim, hollow, floor. |
| `tube_front(base, r, inner, h)` | The tube's near walls and lip; push after the contents. |

## Organic bodies

For animals, people, plants, anything soft: ellipsoids, shaded by the light
(a lit tone and the crescent the light misses), with an exact outline.

| call | |
|---|---|
| `ellipsoid(c, [rx, ry, rz])` / `sphere(c, r)` | A body centered on `c`. Its own axes start as the world's: local +y is its front (it faces down-left on screen), +z its top. |
| `.yaw(deg)` | Turns it about the world's up axis through its center: a head looking around. |
| `.orient(dir)` | Tilts it so its own z axis runs along `dir`: limbs, a tilted ear. Build a limb from A to B as `ellipsoid(mid, [r, r, len / 2]).orient(B - A)`. |
| `body.at([x, y, z])` | The world point at that spot of the body in its own axes (1 = its radius): attach ears, a snout, arms there and they follow every turn. |
| `body.decal(dir, [lon0, lat0, lon1, lat1], r)` | A patch painted on the surface around local direction `dir`, in degrees of longitude and latitude, rounded `r` degrees: masks, eyes, stripes, a nose bridge. It turns with the body and stops at its outline. |
| `.merge(id)` | Shapes pushed one after another with the same id draw as one body: a single outer outline, no seams where head, cheeks and snout meet. |
| `.color("#rrggbb")` | A color of its own for solids, bodies, decals and plates; its lit, mid and shaded tones are derived from it. Use a small palette of `let` colors at the top. |
| `shadow(c, [rx, ry], alpha)` | A soft contact shadow on the ground under `c` (soft style only): put one under everything that stands on something. |
| `.mid()` / `.dark()` | Gray and dark tones (fur, rubber) for bodies, solids and decals; decals are white by default. |

`iso/raccoon.rhai` is the full example: a merged head and body, decals for
the mask and eyes, a tail as a chain of merged balls, arms made with
`orient`, and a head that turns with `yaw`.

## Flat things on a face

Faces: `top(z)` (a = x, b = y), `left(y)` (a = x, b = z), `right(x)` (a = y, b = z).

| call | |
|---|---|
| `plate(face, [a0, b0, a1, b1])` / `plate(face, rect, r)` | Rounded rectangle; outline only unless filled. |
| `disc(face, [a, b], r)` | Circle; outline only unless filled. |
| `line([[x,y,z], ...])` | Polyline in the world (traces, ticks, rails). Muted by default. |

## Text

| call | |
|---|---|
| `label(from, length, "TEXT")` / `label(from, length, "TEXT", size)` | Callout: a dot at the world point `from`, a level leader `length` long, then the text (Geist Mono, size 9 default). A negative `length` puts the text on the left. |
| `.reveal(x)` | On a label: 0..1 of the leader drawn, growing from the text toward the thing; the dot lands at the end. Animate it after the text fades in so the eye follows the line. |
| `at_screen(sx, sy)` | The ground point that shows at screen (sx, sy): place captions and legends by where they appear. |
| `card(at, [w, h], r)` | A rounded panel flat on the screen from its top-left `at` (use `at_screen`), in the page color with a muted edge: what captions sit on so the drawing can pass behind them. |

## Camera

| call | |
|---|---|
| `camera(focus, zoom)` | Where the frame looks. At zoom 1 the view is the scene's resting view (whatever the focus); closer than that, `focus` moves toward the middle (fully by 1.4×). The world scales; labels and text keep their size and ride on their point; labels shorten their leader, or turn to the other side, to stay in frame. Push one per frame; the last one wins. |
| `.fixed()` | Keeps a shape still on screen when the camera moves: captions, cards, legends. |

The resting view is the bounds of the world, captions and labels (at their
shortest) over the loop. Blend camera shots between steps with weights that
crossfade slower than the captions, and make the first and last shot zoom 1.
| `text(at, "TEXT", size)` / `text(at, "TEXT", size, align)` | Text facing the viewer, vertically centered on `at`; `align` 0 left, 0.5 center, 1 right. |
| `text_width("TEXT", size)` | Width it sets at, for layout. |

Capitals get letter spacing; text with lowercase sets tight. Geist Mono has
accents, `¡ ¿ ñ`, `● ○ • · —` and box blocks, but not `■ ◆ ▪`.

## Look (chain them: `slab(...).accent().alpha(a)`)

| call | |
|---|---|
| `.accent()` | Accent tones (solids, plates) or solid accent (discs, text); outline turns accent. |
| `.dark()` | Dark tones with volume for solids (fur, rubber, a mask), and a dark fill for plates and discs. Works in both looks. |
| `.fill(name)` | `top` `left` `right` `recess` `accent_top` `accent_left` `accent_right` `ink` `mute` `accent`. Plates and discs only fill when given one (or `.accent()`). |
| `.ink(name)` | Outline: `edge` (full ink) `mute` `accent` `accent_mute` `none`. |
| `.side_ink(name)` | Outline of side and bottom edges only (a coin with a bright top and soft sides). |
| `.dashed()` | Dashed outline. |
| `.flow(x)` | Dashed outline whose dashes have travelled `x` dash periods along the line: `.flow(tau * 2)` makes them run the way the line was drawn (negative runs back). |
| `.alpha(a)` | Opacity 0..1; 0 draws nothing. |
| `.layer(n)` | Shapes pushed one after another with the same `n` fade as one picture. Give each object its own `n`. |
| `.clip_above(z)` / `.clip(min, max)` | Cut away what lies outside a world box: sinking, rising, sliding out of slots. Discs and text vanish whole. |

## Motion

| call | |
|---|---|
| `keyed(t, [[t0, v0, "ease"], [t1, v1], ...])` | Holds `v0` before `t0` and the last value after; between keys eases by the leaving key's easing (`"out"`, `"in_out"`, `"linear"` default). |
| `progress(t, a, b)` | 0 before `a`, 1 after `b`, linear between. |
| `ease(name, x)` | An easing at `x` (0..1). |
| `lerp(a, b, x)`, `clamp(x, lo, hi)` | |
| `centered(count, size, step, total)` | Where a row of `count` things `size` wide and `step` apart starts so it sits centered in `total`. Place every row of details with it. |
| `along(points, u)` | The point `u` (0..1) of the way along a polyline, by length: packets on traces, parcels on belts. |
| `sin`, `cos`, `abs`, `floor`, `PI()`, `%` | Rhai built-ins. A pulse: `keyed((t * 4.0) % 1.0, ...)`. |

The easings: `"out"` is `cubic-bezier(0.23, 1, 0.32, 1)`, `"in_out"` is
`cubic-bezier(0.77, 0, 0.175, 1)`.

## Patterns

**Something drops in and sinks.** `tube_back`, then the object with
`clip_above(floor)`, then `tube_front`. Move it with `keyed` from above to its
rest and later below the floor.

**A sheet prints out of a slot.** A recess `plate` on the top for the slot,
then the sheet (`slab` with depth ~1.2, `r` 0) rising from `z = top - height`
to `z = top` with `clip_above(top)`. Lines on it ride along.

**Lighting in turn.** A loop over the parts with `on = start + i * step`;
each gets `keyed(t, [[on, 0, "out"], [on + 0.05, 1], [off, 1, "in_out"], [off + 0.08, 0]])`
and draws an accent copy with that alpha and its own `layer`.

**A guided tour.** Split the loop into steps (`k / STEPS`), give each a
caption block (`text(at_screen(...))`, faded by a crossfading weight per step)
and labels that `reveal` inside their step; dim what a step is not about with
`.alpha(...)` on a `layer` per part. The last step must fade into the first
so the loop is seamless. `iso/ec2.rhai` is the full example.

**Packets on a bus.** Define each trace as a point list, draw it with
`line(...).ink("mute")`, and draw a small accent slab at
`along(trace, ease("in_out", progress(t, a, b)))` while 0 < u < 1, fading at
both ends.
