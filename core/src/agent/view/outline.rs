//! The outline of a session: the transcript in miniature on the right edge,
//! with a box over the part on screen. It is the screen made small: a turn
//! takes in the outline the height it takes in the list, so the box is as big
//! as the viewport and dragging it scrolls evenly, as a code minimap does. A
//! click on a turn outside the box brings its prompt to the top.
//!
//! The miniature keeps one size, a mark for each line of text, and grows down
//! from the top as the session does. Once it fills the lane it is squeezed to
//! fit. It shows only when the session is at least two screens long.
//!
//! The list only knows the height of the rows it has drawn, so a row not seen
//! yet is given the height its content should take and corrected once drawn.
//!
//! It costs little by design: a row's pieces are rebuilt only when its entries
//! change, the layout is kept until a height changes, and one canvas paints it.

use gpui::{BorderStyle, Bounds, DispatchPhase, ListOffset, canvas, fill, quad, size};

use super::*;

/// The lane the pointer reaches, and the room the list leaves it when the
/// session is too narrow to hold it in its margin.
const LANE_W: f32 = 26.;
const LANE_ROOM: f32 = 32.;
const LANE_INSET: f32 = 12.;
const LANE_RIGHT: f32 = 4.;
/// Where the miniature starts inside the lane, and how wide it is.
const MARKS_X: f32 = 5.;
const MARKS_W: f32 = 16.;
/// The box never gets too small to take hold of.
const BOX_MIN: f32 = 12.;
/// Lane pixels per pixel of transcript while the miniature still fits: a
/// line of text every five pixels.
const NATURAL: f32 = 0.25;
/// The outline shows from this many screens of transcript on, and stays until
/// the session is well under that, so folding a turn does not flicker it.
const SHOW_AT: f32 = 2.;
const HIDE_AT: f32 = 1.6;
const CARD_W: f32 = 236.;

// What the transcript's pieces measure, to guess the height of a row the list
// has not drawn yet: a line of text, a character, a tool call, the air above a
// prompt and between blocks, and a prompt's bubble.
const LINE: f32 = 20.;
const CHAR_W: f32 = 6.4;
const STEP_H: f32 = 32.;
const TURN_GAP: f32 = 48.;
const BLOCK_GAP: f32 = 12.;
const BUBBLE_W: f32 = 576.;

/// One piece of a row's miniature.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Atom {
    /// A prompt: whether it runs past one line, and its share of the width.
    Prompt { tall: bool, width: f32 },
    /// A block of text.
    Text,
    /// A tool call or a line of the same weight, as long as its kind reads.
    Step(f32),
    /// A task list of this many rows.
    Tasks(u8),
}

/// A piece and where its row puts it, in the row's own guessed pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Piece {
    y: f32,
    h: f32,
    atom: Atom,
}

/// What one list row shows, and the height that should take.
#[derive(Clone, Debug, Default, PartialEq)]
struct RowModel {
    pieces: Vec<Piece>,
    h: f32,
}

/// The lines `text` takes at `cpl` characters a line, and its longest line.
fn measure(text: &str, cpl: f32) -> (f32, f32) {
    let (mut lines, mut longest) = (0.0_f32, 0.0_f32);
    for line in text.split('\n') {
        let len = line.len() as f32;
        // An empty line is the air between two paragraphs.
        lines += if len == 0. { 0.6 } else { (len / cpl).ceil() };
        longest = longest.max(len);
    }
    (lines.max(1.), longest)
}

/// Builds a row's model top to bottom.
struct Sketch {
    pieces: Vec<Piece>,
    y: f32,
    /// The width of the transcript's column, and the characters a line holds.
    col_w: f32,
    cpl: f32,
}

impl Sketch {
    fn new(col_w: f32) -> Self {
        let col_w = col_w.max(120.);
        Sketch {
            pieces: Vec::new(),
            y: 0.,
            col_w,
            cpl: (col_w / CHAR_W).max(20.),
        }
    }

    fn put(&mut self, atom: Atom, h: f32, after: f32) {
        self.pieces.push(Piece { y: self.y, h, atom });
        self.y += h + after;
    }

    fn prompt(&mut self, text: &str, first: bool) {
        self.y += if first { LINE } else { TURN_GAP };
        let cpl = ((BUBBLE_W.min(self.col_w) - 24.) / CHAR_W).max(20.);
        let (lines, longest) = measure(text, cpl);
        let width = ((longest.min(cpl) * CHAR_W + 24.) / self.col_w).clamp(0.3, 1.);
        let tall = lines > 1.5;
        self.put(Atom::Prompt { tall, width }, lines * LINE + 16., BLOCK_GAP);
    }

    fn entry(&mut self, e: &Entry) {
        match e {
            Entry::Agent { text } => {
                let (lines, _) = measure(text, self.cpl);
                self.put(Atom::Text, lines * LINE, BLOCK_GAP);
            }
            Entry::Tool(call) => {
                let reach = match call.kind.as_str() {
                    "read" | "search" | "fetch" | "think" => 5.,
                    "execute" => 10.,
                    _ => 7.,
                };
                self.put(Atom::Step(reach), LINE, STEP_H - LINE);
            }
            Entry::Thought { .. } => self.put(Atom::Step(5.), LINE, STEP_H - LINE),
            Entry::Plan { entries } => {
                let rows = entries.len().clamp(1, 12) as u8;
                self.put(Atom::Tasks(rows), 44. + 28. * f32::from(rows), BLOCK_GAP);
            }
            Entry::Notice { .. } | Entry::Compacted { .. } => self.y += 28.,
            Entry::User { .. } => {}
        }
    }

    /// The "Worked for" line a turn's work folds behind.
    fn work_line(&mut self) {
        self.put(Atom::Step(9.), LINE, STEP_H - LINE);
    }

    fn done(self) -> RowModel {
        RowModel {
            h: self.y + 8.,
            pieces: self.pieces,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tone {
    Prompt,
    Text,
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    turn: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    tone: Tone,
}

/// A row to lay out: its turn, what it shows and the height it has.
struct Row<'a> {
    turn: usize,
    model: &'a RowModel,
    h: f32,
}

/// The miniature laid out for one lane height.
#[derive(Debug)]
pub(super) struct Laid {
    lane_h: f32,
    /// The height of the whole transcript, and lane pixels per pixel of it.
    total: f32,
    k: f32,
    /// The height the miniature takes in the lane, from its top.
    extent: f32,
    /// Where each row starts in the transcript; one more closes the last.
    tops: Vec<f32>,
    row_turn: Vec<usize>,
    rects: Vec<Rect>,
}

/// Lays the rows out from the top of a lane `lane_h` tall. The miniature keeps
/// its natural size while it fits and is squeezed once it would pass the lane.
/// Marks that would land on top of each other are dropped, so the number
/// painted never passes what fits.
fn lay(rows: &[Row<'_>], footer: f32, lane_h: f32) -> Laid {
    let mut tops = Vec::with_capacity(rows.len() + 1);
    let mut y = 0.0_f32;
    for r in rows {
        tops.push(y);
        y += r.h;
    }
    tops.push(y);
    let total = y + footer;
    let k = if total > 0. {
        (lane_h / total).min(NATURAL)
    } else {
        NATURAL
    };
    let mut rects: Vec<Rect> = Vec::new();
    // The bottom of the last mark drawn, with its air: nothing starts above it.
    let mut floor = 0.0_f32;
    for (i, r) in rows.iter().enumerate() {
        let scale = if r.model.h > 0. { r.h / r.model.h } else { 1. };
        let turn = r.turn;
        let push = |rects: &mut Vec<Rect>, floor: &mut f32, x, y: f32, w, h: f32, tone| {
            if y + h > lane_h + 0.5 {
                return;
            }
            rects.push(Rect {
                turn,
                x,
                y,
                w,
                h,
                tone,
            });
            *floor = floor.max(y + h + 1.);
        };
        for p in &r.model.pieces {
            let y = ((tops[i] + p.y * scale) * k).round();
            let h = p.h * scale * k;
            match p.atom {
                Atom::Prompt { tall, width } => {
                    let ph = match (h < 6., tall && h >= 10.) {
                        (true, _) => 3.,
                        (false, true) => 7.,
                        (false, false) => 4.,
                    };
                    let w = (width * MARKS_W).round().clamp(5., MARKS_W);
                    // A prompt is never dropped: it moves under what is there.
                    let y = y.max(floor);
                    push(&mut rects, &mut floor, MARKS_W - w, y, w, ph, Tone::Prompt);
                }
                Atom::Text => {
                    // Lines fill the block at a pitch the eye reads as text.
                    let pitch = (LINE * scale * k).clamp(3., 5.);
                    let most = lane_h as usize / 3 + 1;
                    let n = ((h / pitch).floor().max(1.) as usize).min(most);
                    for j in 0..n {
                        let at = y + (j as f32 * pitch).round();
                        if at < floor {
                            continue;
                        }
                        let w = match (n, j + 1 == n) {
                            (1, _) => 11.,
                            (_, true) => 9.,
                            _ => MARKS_W,
                        };
                        push(&mut rects, &mut floor, 0., at, w, 2., Tone::Text);
                    }
                }
                Atom::Step(reach) => {
                    if y >= floor {
                        if STEP_H * scale * k >= 4. && y + 2. <= lane_h {
                            rects.push(Rect {
                                turn,
                                x: 1.,
                                y,
                                w: 2.,
                                h: 2.,
                                tone: Tone::Step,
                            });
                        }
                        push(&mut rects, &mut floor, 5., y, reach, 2., Tone::Step);
                    }
                }
                Atom::Tasks(rows) => {
                    // One short line a task, close together: it reads as a list.
                    let pitch = (h / f32::from(rows)).clamp(3., 6.);
                    for j in 0..rows {
                        let at = y + (f32::from(j) * pitch).round();
                        if at < floor {
                            continue;
                        }
                        if pitch >= 4. && at + 2. <= lane_h {
                            rects.push(Rect {
                                turn,
                                x: 1.,
                                y: at,
                                w: 2.,
                                h: 2.,
                                tone: Tone::Text,
                            });
                        }
                        push(&mut rects, &mut floor, 5., at, 8., 2., Tone::Text);
                    }
                }
            }
        }
    }
    Laid {
        lane_h,
        total,
        k,
        extent: total * k,
        tops,
        row_turn: rows.iter().map(|r| r.turn).collect(),
        rects,
    }
}

impl Laid {
    /// The row holding transcript pixel `px`, held inside the rows.
    fn row_of(&self, px: f32) -> Option<usize> {
        let n = self.row_turn.len();
        if n == 0 {
            return None;
        }
        Some(
            self.tops[..n]
                .partition_point(|&top| top <= px)
                .saturating_sub(1),
        )
    }

    /// The turn drawn at `y`, counted from the top of the lane.
    fn turn_at(&self, y: f32) -> Option<usize> {
        if y < 0. || y > self.extent || self.k <= 0. {
            return None;
        }
        Some(self.row_turn[self.row_of(y / self.k)?])
    }

    /// Where `turn` starts in the lane.
    fn turn_y(&self, turn: usize) -> Option<f32> {
        let row = self.row_turn.partition_point(|&t| t < turn);
        (row < self.row_turn.len()).then(|| self.tops[row] * self.k)
    }
}

/// The button held down on the lane.
struct Drag {
    /// Where it went down. A press that never leaves this spot is a click.
    from: Point<Pixels>,
    moved: bool,
    /// How far under the top of the box the pointer holds it, so the box does
    /// not jump under the pointer when the drag starts.
    grip: f32,
    /// It went down on the box: a click there has nowhere to go.
    on_box: bool,
}

/// What the outline keeps between frames.
#[derive(Default)]
pub(super) struct Outline {
    /// What each list row shows, row by row.
    models: Vec<RowModel>,
    /// The heights of the rows the list has drawn, and of what follows them.
    measured: RefCell<HashMap<ListRow, f32>>,
    footer: Cell<f32>,
    /// The width the models were made for: text wraps by it.
    col_w: f32,
    laid: RefCell<Option<Rc<Laid>>>,
    /// Where the lane was painted, to tell what the pointer is over.
    at: Cell<Bounds<Pixels>>,
    hover: Option<usize>,
    over_box: bool,
    drag: Option<Drag>,
    /// A row the drag scrolled to before the list had measured it, and how
    /// far into it to go once it has.
    settle: Option<(usize, f32)>,
    /// The session is long enough for the outline to be worth showing.
    shown: bool,
    /// The session is too narrow to hold the lane in its margin, so the list
    /// leaves it room.
    own_room: bool,
}

/// What one frame paints.
struct Frame {
    laid: Rc<Laid>,
    /// The part of the transcript on screen, as lane positions.
    span: Option<(f32, f32)>,
    hover: Option<usize>,
    /// The box is under the pointer or held.
    held: bool,
    dragging: bool,
    settle: bool,
}

fn paint(bounds: Bounds<Pixels>, frame: &Frame, window: &mut Window) {
    let laid = &frame.laid;
    let o = bounds.origin;
    let at = |x: f32, y: f32, w: f32, h: f32| {
        Bounds::new(point(o.x + px(x), o.y + px(y)), size(px(w), px(h)))
    };
    if let Some(turn) = frame.hover {
        let mine = laid.rects.iter().filter(|r| r.turn == turn);
        let top = mine.clone().map(|r| r.y).fold(f32::MAX, f32::min);
        let end = mine.map(|r| r.y + r.h).fold(0., f32::max);
        if end > top {
            window.paint_quad(quad(
                at(MARKS_X - 3., top - 3., MARKS_W + 6., end - top + 6.),
                px(3.),
                theme::fill_raised(),
                px(0.),
                gpui::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    }
    let (seen_top, seen_end) = frame.span.unwrap_or((-1., -1.));
    if let Some((top, end)) = frame.span {
        let (ground, edge) = if frame.held {
            (theme::fill_selected(), theme::border_strong())
        } else {
            (theme::fill_hover(), theme::border())
        };
        window.paint_quad(quad(
            at(0., top, LANE_W, end - top),
            px(4.),
            ground,
            px(1.),
            edge,
            BorderStyle::Solid,
        ));
    }
    for r in &laid.rects {
        // The box lights what it holds; the pointer lights its turn a step more.
        let lit = frame.hover == Some(r.turn);
        let seen = r.y + r.h > seen_top && r.y < seen_end;
        let color = match (r.tone, lit, seen) {
            (Tone::Prompt, true, _) => theme::text(),
            (Tone::Prompt, false, true) => theme::text_subtle(),
            (Tone::Prompt, false, false) => theme::border_strong(),
            (Tone::Text, true, _) | (Tone::Text, _, true) => theme::text_faint(),
            (Tone::Text, false, false) => theme::border(),
            (Tone::Step, true, _) | (Tone::Step, _, true) => theme::border_strong(),
            (Tone::Step, false, false) => theme::border().opacity(0.6),
        };
        let b = at(MARKS_X + r.x, r.y, r.w, r.h);
        match r.tone {
            Tone::Prompt => window.paint_quad(quad(
                b,
                px(2.),
                color,
                px(0.),
                gpui::transparent_black(),
                BorderStyle::Solid,
            )),
            Tone::Text | Tone::Step => window.paint_quad(fill(b, color)),
        }
    }
}

impl ThreadView {
    /// What list row `row` shows: its prompt, the "Worked for" line with the
    /// work behind it when that is open, the plan and the reply.
    fn row_model(&self, row: ListRow, t: &Thread) -> RowModel {
        let turn = row.turn();
        let Some(range) = self.turns.get(turn).cloned() else {
            return RowModel::default();
        };
        let Some(entries) = t.entries().get(range.clone()) else {
            return RowModel::default();
        };
        let live = is_live_turn(turn, &self.turns, t);
        let parts = turn_parts(entries, live);
        let body = &entries[parts.body_start..];
        let content = &body[..body.len() - parts.trailing_notices];
        let mut s = Sketch::new(self.outline.col_w);
        let head = |s: &mut Sketch| {
            if let Some(Entry::User { text, .. }) = entries.first().map(|e| e.as_ref()) {
                s.prompt(text, turn == 0);
            }
            if !parts.work.is_empty() {
                s.work_line();
            }
        };
        let tail = |s: &mut Sketch| {
            if let Some(plan) = parts.plan {
                s.entry(&content[plan]);
            }
            for i in parts.reply.clone() {
                s.entry(&content[i]);
            }
            s.y += 28. * parts.trailing_notices as f32;
        };
        match row {
            ListRow::Turn(_) => {
                head(&mut s);
                if live || self.folds.contains(&range.start) {
                    for &i in &parts.work {
                        s.entry(&content[i]);
                    }
                }
                tail(&mut s);
            }
            ListRow::TurnHead(_) => head(&mut s),
            ListRow::TurnPhase { phase, .. } => {
                let phase = self
                    .long_turns
                    .get(&turn)
                    .and_then(|long| long.phases.get(phase));
                if let Some(phase) = phase {
                    for &i in phase.head.iter().chain(&phase.steps) {
                        if let Some(e) = content.get(i) {
                            s.entry(e);
                        }
                    }
                }
            }
            ListRow::TurnTail(_) => tail(&mut s),
        }
        s.done()
    }

    /// Rebuilds the models of the rows holding entry `from` and later; all of
    /// them with `None`. A streamed chunk therefore visits only the last turn,
    /// and the layout is thrown away only when a row really changed.
    pub(super) fn sync_outline(&mut self, from: Option<usize>, cx: &App) {
        let t = self.thread.read(cx);
        let first = match from {
            Some(from) => {
                let turn = self.turns.partition_point(|r| r.end <= from);
                self.rows.partition_point(|r| r.turn() < turn)
            }
            None => 0,
        }
        .min(self.outline.models.len())
        .min(self.rows.len());
        let fresh: Vec<RowModel> = self.rows[first..]
            .iter()
            .map(|&row| self.row_model(row, t))
            .collect();
        if self.outline.models.len() == self.rows.len() && self.outline.models[first..] == fresh {
            return;
        }
        // A row whose content changed off screen has a height nobody measured.
        {
            let mut measured = self.outline.measured.borrow_mut();
            for (i, model) in fresh.iter().enumerate() {
                if self.outline.models.get(first + i) != Some(model) {
                    measured.remove(&self.rows[first + i]);
                }
            }
        }
        self.outline.models.truncate(first);
        self.outline.models.extend(fresh);
        self.outline.laid.take();
        if self.outline.hover.is_some_and(|t| t >= self.turns.len()) {
            self.outline.hover = None;
        }
    }

    /// Forgets every height: the transcript is another one, or wraps anew.
    pub(super) fn reset_outline(&mut self, cx: &App) {
        self.outline.measured.borrow_mut().clear();
        self.outline.footer.set(0.);
        self.outline.settle = None;
        self.outline.drag = None;
        self.sync_outline(None, cx);
    }

    /// Whether the outline shows: only when there is scrolling worth the name.
    fn outline_shown(&self) -> bool {
        self.outline.shown
    }

    /// The height the lane has for the miniature.
    fn outline_lane(&self) -> f32 {
        (self.list.viewport_bounds().size.height.as_f32() - 2. * LANE_INSET).max(0.)
    }

    /// The room the list leaves the lane. Wide sessions have it in their
    /// margin already, so their column stays centered under the composer.
    pub(super) fn outline_room(&mut self, cx: &App) -> Pixels {
        let had = if self.outline.own_room { LANE_ROOM } else { 0. };
        let width = self.list.viewport_bounds().size.width.as_f32();
        if width > 0. {
            // Text wraps by the column's width, and so do the guessed heights.
            let col_w = width.min(COLUMN_W) - 12.;
            if (col_w - self.outline.col_w).abs() > 8. {
                self.outline.col_w = col_w;
                self.reset_outline(cx);
            }
        }
        // Worth showing from two screens of transcript on.
        if self.outline_measure() {
            self.outline.laid.take();
        }
        let view_h = self.list.viewport_bounds().size.height.as_f32();
        let screens = if view_h > 0. && !self.turns.is_empty() {
            self.outline_laid(self.outline_lane()).total / view_h
        } else {
            0.
        };
        let least = if self.outline.shown { HIDE_AT } else { SHOW_AT };
        self.outline.shown = screens >= least;
        if !self.outline_shown() {
            self.outline.own_room = false;
            return px(0.);
        }
        if width > 0. {
            self.outline.own_room = width + had < COLUMN_W + 2. * LANE_ROOM;
        }
        px(if self.outline.own_room { LANE_ROOM } else { 0. })
    }

    /// Takes the heights of the rows the list has laid out. True when one
    /// changed, so the miniature has to be laid out again.
    fn outline_measure(&self) -> bool {
        let mut measured = self.outline.measured.borrow_mut();
        let view = self.list.viewport_bounds();
        let mut changed = false;
        let mut ix = self.list.logical_scroll_top().item_ix;
        while let Some(&row) = self.rows.get(ix) {
            let Some(b) = self.list.bounds_for_item(ix) else {
                return changed;
            };
            let h = b.size.height.as_f32();
            if measured
                .insert(row, h)
                .is_none_or(|old| (old - h).abs() > 0.5)
            {
                changed = true;
            }
            if b.top() > view.bottom() {
                return changed;
            }
            ix += 1;
        }
        // What the list draws after its last row: the status line and asks.
        if let Some(b) = self.list.bounds_for_item(ix) {
            let h = b.size.height.as_f32();
            if (self.outline.footer.replace(h) - h).abs() > 0.5 {
                changed = true;
            }
        }
        changed
    }

    fn outline_laid(&self, lane_h: f32) -> Rc<Laid> {
        let mut slot = self.outline.laid.borrow_mut();
        if let Some(laid) = &*slot
            && laid.lane_h == lane_h
        {
            return laid.clone();
        }
        let measured = self.outline.measured.borrow();
        let rows: Vec<Row<'_>> = self
            .rows
            .iter()
            .zip(&self.outline.models)
            .map(|(row, model)| Row {
                turn: row.turn(),
                model,
                h: measured.get(row).copied().unwrap_or(model.h),
            })
            .collect();
        let laid = Rc::new(lay(&rows, self.outline.footer.get(), lane_h));
        *slot = Some(laid.clone());
        laid
    }

    /// The part of the transcript on screen, in the lane.
    fn outline_box(&self, laid: &Laid) -> Option<(f32, f32)> {
        let view_h = self.list.viewport_bounds().size.height.as_f32();
        if view_h <= 0. || laid.tops.is_empty() {
            return None;
        }
        let top = self.list.logical_scroll_top();
        let row = top.item_ix.min(laid.tops.len() - 1);
        let scroll = laid.tops[row] + top.offset_in_item.as_f32();
        let (a, b) = (
            scroll * laid.k,
            ((scroll + view_h) * laid.k).min(laid.extent),
        );
        if b - a >= BOX_MIN {
            return Some((a, b));
        }
        let a = ((a + b - BOX_MIN) / 2.).clamp(0., (laid.extent - BOX_MIN).max(0.));
        Some((a, a + BOX_MIN))
    }

    /// Runs after the list is laid out, so the box is where the list is now.
    fn outline_frame(&self, bounds: Bounds<Pixels>) -> Frame {
        self.outline.at.set(bounds);
        if self.outline_measure() {
            self.outline.laid.take();
        }
        let laid = self.outline_laid(self.outline_lane());
        Frame {
            span: self.outline_box(&laid),
            laid,
            hover: self.outline.hover,
            held: self.outline.over_box || self.outline.drag.is_some(),
            dragging: self.outline.drag.is_some(),
            settle: self.outline.settle.is_some(),
        }
    }

    /// Where `at` falls in the lane, and whether that is on the box.
    fn outline_spot(&self, at: Point<Pixels>) -> Option<(Rc<Laid>, f32, bool)> {
        let laid = self.outline.laid.borrow().clone()?;
        let y = (at.y - self.outline.at.get().top()).as_f32();
        let on_box = self
            .outline_box(&laid)
            .is_some_and(|(top, end)| y >= top && y <= end);
        Some((laid, y, on_box))
    }

    fn outline_hover(&mut self, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        // A drag owns the lane: no card comes and goes under it.
        if self.outline.drag.is_some() {
            return;
        }
        let spot = at
            .filter(|at| self.outline.at.get().contains(at))
            .and_then(|at| self.outline_spot(at));
        // The box is what is on screen already, so it shows no card.
        let (hover, over_box) = match spot {
            Some((_, _, true)) => (None, true),
            Some((laid, y, false)) => (laid.turn_at(y), false),
            None => (None, false),
        };
        if hover != self.outline.hover || over_box != self.outline.over_box {
            self.outline.hover = hover;
            self.outline.over_box = over_box;
            cx.notify();
        }
    }

    /// Brings the prompt of `turn` to the top.
    fn outline_jump(&mut self, turn: usize, cx: &mut Context<Self>) {
        let row = self.rows.partition_point(|r| r.turn() < turn);
        self.glide = None;
        self.outline.settle = None;
        self.list.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        });
        self.following = false;
        cx.notify();
    }

    /// The button went down on the lane: a drag starts, or a click.
    fn outline_press(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((laid, y, on_box)) = self.outline_spot(at) else {
            return;
        };
        // Held by the box it keeps its place under the pointer; pressed
        // anywhere else, the box centers on the pointer once it moves.
        let grip = match self.outline_box(&laid) {
            Some((top, _)) if on_box => y - top,
            Some((top, end)) => (end - top) / 2.,
            None => 0.,
        };
        self.outline.drag = Some(Drag {
            from: at,
            moved: false,
            grip,
            on_box,
        });
        self.outline.hover = None;
        cx.notify();
    }

    /// The pointer moved with the button down: the transcript follows it.
    fn outline_drag(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        // The button came up outside the window, where no event said so.
        if ev.pressed_button != Some(MouseButton::Left) {
            self.outline.drag = None;
            cx.notify();
            return;
        }
        let Some(drag) = &mut self.outline.drag else {
            return;
        };
        if !drag.moved && (ev.position.y - drag.from.y).abs() < px(3.) {
            return;
        }
        drag.moved = true;
        let grip = drag.grip;
        let Some(laid) = self.outline.laid.borrow().clone() else {
            return;
        };
        let view_h = self.list.viewport_bounds().size.height.as_f32();
        let top = (ev.position.y - self.outline.at.get().top()).as_f32() - grip;
        let scroll = top / laid.k;
        if scroll >= laid.total - view_h - 1. {
            // Dragged to the end: it follows new content again.
            self.outline.settle = None;
            self.jump_to_bottom(cx);
        } else {
            self.outline_scroll_to(&laid, scroll.max(0.), cx);
        }
    }

    /// The button came up: a press that never moved was a click on a turn.
    fn outline_release(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.outline.drag.take() else {
            return;
        };
        if !drag.moved
            && !drag.on_box
            && let Some((laid, y, _)) = self.outline_spot(drag.from)
            && let Some(turn) = laid.turn_at(y)
        {
            self.outline_jump(turn, cx);
        }
        self.outline_hover(Some(at), cx);
        cx.notify();
    }

    /// Puts transcript pixel `scroll` at the top of the list. A row's height
    /// is known only once the list measured it, so a row reached for the first
    /// time shows its start and settles a frame later.
    fn outline_scroll_to(&mut self, laid: &Laid, scroll: f32, cx: &mut Context<Self>) {
        let Some(row) = laid.row_of(scroll) else {
            return;
        };
        let Some(&key) = self.rows.get(row) else {
            return;
        };
        let inner = (scroll - laid.tops[row]).max(0.);
        let known = self.outline.measured.borrow().contains_key(&key);
        let offset = if known {
            self.outline.settle = None;
            inner
        } else {
            let guessed = laid.tops[row + 1] - laid.tops[row];
            let into = if guessed > 0. { inner / guessed } else { 0. };
            self.outline.settle = Some((row, into.clamp(0., 1.)));
            0.
        };
        self.glide = None;
        self.list.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(offset),
        });
        self.following = false;
        cx.notify();
    }

    /// Finishes a scroll to a row whose height was not known when it started.
    fn outline_settle(&mut self, cx: &mut Context<Self>) {
        let Some((row, into)) = self.outline.settle.take() else {
            return;
        };
        if self.list.logical_scroll_top().item_ix != row {
            return;
        }
        if let Some(b) = self.list.bounds_for_item(row) {
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: b.size.height * into,
            });
            cx.notify();
        }
    }

    /// The lane, as tall as the miniature so that the rest of the edge does
    /// nothing, and the card of the turn the pointer is over.
    pub(super) fn render_outline(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !self.outline_shown() {
            return Vec::new();
        }
        let extent = self.outline_laid(self.outline_lane()).extent;
        let mut parts = Vec::new();
        if self.outline.own_room {
            // The room the list left is no part of the list: the wheel still
            // scrolls the session there.
            let room = div()
                .id("session-outline-room")
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(LANE_ROOM))
                .on_scroll(cx.listener(|this, ev: &ScrollEvent, window, cx| {
                    let delta = ev.delta.pixel_delta(window.line_height());
                    this.glide = None;
                    this.list.scroll_by(-delta.y);
                    this.following = this.list.is_following_tail();
                    cx.notify();
                }));
            parts.push(room.into_any_element());
        }
        let (this, held) = (cx.entity(), cx.entity());
        let lane = crate::ui::labelled(
            div().id("session-outline"),
            gpui::Role::Button,
            "Session outline",
        )
        .absolute()
        .top(px(LANE_INSET))
        .h(px(extent))
        .right(px(LANE_RIGHT))
        .w(px(LANE_W))
        .cursor_pointer()
        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
            this.outline_hover(Some(ev.position), cx);
        }))
        .on_hover(cx.listener(|this, over: &bool, _, cx| {
            if !*over {
                this.outline_hover(None, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.outline_press(ev.position, cx);
            }),
        )
        .child(
            canvas(
                move |bounds, _, cx| this.read(cx).outline_frame(bounds),
                move |bounds, frame, window, _| {
                    paint(bounds, &frame, window);
                    if frame.settle {
                        let view = held.clone();
                        window.on_next_frame(move |_, cx| {
                            view.update(cx, |this, cx| this.outline_settle(cx));
                        });
                    }
                    if !frame.dragging {
                        return;
                    }
                    // A drag goes on wherever the pointer is, off the lane
                    // too, and nothing under it reacts meanwhile.
                    let view = held.clone();
                    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            view.update(cx, |this, cx| this.outline_drag(ev, cx));
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            held.update(cx, |this, cx| this.outline_release(ev.position, cx));
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .size_full(),
        );
        let lane = crate::ui::motion::enter(
            lane,
            "session-outline-in",
            crate::ui::motion::From::Above,
            0.,
            crate::ui::motion::BASE,
        );
        parts.push(lane.into_any_element());
        parts.extend(self.render_outline_card(cx));
        parts
    }

    /// The prompt of the turn under the pointer, with its number, its time
    /// and what the turn took.
    fn render_outline_card(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let turn = self.outline.hover?;
        let laid = self.outline.laid.borrow().clone()?;
        let range = self.turns.get(turn)?.clone();
        let t = self.thread.read(cx);
        let entries = t.entries().get(range)?;
        let Entry::User { text, at, .. } = entries.first()?.as_ref() else {
            return None;
        };
        // The first turn has no prompt when the transcript starts mid-session.
        let first = t.entries().first().map(|e| e.as_ref());
        let unprompted = !matches!(first, Some(Entry::User { .. }));
        let number = turn + 1 - usize::from(unprompted);
        let (shown, _) = crate::redact::chat_text(text);
        let mut line: String = shown.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some((cut, _)) = line.char_indices().nth(200) {
            line.truncate(cut);
            line.push('…');
        }
        let steps = entries
            .iter()
            .filter(|e| matches!(e.as_ref(), Entry::Tool(_)))
            .count();
        let failed = entries
            .iter()
            .filter(|e| matches!(e.as_ref(), Entry::Tool(c) if c.status == ToolStatus::Failed))
            .count();
        let plural = |n: usize, one: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {one}s")
            }
        };
        let mut took = Vec::new();
        if steps > 0 {
            took.push(plural(steps, "step"));
        }
        if failed > 0 {
            took.push(plural(failed, "error"));
        }
        let top = (LANE_INSET + laid.turn_y(turn)? - 9.)
            .min(LANE_INSET + laid.lane_h - 72.)
            .max(4.);
        Some(
            div()
                .absolute()
                .top(px(top))
                .right(px(LANE_RIGHT + LANE_W + 6.))
                .w(px(CARD_W))
                .px(crate::ui::sp(10.))
                .py(crate::ui::sp(8.))
                .rounded(crate::ui::rad(crate::ui::RADIUS))
                .border_1()
                .border_color(theme::border())
                .bg(theme::dialog())
                .flex()
                .flex_col()
                .gap(crate::ui::sp(4.))
                .text_size(px(crate::ui::TEXT_SM))
                .text_color(theme::text_muted())
                .child(div().line_clamp(3).text_ellipsis().child(line))
                .child(
                    div()
                        .flex()
                        .gap(crate::ui::sp(8.))
                        .font_family(theme::FONT_MONO)
                        .text_size(px(crate::ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(format!("{number:02}"))
                        .child(crate::when::clock(*at))
                        .child(div().flex_1())
                        .child(took.join(" · ")),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(len: usize) -> Entry {
        Entry::Agent {
            text: "a".repeat(len),
        }
    }

    fn turn(reply_chars: usize) -> RowModel {
        let mut s = Sketch::new(640.);
        s.prompt("Fix the tooltip", false);
        s.work_line();
        s.entry(&agent(reply_chars));
        s.done()
    }

    #[test]
    fn a_row_is_as_tall_as_what_it_shows() {
        // 640 px hold 100 characters a line.
        assert_eq!(measure(&"a".repeat(250), 100.), (3., 250.));
        assert_eq!(measure("one\n\ntwo", 100.), (2.6, 3.));
        let short = turn(80);
        let long = turn(800);
        assert!(matches!(
            short.pieces[0].atom,
            Atom::Prompt { tall: false, .. }
        ));
        assert_eq!(short.pieces.len(), 3);
        // Seven more lines of reply are seven lines more of height.
        assert_eq!(long.h - short.h, 7. * LINE);
        // A prompt of two lines is the taller mark.
        let mut s = Sketch::new(640.);
        s.prompt("one\ntwo", true);
        assert!(matches!(
            s.done().pieces[0].atom,
            Atom::Prompt { tall: true, .. }
        ));
    }

    #[test]
    fn the_miniature_keeps_its_size_and_the_rows_in_proportion() {
        let (a, b) = (turn(80), turn(800));
        let rows = [
            Row {
                turn: 0,
                model: &a,
                h: a.h,
            },
            Row {
                turn: 1,
                model: &b,
                // The list measured this row at twice what was guessed.
                h: b.h * 2.,
            },
        ];
        let laid = lay(&rows, 40., 600.);
        assert_eq!(laid.total, a.h + b.h * 2. + 40.);
        // It fits, so it keeps its natural size and leaves the rest of the lane.
        assert_eq!(laid.k, NATURAL);
        assert_eq!(laid.extent, laid.total * NATURAL);
        assert_eq!(laid.tops, [0., a.h, a.h + b.h * 2.]);
        assert!(laid.rects.iter().all(|r| r.y + r.h <= 600.5));
        // A point in the lane is the turn drawn there.
        assert_eq!(laid.turn_at(0.), Some(0));
        assert_eq!(laid.turn_at(a.h * laid.k + 1.), Some(1));
        assert_eq!(laid.turn_at(laid.extent - 1.), Some(1));
        // Under the miniature there is nothing to point at.
        assert_eq!(laid.turn_at(laid.extent + 1.), None);
        assert_eq!(laid.turn_y(1), Some(a.h * laid.k));
        assert_eq!(laid.row_of(a.h + 5.), Some(1));
    }

    #[test]
    fn a_long_session_is_squeezed_and_never_paints_more_than_fits() {
        let model = turn(4000);
        let rows: Vec<Row<'_>> = (0..400)
            .map(|turn| Row {
                turn,
                model: &model,
                h: model.h,
            })
            .collect();
        let laid = lay(&rows, 0., 760.);
        assert_eq!(laid.tops.len(), 401);
        assert_eq!(laid.extent, 760.);
        assert!(laid.rects.len() as f32 <= 760. / 3. + 1.);
        assert!(laid.rects.iter().all(|r| r.y + r.h <= 760.5));
        // No two marks share a line.
        assert!(laid.rects.windows(2).all(|w| w[1].y >= w[0].y + w[0].h));
    }
}
