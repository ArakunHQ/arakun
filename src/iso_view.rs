//! The Isometric tab: the scenes of `iso` playing live, with a timeline to
//! scrub, a light "paper" look next to Arakun's own, and Copy SVG for the
//! frame on screen.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Pixels, Render, RenderImage,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, canvas, div, px,
};

use crate::input::{InputEvent, TextInput};
use crate::iso::ask::{self, Ask};
use crate::iso::starters::{self, Starter};

use crate::iso::raster::{self, Fit};
use crate::iso::script::{self, ScriptScene};
use crate::iso::svg::{Palette, Rgb};
use crate::iso::{self, Item, Node, Rect, scenes};
use crate::theme;
use crate::ui::{self, ButtonKind, Type};

/// The look a scene is painted in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    /// Arakun's night blue, its text color and amber.
    Arakun,
    /// White paper, near-black ink and teal, like a docs page.
    Paper,
}

impl Look {
    fn palette(self) -> Palette {
        match self {
            Look::Arakun => Palette {
                ink: rgb(theme::text()),
                ground: rgb(theme::background()),
                accent: rgb(theme::accent()),
                dark: true,
                soft: false,
            },
            Look::Paper => Palette {
                ink: [0.165, 0.165, 0.165],
                ground: [1., 1., 1.],
                accent: [0.114, 0.686, 0.612],
                dark: false,
                soft: false,
            },
        }
    }
}

fn rgb(c: Hsla) -> Rgb {
    let c = c.to_rgb();
    [c.r, c.g, c.b]
}

fn hsla(c: Rgb, alpha: f32) -> Hsla {
    gpui::Rgba {
        r: c[0],
        g: c[1],
        b: c[2],
        a: alpha,
    }
    .into()
}

/// A scene the tab can play: one of `iso::scenes`, a starter that ships
/// with Arakun, a script from the project's `iso/` folder, or a script that
/// failed to load.
// A handful per tab; boxing the script scenes would buy nothing.
#[allow(clippy::large_enum_variant)]
enum Entry {
    Builtin(&'static scenes::Scene),
    Script(ScriptScene),
    /// Compiled and measured once per tab (`starter_cache`), not per reload.
    Starter(&'static Starter, Rc<ScriptScene>, Rect),
    Broken {
        path: PathBuf,
        error: String,
    },
}

impl Entry {
    /// The compiled script behind a project or starter scene.
    fn script(&self) -> Option<&ScriptScene> {
        match self {
            Entry::Script(s) => Some(s),
            Entry::Starter(_, s, _) => Some(s),
            _ => None,
        }
    }

    fn name(&self) -> String {
        match self {
            Entry::Builtin(s) => s.name.into(),
            Entry::Script(_) | Entry::Starter(..) => self
                .script()
                .map_or_else(String::new, |s| s.meta.name.clone()),
            Entry::Broken { path, .. } => path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        }
    }

    fn about(&self) -> String {
        match self {
            Entry::Builtin(s) => s.about.into(),
            Entry::Script(_) | Entry::Starter(..) => self
                .script()
                .map_or_else(String::new, |s| s.meta.about.clone()),
            Entry::Broken { .. } => String::new(),
        }
    }

    fn seconds(&self) -> f32 {
        match self {
            Entry::Builtin(s) => s.seconds,
            Entry::Script(_) | Entry::Starter(..) => self.script().map_or(1., |s| s.meta.seconds),
            Entry::Broken { .. } => 1.,
        }
    }

    fn path(&self) -> Option<&PathBuf> {
        match self {
            Entry::Builtin(_) | Entry::Starter(..) => None,
            Entry::Script(s) => Some(&s.path),
            Entry::Broken { path, .. } => Some(path),
        }
    }

    /// The steps of a guided scene and where in each the guide stops.
    fn steps(&self) -> Option<(u32, f32)> {
        self.script()
            .filter(|s| s.meta.steps > 0)
            .map(|s| (s.meta.steps, s.meta.hold))
    }

    /// Whether it is drawn in the soft style.
    fn soft(&self) -> bool {
        self.script().is_some_and(|s| s.meta.soft)
    }

    fn look(&self) -> Option<Look> {
        match self.script()?.meta.look.as_deref() {
            Some("paper") => Some(Look::Paper),
            Some("arakun") => Some(Look::Arakun),
            _ => None,
        }
    }

    fn nodes(&self, t: f32, tau: f32) -> Result<Vec<Node>, String> {
        match self {
            Entry::Builtin(s) => Ok((s.build)(t)),
            Entry::Script(s) => s.frame_at(t, tau),
            Entry::Starter(_, s, _) => s.frame_at(t, tau),
            Entry::Broken { error, .. } => Err(error.clone()),
        }
    }

    /// The bounds over the whole loop, so the fit holds still.
    fn extent(&self) -> Rect {
        match self {
            Entry::Starter(_, _, ext) => *ext,
            _ => self.measure(),
        }
    }

    /// Runs the loop to find its bounds.
    fn measure(&self) -> Rect {
        iso::rest_bounds((0..=48).filter_map(|i| {
            let t = i as f32 / 48.;
            self.nodes(t, t * self.seconds()).ok()
        }))
        .unwrap_or([0., 0., 1., 1.])
    }
}

/// The `.rhai` files of a folder and when each last changed.
fn signature(dir: &std::path::Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rhai"))
        .map(|p| {
            let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            (p, m)
        })
        .collect();
    out.sort();
    out
}

pub struct IsoView {
    entries: Vec<Entry>,
    extents: Vec<Rect>,
    scene: usize,
    /// The project whose `iso/` folder is listed.
    root: Option<PathBuf>,
    /// The scripts as last loaded; a change on disk reloads them.
    loaded: Vec<(PathBuf, Option<SystemTime>)>,
    /// Bumped on every reload, so no image of an old script is reused.
    generation: u64,
    /// Where in the loop, 0..1.
    t: f32,
    /// The ambient clock, in seconds: it runs while the video plays or the
    /// guide is on, also when the guide holds the story still. Paused, it
    /// stops.
    tau: f32,
    playing: bool,
    /// Step by step instead of a video: the guide's place and where it is
    /// headed, on an unwrapped clock (whole loops count), so going on from
    /// the last step runs into the first.
    guide: Option<Guide>,
    last_frame: Option<Instant>,
    look: Look,
    timeline: Rc<Cell<Option<Bounds<Pixels>>>>,
    scrubbing: bool,
    copied: Option<Instant>,
    shown: Shown,
    focus: FocusHandle,
    /// The New scene or Change prompt, while it is open.
    asking: Option<Asking>,
    /// Scene files asked for and not written yet; the first of them to
    /// appear is selected.
    pending: Vec<PathBuf>,
    /// Why the last request could not be sent.
    ask_error: Option<String>,
    exporting: Exporting,
    /// The starters, compiled and measured once: they never change while
    /// Arakun runs, and every save in `iso/` reloads the list.
    starters: Vec<CachedStarter>,
    _watch: gpui::Task<()>,
}

/// The open prompt row: whether it changes the scene on screen, and its input.
struct Asking {
    /// For Change, the scene on screen when the row opened (its file, or
    /// its name when it has none): picking another scene meanwhile must
    /// not move the request to it.
    change: Option<(Option<PathBuf>, String)>,
    input: Entity<TextInput>,
    _sub: Subscription,
}

/// What the tab asks the shell for: a session with this title and first prompt.
pub enum IsoEvent {
    Ask { title: String, prompt: String },
}

impl EventEmitter<IsoEvent> for IsoView {}

impl IsoView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Saving a scene shows it: the folder is checked twice a second.
        let watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let alive = this.update(cx, |v, cx| {
                    if let Some(root) = &v.root
                        && signature(&root.join("iso")) != v.loaded
                    {
                        v.reload();
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut view = Self {
            entries: Vec::new(),
            extents: Vec::new(),
            scene: 0,
            root: None,
            loaded: Vec::new(),
            generation: 0,
            t: 0.,
            playing: true,
            tau: 0.,
            guide: None,
            last_frame: None,
            look: Look::Arakun,
            timeline: Rc::default(),
            scrubbing: false,
            copied: None,
            shown: Rc::default(),
            focus: cx.focus_handle(),
            asking: None,
            pending: Vec::new(),
            ask_error: None,
            exporting: Exporting::Idle,
            starters: starter_cache(),
            _watch: watch,
        };
        view.reload();
        view
    }

    /// Lists the scenes of `root`'s `iso/` folder ahead of the built-in ones.
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            // Requests belong to the environment they were made in.
            self.pending.clear();
            self.asking = None;
            self.root = Some(root);
            self.reload();
            cx.notify();
        }
    }

    fn reload(&mut self) {
        let current = self
            .entries
            .get(self.scene)
            .map(|e| (e.path().cloned(), e.name()));
        let mut scripts = Vec::new();
        self.loaded = Vec::new();
        if let Some(root) = &self.root {
            let dir = root.join("iso");
            self.loaded = signature(&dir);
            for loaded in script::load_dir(&dir) {
                scripts.push(match loaded {
                    Ok(s) => Entry::Script(s),
                    Err((path, error)) => Entry::Broken { path, error },
                });
            }
        }
        let starters = starter_entries(&self.starters);
        let builtins = scenes::ALL.iter().map(Entry::Builtin).collect();
        // A script of the same name replaces the starter or built-in scene.
        let entries = order(scripts, starters, builtins);
        self.extents = entries.iter().map(Entry::extent).collect();
        // Stay on the scene that was showing, found by file or by name.
        self.scene = current
            .and_then(|(path, name)| find_entry(&entries, path.as_deref(), &name))
            .unwrap_or(0);
        // Scenes asked for have appeared: show the first, once.
        let paths: Vec<Option<&Path>> = entries
            .iter()
            .map(|e| e.path().map(PathBuf::as_path))
            .collect();
        let asked = take_arrived(&mut self.pending, &paths);
        self.entries = entries;
        self.generation += 1;
        if let Some(i) = asked {
            self.select(i);
        }
    }

    /// Opens the prompt row for a new scene, or for a change to this one.
    fn open_ask(&mut self, change: bool, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = if change {
            "Describe the change"
        } else {
            "Describe the scene"
        };
        let input = cx.new(|cx| TextInput::new(placeholder, cx));
        let sub = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| match ev {
            InputEvent::Submit => {
                let words = input.read(cx).text().trim().to_string();
                if !words.is_empty() {
                    this.send_ask(words, cx);
                }
            }
            InputEvent::Cancel => {
                this.asking = None;
                cx.notify();
            }
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        let change = change
            .then(|| self.entries.get(self.scene))
            .flatten()
            .map(|e| (e.path().cloned(), e.name()));
        self.asking = Some(Asking {
            change,
            input,
            _sub: sub,
        });
        self.ask_error = None;
        cx.notify();
    }

    /// Works out the file, then asks the shell for a session that writes it.
    fn send_ask(&mut self, words: String, cx: &mut Context<Self>) {
        let change = self.asking.as_ref().and_then(|a| a.change.clone());
        match self.ask_for(words, change) {
            Ok(ask) => {
                self.asking = None;
                self.ask_error = None;
                cx.emit(IsoEvent::Ask {
                    title: ask::title(&ask),
                    prompt: ask::prompt(&ask),
                });
            }
            Err(e) => self.ask_error = Some(e),
        }
        cx.notify();
    }

    fn ask_for(
        &mut self,
        words: String,
        change: Option<(Option<PathBuf>, String)>,
    ) -> Result<Ask, String> {
        let root = self.root.clone().ok_or("No environment is open")?;
        let dir = root.join("iso");
        let rel = |p: &Path| p.strip_prefix(&root).unwrap_or(p).to_path_buf();
        let mkdir = || std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()));
        let Some((path, name)) = change else {
            mkdir()?;
            let path = ask::free_path_except(&dir, &ask::slug(&words), &self.pending);
            self.pending.push(path.clone());
            return Ok(Ask::New {
                path: rel(&path),
                words,
            });
        };
        let index = find_entry(&self.entries, path.as_deref(), &name)
            .ok_or("That scene is gone; pick it again")?;
        match self.entries.get(index) {
            Some(Entry::Script(s)) => Ok(Ask::Change {
                path: rel(&s.path),
                words,
                error: None,
                copied_from: None,
            }),
            Some(Entry::Broken { path, error }) => Ok(Ask::Change {
                path: rel(path),
                words,
                error: Some(error.clone()),
                copied_from: None,
            }),
            // Never edited where it ships: the project gets its own copy.
            Some(Entry::Starter(st, s, _)) => {
                mkdir()?;
                let name = s.meta.name.clone();
                let path = ask::free_path_except(&dir, &ask::slug(&name), &self.pending);
                std::fs::write(&path, st.source).map_err(|e| format!("{}: {e}", path.display()))?;
                self.pending.push(path.clone());
                Ok(Ask::Change {
                    path: rel(&path),
                    words,
                    error: None,
                    copied_from: Some(name),
                })
            }
            Some(Entry::Builtin(_)) | None => Err("This scene has no script to change".into()),
        }
    }

    fn select(&mut self, i: usize) {
        self.scene = i;
        self.t = 0.;
        self.last_frame = None;
        self.guide = None;
        if let Some(look) = self.entries.get(i).and_then(Entry::look) {
            self.look = look;
        }
    }

    /// Turns the guide on (from where the video is) or off.
    fn set_guide(&mut self, on: bool) {
        let steps = self.entries.get(self.scene).and_then(Entry::steps);
        match (on, steps) {
            (true, Some((n, hold))) => {
                let n = n as f32;
                let step = (self.t * n).floor();
                let stop = (step + hold) / n;
                // Past this step's stop already: settle there without
                // playing backwards.
                self.guide = Some(Guide {
                    at: self.t.min(stop),
                    to: stop,
                });
            }
            _ => {
                self.guide = None;
                self.playing = true;
            }
        }
        self.last_frame = None;
    }

    /// Goes on to the next stop, or back to the previous step. On: the rest
    /// of this step goes by quickly, then the next plays in full. Back: the
    /// previous step starts over and plays to its stop, never in reverse.
    fn guide_step(&mut self, by: i32) {
        let Some((n, hold)) = self.entries.get(self.scene).and_then(Entry::steps) else {
            return;
        };
        let n = n as f32;
        if let Some(g) = &mut self.guide {
            let step = (g.to * n).floor() + by as f32;
            g.to = (step + hold) / n;
            if by < 0 {
                g.at = step / n;
            }
        }
        self.last_frame = None;
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    fn seek(&mut self, x: Pixels) {
        if let Some(b) = self.timeline.get() {
            let w = b.size.width.as_f32().max(1.);
            self.t = ((x - b.origin.x).as_f32() / w).clamp(0., 1.);
            self.playing = false;
            self.last_frame = None;
        }
    }

    /// Asks where to save, then writes the export on a background thread.
    fn export(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if matches!(self.exporting, Exporting::Busy(_)) {
            return;
        }
        let Some(entry) = self.entries.get(self.scene) else {
            return;
        };
        let source = match entry {
            Entry::Builtin(s) => Source::Builtin(s),
            Entry::Starter(st, ..) => Source::Starter(st),
            Entry::Script(s) => Source::File(s.path.clone()),
            Entry::Broken { .. } => return,
        };
        let mut palette = self.look.palette();
        palette.soft = entry.soft();
        let job = Job {
            kind,
            source,
            name: entry.name(),
            seconds: entry.seconds(),
            palette,
            t: self.t,
            tau: self.tau,
        };
        let dir = self.root.clone().unwrap_or_else(crate::platform::home);
        let rx = cx.prompt_for_new_path(&dir, Some(&export_name(&job.name, kind)));
        cx.spawn(async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let _ = this.update(cx, |v, cx| {
                v.exporting = Exporting::Busy(kind);
                cx.notify();
            });
            let result = cx
                .background_spawn(async move {
                    let bytes = job.run()?;
                    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.exporting = match result {
                    Ok(()) => Exporting::Idle,
                    Err(e) => Exporting::Failed(e, Instant::now()),
                };
                cx.notify();
            });
            // Comes back to clear the error once its time is up.
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    fn copy_svg(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(self.scene) else {
            return;
        };
        let Ok(nodes) = entry.nodes(self.t, self.tau) else {
            return;
        };
        let items = iso::shoot(&nodes, self.extents[self.scene]);
        let mut palette = self.look.palette();
        palette.soft = entry.soft();
        let svg = iso::svg::frame(&entry.name(), &items, &palette, 16.);
        cx.write_to_clipboard(ClipboardItem::new_string(svg));
        self.copied = Some(Instant::now());
        cx.notify();
    }
}

/// The step-by-step guide's clock: `at` is where the scene is, `to` where
/// it stops next; both run past 1 and below 0 rather than wrapping.
#[derive(Clone, Copy)]
struct Guide {
    at: f32,
    to: f32,
}

/// A square outline button the height of the page's other controls.
fn square_button(id: &'static str, icon: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(ui::BUTTON_H))
        .rounded(crate::ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border())
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()))
        .child(ui::icon(icon, 14.).text_color(theme::text()))
}

impl gpui::Focusable for IsoView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// What a rasterized frame was drawn from: when nothing changed (paused, no
/// resize), the last image is painted again instead of a new one.
#[derive(Clone, Copy, PartialEq)]
struct FrameKey {
    scene: usize,
    generation: u64,
    t: f32,
    /// The ambient clock too: at a guide stop `t` holds still while `tau`
    /// keeps the scene moving, and a key without it froze the picture.
    tau: f32,
    look: Look,
    size: (u32, u32),
    scale: f32,
}

/// The stage's frames, shared by the view and the frame being drawn.
type Shown = Rc<RefCell<Stage>>;

#[derive(Default)]
struct Stage {
    /// The image on screen and what it shows.
    shown: Option<(FrameKey, Arc<RenderImage>)>,
    /// A frame is being drawn on the background executor.
    drawing: bool,
    /// The buffers, while no frame is being drawn with them.
    frames: Option<raster::Frames>,
    /// Images replaced since the last paint. They leave the sprite atlas on
    /// the next paint, once nothing shows them any more.
    retired: Vec<Arc<RenderImage>>,
}

/// Paints the newest frame over `bounds`, and starts drawing the one `key`
/// asks for when it differs. Rasterizing a full-size frame takes
/// milliseconds, so it runs on the background executor and the stage shows
/// the previous frame until it lands. While one is drawing no other starts:
/// when drawing is slower than the display, frames are skipped, not queued.
#[allow(clippy::too_many_arguments)]
fn paint(
    items: Vec<Item>,
    extent: Rect,
    palette: Palette,
    key: FrameKey,
    shown: &Shown,
    view: gpui::WeakEntity<IsoView>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let mut stage = shown.borrow_mut();
    for old in stage.retired.drain(..) {
        let _ = window.drop_image(old);
    }
    let (w, h) = key.size;
    if w > 0
        && h > 0
        && !stage.drawing
        && stage.shown.as_ref().is_none_or(|(k, _)| *k != key)
        && let Some(fit) = Fit::new(extent, w as f32, h as f32, 32. * key.scale)
    {
        stage.drawing = true;
        let mut frames = stage.frames.take().unwrap_or_default();
        let task = cx.background_spawn(async move {
            let image = draw_frame(&mut frames, &items, &palette, w, h, fit, 1.25 * key.scale);
            (frames, image)
        });
        let shown = shown.clone();
        cx.spawn(async move |cx| {
            let (frames, image) = task.await;
            let mut stage = shown.borrow_mut();
            stage.drawing = false;
            stage.frames = Some(frames);
            let Some(image) = image else {
                return;
            };
            if let Some((_, old)) = stage.shown.replace((key, image)) {
                stage.retired.push(old);
            }
            drop(stage);
            let _ = view.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
    if let Some((_, image)) = &stage.shown {
        // Inside the stage's 1 px border, following the radius setting like
        // the stage does, so the picture's corners stay on its border.
        let corners = gpui::Corners::all(ui::nested(ui::RADIUS_LG, 1.));
        let _ = window.paint_image(bounds, bounds, corners, image.clone(), 0, false);
    }
}

/// Rasterizes `items` with tiny-skia into `frames` and copies the result
/// into an image GPUI can upload. The copy can't be avoided: a RenderImage
/// owns its bytes and never hands them back.
fn draw_frame(
    frames: &mut raster::Frames,
    items: &[Item],
    palette: &Palette,
    w: u32,
    h: u32,
    fit: Fit,
    line: f32,
) -> Option<Arc<RenderImage>> {
    let pixmap = frames.draw(items, palette, w, h, fit, line)?;
    // The ground is opaque, so premultiplied RGBA is plain RGBA; GPUI wants BGRA.
    let mut data = pixmap.data().to_vec();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(w, h, data)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl Render for IsoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let seconds = self.entries.get(self.scene).map_or(1., Entry::seconds);
        // Both modes keep the ambient clock going while the tab draws. A
        // paused scene holds it still: a finished frame notifies the view,
        // and a clock that moved would ask for another frame forever.
        let now = Instant::now();
        let dt = self
            .last_frame
            .map_or(0., |l| (now - l).as_secs_f32().min(0.1));
        self.last_frame = Some(now);
        if self.playing || self.guide.is_some() {
            self.tau += dt;
        }
        if let Some(g) = &mut self.guide {
            let steps = self.entries.get(self.scene).and_then(Entry::steps);
            let d = g.to - g.at;
            if d > 1e-5 {
                // The tail of a step (past its stop) goes by fast: the next
                // step is what the press asked for.
                let speed = match steps {
                    Some((n, hold)) if (g.at * n as f32).fract() > hold - 1e-4 => 4.,
                    _ => 1.,
                };
                g.at = (g.at + dt / seconds * speed).min(g.to);
            } else {
                // At the stop: keep the clock small so it never drifts far.
                let whole = g.at.floor();
                g.at -= whole;
                g.to -= whole;
            }
            self.t = g.at.rem_euclid(1.);
            window.request_animation_frame();
        } else if self.playing {
            self.t = (self.t + dt / seconds) % 1.;
            window.request_animation_frame();
        }
        if self
            .copied
            .is_some_and(|at| at.elapsed().as_secs_f32() > 1.6)
        {
            self.copied = None;
        }

        let entry = &self.entries[self.scene];
        let (items, error) = match entry.nodes(self.t, self.tau) {
            Ok(nodes) => (iso::shoot(&nodes, self.extents[self.scene]), None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let extent = self.extents[self.scene];
        let (name, about) = (entry.name(), entry.about());
        let file =
            entry.path().map(
                |p| match self.root.as_ref().and_then(|r| p.strip_prefix(r).ok()) {
                    Some(rel) => rel.display().to_string(),
                    None => p.display().to_string(),
                },
            );
        let mut palette = self.look.palette();
        palette.soft = entry.soft();

        // ---- toolbar -------------------------------------------------------------
        let mut scenes_seg = ui::segmented();
        for (i, e) in self.entries.iter().enumerate() {
            let broken = matches!(e, Entry::Broken { .. });
            scenes_seg = scenes_seg.child(
                ui::segment(
                    ("iso-scene", i),
                    broken.then_some("alert-circle"),
                    e.name(),
                    i == self.scene,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(i);
                    cx.notify();
                })),
            );
        }
        let mut looks = ui::segmented();
        for (i, (look, label)) in [(Look::Arakun, "Arakun"), (Look::Paper, "Paper")]
            .into_iter()
            .enumerate()
        {
            looks = looks.child(
                ui::segment(("iso-look", i), None, label, self.look == look).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.look = look;
                        cx.notify();
                    },
                )),
            );
        }
        // A square outline button, the height of the others in the page.
        let play = div()
            .id("iso-play")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(ui::BUTTON_H))
            .rounded(crate::ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border())
            .cursor_pointer()
            .hover(|s| s.bg(theme::fill_hover()))
            .child(
                ui::icon(if self.playing { "pause" } else { "play" }, 14.)
                    .text_color(theme::text()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.playing = !this.playing;
                this.last_frame = None;
                cx.notify();
            }));
        let copy = ui::button_with(
            "iso-copy",
            Some("copy01"),
            if self.copied.is_some() {
                "Copied"
            } else {
                "Copy SVG"
            },
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.copy_svg(cx)));
        let new_scene = ui::button_with(
            "iso-new",
            Some("add01"),
            "New scene",
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, window, cx| this.open_ask(false, window, cx)));
        let changeable = !matches!(entry, Entry::Builtin(_));
        let change = ui::button_with(
            "iso-change",
            Some("pencil-edit01"),
            "Change",
            ButtonKind::Outline,
            changeable,
        )
        .when(changeable, |b| {
            b.on_click(cx.listener(|this, _, window, cx| this.open_ask(true, window, cx)))
        })
        .when(!changeable, |b| {
            b.tooltip(ui::tip(
                "This scene is part of Arakun and has no script to change",
            ))
        });
        let ask_row = self.asking.as_ref().map(|a| {
            ui::field()
                .child(ui::icon("sparkles", 14.).text_color(theme::text_muted()))
                .child(div().flex_1().min_w_0().child(a.input.clone()))
        });
        let ask_error = self.ask_error.clone();
        if let Exporting::Failed(_, at) = &self.exporting
            && at.elapsed().as_secs_f32() > 4.
        {
            self.exporting = Exporting::Idle;
        }
        let exportable = !matches!(entry, Entry::Broken { .. });
        let busy = matches!(self.exporting, Exporting::Busy(_));
        let export_failed = match &self.exporting {
            Exporting::Failed(e, _) => Some(e.clone()),
            _ => None,
        };
        let exports = [Kind::Png, Kind::Svg, Kind::Loop].map(|kind| {
            let label = match &self.exporting {
                Exporting::Busy(k) if *k == kind => "Exporting…",
                _ => kind.label(),
            };
            let enabled = exportable && !busy;
            ui::button_with(
                ("iso-export", kind as usize),
                Some("download04"),
                label,
                ButtonKind::Outline,
                enabled,
            )
            .when(enabled, |b| {
                b.on_click(cx.listener(move |this, _, _, cx| this.export(kind, cx)))
            })
        });

        // ---- timeline ------------------------------------------------------------
        let slot = self.timeline.clone();
        let progress = self.t;
        let track = div()
            .id("iso-timeline")
            .flex_1()
            .relative()
            .h(px(ui::BUTTON_H))
            .cursor_pointer()
            .child(
                canvas(move |b, _, _| slot.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(px((ui::BUTTON_H - 4.) / 2.))
                    .left_0()
                    .right_0()
                    .h(px(4.))
                    .rounded_full()
                    .bg(theme::fill_selected())
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(progress))
                            .rounded_full()
                            .bg(theme::accent()),
                    ),
            )
            // The playhead: a knob ringed in the page color, centered on the time.
            .child(
                div()
                    .absolute()
                    .top(px((ui::BUTTON_H - 14.) / 2.))
                    .left(gpui::relative(progress))
                    .ml(px(-7.))
                    .size(px(14.))
                    .rounded_full()
                    .border_2()
                    .border_color(theme::background())
                    .bg(theme::accent()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.scrubbing = true;
                    this.seek(e.position.x);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.scrubbing && e.pressed_button == Some(MouseButton::Left) {
                    this.seek(e.position.x);
                    cx.notify();
                } else {
                    this.scrubbing = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.scrubbing = false),
            );
        // Fixed width, so the track doesn't twitch as the digits change.
        let clock = ui::text(
            Type::Mono,
            theme::text_subtle(),
            format!("{:.2}s / {:.1}s", self.t * seconds, seconds),
        )
        .w(px(104.))
        .flex_none()
        .text_right();

        // ---- the guide or the video controls ---------------------------------------------
        let steps = self.entries.get(self.scene).and_then(Entry::steps);
        let mut modes = ui::segmented();
        if steps.is_some() {
            for (i, (guide, label)) in [(false, "Video"), (true, "Guide")].into_iter().enumerate() {
                modes = modes.child(
                    ui::segment(("iso-mode", i), None, label, self.guide.is_some() == guide)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_guide(guide);
                            cx.notify();
                        })),
                );
            }
        }
        let controls = match (self.guide, steps) {
            (Some(g), Some((n, _))) => {
                let current = ((g.to.rem_euclid(1.)) * n as f32).floor() as u32 % n + 1;
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(crate::ui::sp(12.))
                    .child(
                        square_button("iso-prev", "arrow-left01").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.guide_step(-1);
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        ui::text(Type::Label, theme::text(), format!("Step {current} of {n}"))
                            .w(px(104.))
                            .flex_none()
                            .text_center(),
                    )
                    .child(
                        square_button("iso-next", "arrow-right01").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.guide_step(1);
                                cx.notify();
                            },
                        )),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        "← → on the keyboard",
                    ))
            }
            _ => div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(12.))
                .child(play)
                .child(track)
                .child(clock),
        };

        // ---- stage ---------------------------------------------------------------
        let (index, generation, t, tau, look, shown) = (
            self.scene,
            self.generation,
            self.t,
            self.tau,
            self.look,
            self.shown.clone(),
        );
        let view = cx.entity().downgrade();
        let stage = div()
            .flex_1()
            .min_h(px(240.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(hsla(palette.ground, 1.))
            .child(
                canvas(
                    |_, _, _| {},
                    move |b, _, window, cx| {
                        let scale = window.scale_factor();
                        let size = (
                            (b.size.width.as_f32() * scale).round().max(0.) as u32,
                            (b.size.height.as_f32() * scale).round().max(0.) as u32,
                        );
                        let key = FrameKey {
                            scene: index,
                            generation,
                            t,
                            tau,
                            look,
                            size,
                            scale,
                        };
                        paint(items, extent, palette, key, &shown, view, b, window, cx)
                    },
                )
                .size_full(),
            );

        div()
            .id("iso-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if this.guide.is_none() {
                    return;
                }
                match ev.keystroke.key.as_str() {
                    "right" | "space" => this.guide_step(1),
                    "left" => this.guide_step(-1),
                    _ => return,
                }
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .p(crate::ui::sp(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(4.))
                    .child(ui::text(Type::Title, theme::text(), name))
                    .when(!about.is_empty(), |d| {
                        d.child(ui::text(Type::Body, theme::text_subtle(), about))
                    })
                    .when_some(file, |d, f| {
                        d.child(ui::text(Type::Mono, theme::text_faint(), f))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .children(exports)
                    .when_some(export_failed, |d, e| {
                        d.child(ui::text(Type::Caption, theme::error().into(), e))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    // More scenes than fit the line scroll sideways.
                    .child(
                        div()
                            .id("iso-scenes")
                            .flex()
                            .max_w_full()
                            .overflow_x_scroll()
                            .child(scenes_seg),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(10.))
                            .when(steps.is_some(), |d| d.child(modes))
                            .child(looks)
                            .child(new_scene)
                            .child(change)
                            .child(copy),
                    ),
            )
            .when_some(ask_row, |d, row| d.child(row))
            .when_some(ask_error, |d, e| {
                d.child(ui::text(Type::Caption, theme::error().into(), e))
            })
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .p(crate::ui::sp(12.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::error())
                        .child(ui::text(Type::Mono, theme::error().into(), e)),
                )
            })
            .child(stage)
            .child(controls)
    }
}

/// The three exports of a scene.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Png,
    Svg,
    Loop,
}

/// The file name a save dialog suggests for an export of `scene`.
fn export_name(scene: &str, kind: Kind) -> String {
    let slug = ask::slug(scene);
    match kind {
        Kind::Png => format!("{slug}.png"),
        Kind::Svg => format!("{slug}.svg"),
        Kind::Loop => format!("{slug}-loop.svg"),
    }
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Png => "Export PNG",
            Kind::Svg => "Export SVG",
            Kind::Loop => "Export animated SVG",
        }
    }
}

/// Where an export reads its scene from, sent to a background thread: a
/// compiled script stays on the UI thread, so it is compiled again there.
enum Source {
    Builtin(&'static scenes::Scene),
    Starter(&'static Starter),
    File(PathBuf),
}

/// Everything an export needs, owned so it can leave the UI thread.
struct Job {
    kind: Kind,
    source: Source,
    name: String,
    seconds: f32,
    palette: Palette,
    t: f32,
    tau: f32,
}

impl Job {
    /// The file's bytes. PNG is 1600 px wide; the loop is sampled 240 times
    /// a second and strays at most 0.1 px, as `arakun iso anim` does.
    fn run(self) -> Result<Vec<u8>, String> {
        let script = match &self.source {
            Source::Builtin(_) => None,
            Source::Starter(st) => Some(starters::load(st)?),
            Source::File(path) => Some(ScriptScene::load(path)?),
        };
        let frame = |t: f32, tau: f32| match (&self.source, &script) {
            (_, Some(s)) => s.frame_at(t, tau),
            (Source::Builtin(b), None) => Ok((b.build)(t)),
            _ => Err("no scene".into()),
        };
        // Measured again rather than taken from the tab, whose fallback box
        // would let a scene that draws nothing export a blank file.
        // The file may have changed since the click: its own length counts.
        let seconds = script.as_ref().map_or(self.seconds, |s| s.meta.seconds);
        let ext = iso::export::extent(&frame, seconds)?;
        let pal = &self.palette;
        match self.kind {
            Kind::Png => iso::export::png(&frame, ext, self.t, self.tau, pal, 1600),
            Kind::Svg => iso::export::svg(&self.name, &frame, ext, self.t, self.tau, pal)
                .map(String::into_bytes),
            Kind::Loop => iso::export::loop_svg(&self.name, &frame, ext, seconds, pal, 240., 0.1)
                .map(String::into_bytes),
        }
    }
}

/// The state of the export buttons.
enum Exporting {
    Idle,
    Busy(Kind),
    /// Why the last export failed, shown in place of the labels for a while.
    Failed(String, Instant),
}

/// A starter with its compiled scene and the box it fills.
type CachedStarter = (&'static Starter, Rc<ScriptScene>, Rect);

/// Compiles and measures every starter that loads.
fn starter_cache() -> Vec<CachedStarter> {
    starters::ALL
        .iter()
        .filter_map(|st| {
            let s = Rc::new(starters::load(st).ok()?);
            let ext = Entry::Starter(st, s.clone(), [0.; 4]).measure();
            Some((st, s, ext))
        })
        .collect()
}

/// The list entries of the cached starters, sharing their compiled scenes.
fn starter_entries(cache: &[CachedStarter]) -> Vec<Entry> {
    cache
        .iter()
        .map(|(st, s, ext)| Entry::Starter(st, s.clone(), *ext))
        .collect()
}

/// Project scripts, then starters, then the Rust scenes. Every project
/// script stays, even two of one name, so a file an agent just wrote always
/// shows; a starter or Rust scene with the name of an earlier scene is left
/// out.
fn order(scripts: Vec<Entry>, starters: Vec<Entry>, builtins: Vec<Entry>) -> Vec<Entry> {
    let mut out = scripts;
    for e in starters.into_iter().chain(builtins) {
        let name = e.name();
        if !out.iter().any(|o| o.name() == name) {
            out.push(e);
        }
    }
    out
}

/// The pending paths that `paths` (the new list's files, in order) holds
/// are done: all of them leave `pending`, and the index of the first one in
/// the list comes back to be shown.
fn take_arrived(pending: &mut Vec<PathBuf>, paths: &[Option<&Path>]) -> Option<usize> {
    let arrived = |p: &PathBuf| paths.contains(&Some(p.as_path()));
    let first = paths
        .iter()
        .position(|q| q.is_some_and(|q| pending.iter().any(|p| p == q)));
    pending.retain(|p| !arrived(p));
    first
}

/// The entry with this file, or for a scene with no file, this name.
fn find_entry(entries: &[Entry], path: Option<&Path>, name: &str) -> Option<usize> {
    entries.iter().position(|e| match (path, e.path()) {
        (Some(p), Some(q)) => p == q,
        (Some(_), None) => false,
        (None, _) => e.name() == name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broken(path: &str) -> Entry {
        Entry::Broken {
            path: PathBuf::from(path),
            error: String::new(),
        }
    }

    #[test]
    fn order_hides_later_same_names() {
        let names: Vec<String> = order(
            vec![broken("x/a.rhai")],
            vec![broken("s/b.rhai"), broken("s/a.rhai")],
            vec![],
        )
        .iter()
        .map(Entry::name)
        .collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn order_keeps_every_project_script() {
        let names: Vec<String> = order(
            vec![broken("x/a.rhai"), broken("y/a.rhai")],
            vec![broken("s/a.rhai")],
            vec![],
        )
        .iter()
        .map(|e| e.path().unwrap().display().to_string())
        .collect();
        assert_eq!(names, ["x/a.rhai", "y/a.rhai"]);
    }

    #[test]
    fn starter_entries_reuse_the_cache() {
        let cache = starter_cache();
        assert_eq!(cache.len(), starters::ALL.len());
        let entries = starter_entries(&cache);
        for (e, c) in entries.iter().zip(&cache) {
            let Entry::Starter(_, s, ext) = e else {
                panic!("not a starter")
            };
            assert!(Rc::ptr_eq(s, &c.1));
            assert_eq!(e.extent(), *ext);
        }
    }

    #[test]
    fn take_arrived_clears_every_arrived_path() {
        let mut pending = vec![
            PathBuf::from("/r/iso/a.rhai"),
            PathBuf::from("/r/iso/b.rhai"),
            PathBuf::from("/r/iso/c.rhai"),
        ];
        let paths = [
            Some(Path::new("/r/iso/x.rhai")),
            Some(Path::new("/r/iso/b.rhai")),
            None,
            Some(Path::new("/r/iso/a.rhai")),
        ];
        assert_eq!(take_arrived(&mut pending, &paths), Some(1));
        assert_eq!(pending, [PathBuf::from("/r/iso/c.rhai")]);
        assert_eq!(take_arrived(&mut pending, &paths), None);
    }

    #[test]
    fn find_entry_prefers_the_file() {
        let entries = vec![broken("x/a.rhai"), broken("y/a.rhai")];
        assert_eq!(
            find_entry(&entries, Some(Path::new("y/a.rhai")), "a"),
            Some(1)
        );
        assert_eq!(find_entry(&entries, None, "a"), Some(0));
        assert_eq!(find_entry(&entries, Some(Path::new("z/a.rhai")), "a"), None);
    }

    #[test]
    fn loop_export_takes_the_length_from_the_script() {
        let job = Job {
            kind: Kind::Loop,
            source: Source::Starter(&starters::ALL[1]),
            name: "CPU".into(),
            seconds: 1.,
            palette: Look::Paper.palette(),
            t: 0.,
            tau: 0.,
        };
        let svg = String::from_utf8(job.run().unwrap()).unwrap();
        assert!(svg.contains("dur=\"5s\""), "the CPU loop is 5 s long");
    }

    #[test]
    fn export_names() {
        assert_eq!(
            export_name("EC2 instances", Kind::Loop),
            "ec2-instances-loop.svg"
        );
        assert_eq!(export_name("CPU", Kind::Png), "cpu.png");
        assert_eq!(export_name("CPU", Kind::Svg), "cpu.svg");
    }

    #[test]
    fn export_job_writes_each_kind() {
        let st = &starters::ALL[1];
        let job = |kind| Job {
            kind,
            source: Source::Starter(st),
            name: "CPU".into(),
            seconds: 5.,
            palette: Look::Paper.palette(),
            t: 0.3,
            tau: 1.5,
        };
        assert_eq!(&job(Kind::Png).run().unwrap()[..4], b"\x89PNG");
        assert!(
            String::from_utf8(job(Kind::Svg).run().unwrap())
                .unwrap()
                .contains("<path")
        );
        let anim = String::from_utf8(job(Kind::Loop).run().unwrap()).unwrap();
        assert!(anim.contains("<animate"));
        let rust = Job {
            source: Source::Builtin(&scenes::ALL[0]),
            ..job(Kind::Svg)
        };
        assert!(rust.run().is_ok());
    }

    #[test]
    fn export_of_an_empty_scene_fails() {
        let dir = std::env::temp_dir().join(format!("arakun-iso-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.rhai");
        std::fs::write(&path, "[]").unwrap();
        assert!(ScriptScene::load(&path).is_ok());
        for kind in [Kind::Png, Kind::Svg, Kind::Loop] {
            let job = Job {
                kind,
                source: Source::File(path.clone()),
                name: "empty".into(),
                seconds: 1.,
                palette: Look::Paper.palette(),
                t: 0.,
                tau: 0.,
            };
            assert!(job.run().is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
