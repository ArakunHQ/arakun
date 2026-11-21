# Usable Isometric tab: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Isometric tab can create scenes (New scene) and change them
(Change) through ordinary agent sessions, export PNG, SVG and animated SVG,
and ships Rhai starter scenes.

**Architecture:** The tab keeps no agent logic. It builds a prompt and emits
an `IsoEvent`, and the shell starts a session with `start_session_in`. The
export code moves out of `src/iso/cli.rs` into `src/iso/export.rs`, which the
CLI and the tab both call. Starters are `iso/*.rhai` files embedded with
`include_str!` and compiled with a new `ScriptScene::from_source`.

**Tech Stack:** Rust, GPUI, Rhai, tiny-skia.

**Spec:** `docs/superpowers/specs/2026-11-21-iso-tab-usable-design.md`

## Global Constraints

- Don't rename anything: `View::Isometric`, `iso/`, `arakun iso` and the
  `arakun-iso` skill stay as they are.
- Leave the `arakun iso` CLI's flags and output unchanged. Only its insides
  move.
- Sessions are ordinary Arakun sessions, started by `start_session_in` with
  `crate::model::default_agent()` and `show: true`. No direct model calls.
- "Change" never writes inside Arakun's own source. A starter is copied into
  the project's `iso/` first.
- Labels: "New scene", "Change", "Export PNG", "Export SVG", "Export
  animated SVG". Interface words follow the glossary (session, environment).
- Take every color and text size from `theme` / `ui`, or `design_lint`
  fails. Retired words fail `words_lint`.
- Tests go in `#[cfg(test)] mod tests` at the bottom of each file.
- Before each commit, run `tools/rustfmt.sh --check`, then
  `cargo clippy --locked --all-targets --no-default-features -- -D warnings`,
  then `cargo test --locked --no-default-features iso`.
- Commit titles follow Conventional Commits. Add no Claude attribution lines.

## Review Focus

1. **Prompt with no letters or digits** ("🙂", "!!!") gives an empty slug.
   Expected: the file is `iso/scene.rhai` (then `scene-2`…), never `iso/.rhai`.
   Pinned in Task 3: `slug_falls_back_to_scene`.
2. **Two New scene requests in a row with the same words** must not hit the
   same file. Expected: the second gets `-2`. Pinned in Task 3:
   `free_path_skips_taken_names`.
3. **The visible environment changes while a New scene request is
   pending.** Expected: the pending path belongs to the old root and is
   dropped on `set_root`, so the tab never jumps to a stray scene. Pinned in
   Task 4: `set_root_clears_pending` (a pure helper test, described there).
4. **Export of a broken scene, or a scene that draws nothing.** Expected:
   the buttons are disabled for `Entry::Broken`, and an export error shows
   in the button's place instead of panicking. Pinned in Task 2:
   `png_errors_on_empty_scene`.
5. **Starter scripts go stale as the engine changes.** Expected: a test
   fails when a starter stops compiling or throws mid-loop. Pinned in Task 1:
   `starters_run_whole_loop`.

---

### Task 1: Embedded Rhai starters, with the Rust billing scene removed

**Files:**
- Modify: `src/iso/script.rs:47-79` (split `load`)
- Create: `src/iso/starters.rs`
- Modify: `src/iso/mod.rs:13-19` (add `pub mod starters;`)
- Modify: `src/iso/scenes.rs` (delete the `billing` entry, `fn billing`
  and the `billing_loops_seamlessly` test, plus any helpers that only
  `billing` used)

**Interfaces:**
- Produces: `ScriptScene::from_source(path: &Path, source: &str) -> Result<ScriptScene, String>`.
  `load` reads the file and calls it.
- Produces: `pub struct Starter { pub file: &'static str, pub source: &'static str }`
  and `pub const ALL: &[Starter]` in `starters.rs`, in this order:
  `billing.rhai`, `cpu.rhai`, `ec2.rhai`, `raccoon.rhai`, each
  `include_str!("../../iso/<file>")`.
- Produces: `pub fn load(s: &Starter) -> Result<ScriptScene, String>`,
  which calls `from_source(Path::new(s.file), s.source)`. Its `path` is
  the bare file name, which marks it as not on disk.

- [ ] **Step 1: Write the failing tests in `starters.rs`**

```rust
#[test]
fn starters_load_with_their_names() {
    let names: Vec<String> = ALL.iter().map(|s| load(s).unwrap().meta.name).collect();
    assert_eq!(names, ["Usage billing", "CPU", "EC2 instances", "Raccoon"]);
}

#[test]
fn starters_run_whole_loop() {
    for s in ALL {
        let scene = load(s).unwrap();
        for i in 0..60 {
            let t = i as f32 / 60.;
            assert!(!scene.frame(t).unwrap().is_empty(), "{} at {t}", s.file);
        }
    }
}

#[test]
fn no_rust_scene_shares_a_starter_name() {
    for s in super::scenes::ALL {
        assert!(ALL.iter().all(|st| load(st).unwrap().meta.name != s.name));
    }
}
```

- [ ] **Step 2: Run** `cargo test --locked --no-default-features iso::starters`.
  It should fail to compile, because `starters` doesn't exist yet.
- [ ] **Step 3: Implement `from_source`, `starters.rs`, and the `scenes.rs` deletion.**
  `svg.rs` and `raster.rs` tests use `scenes::ALL[0]`, which is
  "Primitives" after the deletion. Leave them as they are, unless one
  asserts something specific to billing.
- [ ] **Step 4: Run** `cargo test --locked --no-default-features iso`. It should pass.
- [ ] **Step 5: Commit** `feat(iso): ship the example scenes as built-in Rhai starters`

---

### Task 2: Shared export functions

**Files:**
- Create: `src/iso/export.rs`
- Modify: `src/iso/mod.rs` (add `pub mod export;`)
- Modify: `src/iso/cli.rs` (`frame_pixmap`, `svg`, `anim` and `save` call into `export`)

**Interfaces:**
- Produces: `pub type FrameFn<'a> = &'a dyn Fn(f32, f32) -> Result<Vec<Node>, String>;`.
  It takes `(t, tau)` and returns nodes, so `Entry::nodes` and
  `ScriptScene::frame_at` both fit it.
- Produces: `pub fn png(frame: FrameFn, ext: Rect, t: f32, tau: f32, pal: &Palette, width: u32) -> Result<Vec<u8>, String>`.
  The body is `frame_pixmap`'s, moved from `cli.rs` and encoded with
  `Pixmap::encode_png`.
- Produces: `pub fn pixmap(frame: FrameFn, ext: Rect, t: f32, tau: f32, pal: &Palette, width: u32) -> Result<Pixmap, String>`.
  The CLI keeps using it for sheets and stamps.
- Produces: `pub fn svg(name: &str, frame: FrameFn, ext: Rect, t: f32, tau: f32, pal: &Palette) -> Result<String, String>`.
  It wraps `shoot` and `svg::frame(name, items, pal, 16.)`.
- Produces: `pub fn loop_svg(name: &str, frame: FrameFn, ext: Rect, seconds: f32, pal: &Palette, fps: f32, px: f32) -> Result<String, String>`.
  The sampling loop moves from `cli::anim`, without `--still`, which stays
  in the CLI. Errors read `at t {t:.3}: {e}`, as they do today.

- [ ] **Step 1: Write the failing tests in `export.rs`**, using
  `crate::iso::starters::load(&starters::ALL[1])` (CPU) as the scene and
  the paper palette:

```rust
#[test]
fn png_is_a_png() { let b = png(&f, ext, 0., 0., &pal, 400).unwrap(); assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n"); }
#[test]
fn svg_frame_has_paths() { assert!(svg("CPU", &f, ext, 0.3, 1.5, &pal).unwrap().contains("<path")); }
#[test]
fn loop_svg_animates() { assert!(loop_svg("CPU", &f, ext, 5., &pal, 24., 0.5).unwrap().contains("<animate")); }
#[test]
fn png_errors_on_empty_scene() { let empty = |_: f32, _: f32| Ok(Vec::new()); assert!(png(&empty, [0.,0.,0.,0.], 0., 0., &pal, 400).is_err()); }
```

  (`f` is `|t, tau| scene.frame_at(t, tau)`. `ext` is computed the way
  `cli::extent` does it.)
- [ ] **Step 2: Run** `cargo test --locked --no-default-features iso::export`. It should fail to compile.
- [ ] **Step 3: Implement `export.rs` and switch `cli.rs` to it.** Move
  `extent` too, as `pub fn extent(frame: FrameFn, seconds: f32) -> Result<Rect, String>`.
  If a zero-size `ext` doesn't already make `Fit::new` return `None`, make
  `png` return an error for it.
- [ ] **Step 4: Run** `cargo test --locked --no-default-features iso`. It should
  pass. Then check the CLI by hand. Both commands below should print a path
  and write a file:
  `cargo run --no-default-features -- iso render iso/cpu.rhai --out /tmp/c.png`
  and `cargo run --no-default-features -- iso anim iso/cpu.rhai --out /tmp/c.svg`.
- [ ] **Step 5: Commit** `refactor(iso): share the export code between the CLI and the tab`

---

### Task 3: Slugs, free paths and the agent prompt

**Files:**
- Create: `src/iso/ask.rs`
- Modify: `src/iso/mod.rs` (add `pub mod ask;`)

**Interfaces:**
- Produces: `pub fn slug(text: &str) -> String`. The rules: lowercase ASCII
  letters and digits; every other run becomes `-`; trim the `-`; keep at
  most the first 5 words and at most 40 characters; if the result is
  empty, `"scene"`.
- Produces: `pub fn free_path(dir: &Path, base: &str) -> PathBuf`. It
  returns `dir/<base>.rhai`, or `dir/<base>-2.rhai`, `-3`… for the first
  name not taken on disk.
- Produces:
  ```rust
  pub enum Ask {
      New { path: PathBuf, words: String },
      Change { path: PathBuf, words: String, error: Option<String>, copied_from: Option<String> },
  }
  pub fn prompt(ask: &Ask) -> String;
  pub fn title(ask: &Ask) -> String;   // "Scene: " + first 6 words of `words`
  ```
- `prompt` text (fixed copy; `{path}` is relative to the environment root):
  - New: `"{words}\n\nUse the arakun-iso skill. Write this as a new scene in {path}. Check it with `arakun iso check {path}` and look at it with `arakun iso render {path}` before you finish."`
  - Change: `"{words}\n\nUse the arakun-iso skill. Change the scene in {path} in place and keep its file name. Check it with `arakun iso check {path}` and look at it with `arakun iso render {path}` before you finish."`
    Append `"\n\nIt is a copy of the built-in \"{name}\" scene."` when
    `copied_from` is set, and `"\n\nIt fails to load right now:\n{error}"`
    when `error` is set.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn slug_basic() { assert_eq!(slug("Our Deploy pipeline, as a diagram!"), "our-deploy-pipeline-as-a"); }
#[test] fn slug_falls_back_to_scene() { assert_eq!(slug("🙂 !!!"), "scene"); }
#[test] fn slug_caps_length() { assert!(slug(&"a".repeat(100)).len() <= 40); }
#[test] fn free_path_skips_taken_names() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("cpu.rhai"), "").unwrap();
    assert_eq!(free_path(d.path(), "cpu"), d.path().join("cpu-2.rhai"));
    assert_eq!(free_path(d.path(), "db"), d.path().join("db.rhai"));
}
#[test] fn change_prompt_carries_error_and_origin() {
    let p = prompt(&Ask::Change { path: "iso/cpu.rhai".into(), words: "bigger".into(), error: Some("line 3: oops".into()), copied_from: Some("CPU".into()) });
    assert!(p.starts_with("bigger") && p.contains("in place") && p.contains("line 3: oops") && p.contains("\"CPU\""));
}
#[test] fn title_is_short() { assert_eq!(title(&Ask::New { path: "iso/x.rhai".into(), words: "one two three four five six seven".into() }), "Scene: one two three four five six"); }
```

  If `tempfile` isn't already a dev-dependency (check `Cargo.toml`), use a
  directory under `std::env::temp_dir()` named after the test instead.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features iso::ask`. It should fail to compile.
- [ ] **Step 3: Implement `ask.rs`.**
- [ ] **Step 4: Run** `cargo test --locked --no-default-features iso::ask`. It should pass.
- [ ] **Step 5: Commit** `feat(iso): build the prompts for new and changed scenes`

---

### Task 4: New scene and Change in the tab, started as sessions

**Files:**
- Modify: `src/iso_view.rs` (an `Entry::Starter` variant, reload order,
  the prompt row, pending selection, `IsoEvent`)
- Modify: `src/shell.rs:766` (subscribe to `iso_view`, the same way
  `memory_sub` does it, and keep the subscription with the others)
- Modify: `README.md:425` area (one or two sentences on New scene, Change
  and Export)
- Modify: `.claude/skills/arakun-iso/SKILL.md` (one paragraph: the tab's
  New scene and Change start a session with a prompt naming the file to
  write; write exactly that file)

**Interfaces:**
- Consumes: `starters::{ALL, Starter, load}` (Task 1) and
  `ask::{Ask, prompt, title, slug, free_path}` (Task 3).
- Produces: `pub enum IsoEvent { Ask(String /*title*/, String /*prompt*/) }`
  and `impl EventEmitter<IsoEvent> for IsoView`.
- Produces: `Entry::Starter(&'static Starter, ScriptScene)`. Its `name`,
  `about`, `seconds`, `steps`, `soft`, `look` and `nodes` all behave like
  `Entry::Script`. `path()` returns `None`.
- Produces: `fn order(scripts: Vec<Entry>, starters: Vec<Entry>, builtins: Vec<Entry>) -> Vec<Entry>`.
  It's a free function and pure: it keeps the order given and drops any
  later entry whose `name()` an earlier entry already has. `reload` calls
  it.
- Produces: `fn pending_after_root_change(pending: Option<PathBuf>, old: Option<&Path>, new: &Path) -> Option<PathBuf>`.
  It's a free function and pure: it keeps `pending` only when
  `old == Some(new)`. `set_root` calls it.

Behavior:
- In the toolbar row, put the "New scene" button (Outline) and the
  "Change" button. Change is disabled, with a tooltip saying "This scene
  is part of Arakun and has no script to change", when the selected entry
  is `Entry::Builtin`.
- Clicking either one opens a single `TextInput` row under the toolbar,
  with a placeholder: "Describe the scene" for New, "Describe the change"
  for Change. `InputEvent::Submit` with non-blank text sends the request.
  `Cancel` closes the row. Sending also clears and closes it.
- **New:** `path = ask::free_path(root/iso, &ask::slug(words))`. Create
  `root/iso` if it's missing. Store `pending = Some(path)`. Emit
  `IsoEvent::Ask(title, prompt)`. In `reload`, if an entry's `path()`
  equals `pending`, select it and clear `pending`.
- **Change:** for `Entry::Script` and `Entry::Broken`, the target is its
  path, and Broken passes `error`. For `Entry::Starter`, write
  `starter.source` to `free_path(root/iso, &slug(name))` first, then use
  `copied_from: Some(name)` and set `pending` to that path. The new script
  has the same name, so `order` hides the starter.
- The shell handler is `IsoEvent::Ask(title, text)`. It calls
  `this.start_session_in(&this.env().path.clone(), crate::model::default_agent(), title.into(), text.clone(), true, window, cx)`.
  Opening the session as a plain tab is enough; leave out the split.

- [ ] **Step 1: Write the failing tests in `iso_view.rs`**

```rust
#[test] fn order_hides_later_same_names() {
    // Build Entry::Broken values with distinct paths whose file stems are the names: "a", "b", "a".
    let names: Vec<String> = order(vec![broken("x/a.rhai")], vec![broken("s/b.rhai"), broken("s/a.rhai")], vec![]).iter().map(Entry::name).collect();
    assert_eq!(names, ["a", "b"]);
}
#[test] fn set_root_clears_pending() {
    let p = Some(PathBuf::from("/r1/iso/x.rhai"));
    assert_eq!(pending_after_root_change(p.clone(), Some(Path::new("/r1")), Path::new("/r2")), None);
    assert_eq!(pending_after_root_change(p.clone(), Some(Path::new("/r1")), Path::new("/r1")), p);
}
```

- [ ] **Step 2: Run** `cargo test --locked --no-default-features iso_view`. It should fail to compile.
- [ ] **Step 3: Implement the view changes, the shell subscription and the docs.**
- [ ] **Step 4: Run the tests.** `cargo test --locked --no-default-features iso_view`
  should pass. Then run `cargo test --locked --no-default-features design_lint` and
  `cargo test --locked --no-default-features words_lint`, which should also pass.
- [ ] **Step 5: Check it in the app** with the `ui-change` skill (`arakun dev`).
  Open the Isometric tab, take a screenshot of the toolbar, then open the
  prompt row and take a screenshot of that. Send "a small server rack", and
  confirm that a session tab opens and the prompt names `iso/a-small-server-rack.rhai`.
- [ ] **Step 6: Commit** `feat(iso): create and change scenes from the Isometric tab`

---

### Task 5: Export buttons

**Files:**
- Modify: `src/iso_view.rs` (replace the single Copy SVG area with Copy
  SVG plus three export buttons, or one "Export" button that opens those
  three as a menu if the toolbar gets crowded; that's the builder's call,
  but keep the labels)

**Interfaces:**
- Consumes: `export::{png, svg, loop_svg}` (Task 2) and `ask::slug` (Task 3).
- Produces: `enum Exporting { Idle, Busy(&'static str), Failed(String, Instant) }`
  on `IsoView`.

Behavior:
- Each button calls `cx.prompt_for_new_path(root, Some("<slug(name)>.png" | ".svg" | "-loop.svg"))`,
  the way `src/browser/view.rs:728` does. Nothing happens on cancel.
- PNG: `export::png` at the current `t`/`tau`, with the current look and
  `width = 1600`. SVG: `export::svg`. Animated SVG: `export::loop_svg(..., fps = 240., px = 0.1)`.
  Run all three on `cx.background_spawn`. Build the frame closure from
  `Entry` data that can be sent between threads. If `ScriptScene` isn't
  `Send` (Rhai's `Engine` usually isn't), re-create it on the background
  thread from its path, or from the starter source, and use that.
- While an export runs, its button reads "Exporting…" and every export
  button is disabled. On error, the button reads the error for 4 s
  (`Failed`), then goes back to its label.
- All export buttons are disabled for `Entry::Broken`.

- [ ] **Step 1: Write the failing test.** The only pure piece here is the
  file name, so test it:

```rust
#[test] fn export_names() { assert_eq!(export_name("EC2 instances", Kind::Loop), "ec2-instances-loop.svg"); assert_eq!(export_name("CPU", Kind::Png), "cpu.png"); }
```

  This uses `fn export_name(scene: &str, kind: Kind) -> String` and
  `enum Kind { Png, Svg, Loop }`, both in `iso_view.rs`.
- [ ] **Step 2: Run** `cargo test --locked --no-default-features iso_view::tests::export_names`. It should fail.
- [ ] **Step 3: Implement the buttons and the background export.**
- [ ] **Step 4: Run** `cargo test --locked --no-default-features iso_view` and
  `cargo test --locked --no-default-features design_lint`. Both should pass.
- [ ] **Step 5: Check it in the app** with the `ui-change` skill. Export
  CPU in each format and open the files. The PNG should be 1600 px wide,
  and the animated SVG should play in a browser. Take a screenshot of the
  toolbar.
- [ ] **Step 6: Commit** `feat(iso): export scenes as PNG, SVG and animated SVG from the tab`
