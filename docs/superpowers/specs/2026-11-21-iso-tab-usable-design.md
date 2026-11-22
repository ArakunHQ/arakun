# Make the Isometric tab usable

## Intent

Today the Isometric tab only plays scenes. You can pick a scene, scrub it,
switch between the Arakun and Paper looks, use Video or Guide mode, and copy
one frame as SVG. To make a new scene or change one, you have to leave the tab
and write Rhai, or get an agent to write it. The user's words: "it's only
playing things and really limited."

The goal is to make it a place where you make visuals, not just watch them.
The visuals end up in two places: exported into docs and READMEs, and shown
live inside Arakun. Plan for both.

This is the first of three projects. Next comes a rename of the tab, then
new kinds of visuals (diagrams, 3D). Leave both out of this one. Don't rename
anything here (`View::Isometric`, `iso/`, `arakun iso` and the skill stay
as they are).

## Goals

1. **New scene from the tab.** A "New" button opens a small prompt. You
   describe what you want, and the tab starts an agent session that writes a
   new `iso/<slug>.rhai` in the visible environment.
2. **Change a scene from the tab.** A "Change" button on the selected scene
   opens the same prompt. You describe the change, and an agent session edits
   that scene's file in place.
3. **Watch it appear.** The new scene is selected in the tab as soon as its
   file exists. The existing half-second folder watch reloads it on every save,
   so drafts show as the agent writes them. Broken drafts already show their
   error above the stage, so that needs nothing new.
4. **Export.** Buttons for PNG, SVG (the current frame) and animated SVG (the
   whole loop). Each opens a save dialog. Copy SVG stays.
5. **Starters.** More built-in scenes, so a project without `iso/` still
   shows good examples to start from or ask changes of.

## Non-goals

- A drag-and-drop shape editor. Scenes are code (loops, timing, animation),
  so dragging can't edit most of what's in them.
- Calling a model directly from the tab, or a chat UI inside the tab.
- The rename, new kinds of visuals (diagrams, 3D) and engine changes.
- Changing the `arakun iso` CLI.

## Anti-goals

- A second agent stack with its own API keys, models or history. Sessions
  started here must be ordinary Arakun sessions.
- Losing a scene. "Change" on a built-in scene must not write into Arakun
  itself. It works on a copy in the project (below).

## Approaches considered

1. **Start an ordinary agent session (picked by the user).** Use the existing
   `start_session_in` (`src/shell/sessions.rs`), the path automations and
   `arakun agent new` already take. Pros: reuses agents, models, settings,
   history, and the agent's ability to run `arakun iso render` and check its
   own work. You keep talking to it in a normal session. It's the least new
   code. Con: you watch a full session instead of a small prompt box.
2. **A prompt box that calls a model directly.** Tighter, but it duplicates
   the agent setup and API keys, and it can't render and look at its own
   output. Rejected.
3. **A visual shape editor.** A huge build that fights the code format.
   Rejected.

## Design

### Flow

- `IsoView` gets a prompt row (a `TextInput` from `src/input.rs`) that opens
  under the toolbar for "New" or "Change". Enter sends it and Escape closes it.
- `IsoView` doesn't own sessions, so it emits an event (`gpui::EventEmitter`)
  with the request: kind (new or change), the prompt text, and the target path.
  The shell subscribes when it creates `iso_view` (`src/shell.rs`, near line
  766) and calls `start_session_in` with the visible environment, the default
  agent, a short title ("Scene: <first words>"), and `show: true`.
- **New:** the tab picks the path before starting, as
  `iso/<slug-of-prompt>.rhai`, adding `-2`, `-3` if that name is taken.
  It remembers that path as "pending". When a reload finds it, the tab
  selects it once and clears the pending path.
- **Change, script scene:** the target is that script's own path.
- **Change, built-in scene:** first copy its source into
  `iso/<slug-of-name>.rhai` (a script with the same name already replaces the
  built-in, as `reload` does today), then treat it like a script scene. This
  only works if built-ins have Rhai source (see Starters).
- **Change, broken scene:** allowed. The prompt includes the error.

### The prompt the agent gets

Plain text built by the tab. It holds:

- the user's words
- "Use the arakun-iso skill"
- the target path, and whether that file is new or one to edit in place
- for Change: keep the file name, check it with `arakun iso check`, and look
  at it with `arakun iso render` before finishing
- for a broken scene: the current error

Keep it short. The skill already carries the how.

### Where the session shows

The session opens as a tab with `show: true`. Putting it in a split beside the
stage would be nicer, but it isn't required. Use the existing pane split if
it's cheap, otherwise a plain tab is fine (builder's call).

### Export

- PNG: rasterize the current frame with the current look, the way
  `arakun iso render` does (`src/iso/cli.rs`, `raster`). Default width 1600.
- SVG: what Copy SVG builds today (`svg::frame`), written to a file.
- Animated SVG: `anim::loop_svg` over the whole loop, as `arakun iso anim`
  does, with the current look. This can take a while, so run it on the
  background executor and show "Exporting…" on the button.
- The save dialog: `prompt_for_new_path`, suggesting `<scene-slug>.png` /
  `.svg` in the project root.
- Move the shared pieces out of `cli.rs` into functions both the CLI and the
  tab call. Don't copy them.

### Starters

Built-in scenes today are Rust (`src/iso/scenes.rs`, two scenes), so they
have no Rhai source to copy for Change. Add starters as Rhai files embedded
with `include_str!` and loaded through the same path as project scripts.
Take them from the repo's `iso/` folder: `billing`, `cpu`, `ec2`,
`raccoon`.

Order and names (my call, settled after the builder check):

- The list order is project scripts, then starters, then Rust built-ins.
  Match by scene name, as `reload` does today. A project script hides a
  starter or built-in with the same name, and a starter hides a Rust built-in
  with the same name.
- The `billing` starter has the same name ("Usage billing") as the Rust
  billing scene, so it replaces that scene. Delete the Rust billing scene so
  there's one source. "Primitives" stays in Rust, without Change; that button
  is disabled for it, with a tooltip.
- Starters show in every project, as the built-ins do today. A project that
  has its own copy (like this repo's `iso/`) shows only its own, by the name
  rule above. Change on a starter is only reachable when no project script
  has its name. If its copy path already exists under another name, pick
  the next free slug (`-2`, …).

### Interface text

Use the glossary words (session, environment). The button labels are "New
scene", "Change", "Export PNG", "Export SVG" and "Export animated SVG". Fewer
buttons via a menu is fine, but that's the builder's call. Use the design
system only (`theme`, `ui`), so `design_lint` passes.

### Docs

Update the README's Isometric section with the new buttons, and add a line
on New and Change to the `arakun-iso` skill so agents know where these
prompts come from.

## Left to the builder

- Exact layout of the prompt row and buttons. Use the `ui-change` skill and
  take screenshots.
- Slug rules.
- Tests: slug and unique-path picking, prompt text, embedded starters that
  load and pass the same checks as `arakun iso check`, and export functions
  that return non-empty output.
