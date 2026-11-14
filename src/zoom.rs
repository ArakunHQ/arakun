//! An image you can zoom into with the wheel, toward the cursor, and pan by dragging.
//! Once clicked it also takes + / - / 0 to zoom, and Ctrl+C to copy a local image.
//! In its own tab a tall image opens at its own size, and once zoomed the wheel
//! scrolls it while Ctrl+wheel zooms.
//! Inline (in a chat) the wheel only zooms with Ctrl, so plain scrolling still
//! scrolls the chat, and dragging a local image while it is not zoomed lifts it
//! out as a [`DraggedImage`] to drop beside the chat.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, ElementId, FocusHandle, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseMoveEvent, ObjectFit, ParentElement as _,
    Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, canvas, div, img, prelude::FluentBuilder as _, px,
};

use crate::{theme, ui};

const MAX_SCALE: f32 = 12.;
const DOUBLE_CLICK_SCALE: f32 = 2.5;

pub struct Zoom {
    scale: f32,
    /// Top-left of the scaled image box, relative to the viewer, in points.
    offset: (f32, f32),
    bounds: Bounds<Pixels>,
    drag: Option<Point<Pixels>>,
    dragged: f32,
    focus: Option<FocusHandle>,
    /// A short notice over the image, such as after copying it.
    note: Option<&'static str>,
    /// The image's size in pixels, when it is a file we could read.
    natural: Option<(f32, f32)>,
    /// Set once the first layout has placed the image (see `place`).
    placed: bool,
}

impl Zoom {
    fn new() -> Self {
        Self {
            scale: 1.,
            offset: (0., 0.),
            bounds: Bounds::default(),
            drag: None,
            dragged: 0.,
            focus: None,
            note: None,
            natural: None,
            placed: false,
        }
    }

    /// A tall image opens at its own size (no wider than the viewer) from its
    /// top, for the wheel to scroll down; fitting it whole would leave it tiny.
    fn place(&mut self, scale_factor: f32) {
        self.placed = true;
        let Some((nw, nh)) = self.natural.filter(|(w, h)| *w > 0. && *h > 0.) else {
            return;
        };
        let (bw, bh) = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        if bw <= 0. || bh <= 0. {
            return;
        }
        // Height it shows at when fit, and at its own size.
        let fit_h = bh.min(bw * nh / nw);
        let own_h = bw.min(nw / scale_factor) * nh / nw;
        let scale = (own_h / fit_h).min(MAX_SCALE);
        if scale > 1.001 {
            self.scale = scale;
            self.offset = ((bw - bw * scale) / 2., 0.);
            self.clamp();
        }
    }

    /// How big it shows against its own size, when that is known.
    fn own_percent(&self, scale_factor: f32) -> Option<f32> {
        let (nw, nh) = self.natural.filter(|(w, h)| *w > 0. && *h > 0.)?;
        let (bw, bh) = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        let fit_h = bh.min(bw * nh / nw);
        (fit_h > 0.).then(|| fit_h * self.scale / (nh / scale_factor) * 100.)
    }

    fn zoomed(&self) -> bool {
        self.scale > 1.001
    }

    fn zoom_at(&mut self, factor: f32, at: Point<Pixels>) {
        let old = self.scale;
        let new = (old * factor).clamp(1., MAX_SCALE);
        let k = new / old;
        let px_ = f32::from(at.x - self.bounds.origin.x);
        let py_ = f32::from(at.y - self.bounds.origin.y);
        // Keep the point under the cursor fixed while the scale changes.
        self.offset = (px_ - (px_ - self.offset.0) * k, py_ - (py_ - self.offset.1) * k);
        self.scale = new;
        self.clamp();
    }

    fn pan(&mut self, dx: f32, dy: f32) {
        self.offset = (self.offset.0 + dx, self.offset.1 + dy);
        self.clamp();
    }

    fn zoom_center(&mut self, factor: f32) {
        let center = self.bounds.center();
        self.zoom_at(factor, center);
    }

    fn reset(&mut self) {
        self.scale = 1.;
        self.offset = (0., 0.);
    }

    fn clamp(&mut self) {
        if !self.zoomed() {
            self.reset();
            return;
        }
        let w = f32::from(self.bounds.size.width);
        let h = f32::from(self.bounds.size.height);
        self.offset.0 = self.offset.0.clamp(w * (1. - self.scale), 0.);
        self.offset.1 = self.offset.1.clamp(h * (1. - self.scale), 0.);
    }
}

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A local image lifted out of a chat, on its way to a pane.
#[derive(Clone)]
pub struct DraggedImage {
    pub path: PathBuf,
    offset: Point<Pixels>,
}

impl Render for DraggedImage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .left(-self.offset.x.min(px(80.)))
            .top(-self.offset.y.min(px(60.)))
            .w(px(160.))
            .h(px(120.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::accent().opacity(0.6))
            .bg(theme::dialog())
            .shadow_lg()
            .overflow_hidden()
            .opacity(0.9)
            .child(img(self.path.clone()).size_full().object_fit(ObjectFit::Contain))
    }
}

thread_local! {
    /// The image being dragged out of a chat, if any (the shell shows where it can land).
    static LIFTED: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// The image being dragged out of a chat, while a drag is on.
pub fn lifted_image(cx: &App) -> Option<PathBuf> {
    if !cx.has_active_drag() {
        return None;
    }
    LIFTED.with(|l| l.borrow().clone())
}

pub fn drop_lifted_image() {
    LIFTED.with(|l| *l.borrow_mut() = None);
}

/// `on_click` runs for a plain click on the image while it is not zoomed or dragged.
/// `file` is the image on disk, for Ctrl+C.
pub fn zoom_image(
    id: impl Into<ElementId>,
    source: impl Into<ImageSource>,
    file: Option<PathBuf>,
    on_click: Option<ClickHandler>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    build(id.into(), source.into(), file, on_click, false, window, cx)
}

/// For an image inside scrolling content: Ctrl+wheel zooms (the plain wheel
/// scrolls the content), and dragging `file` while not zoomed lifts it out.
pub fn zoom_image_inline(
    id: impl Into<ElementId>,
    source: impl Into<ImageSource>,
    file: Option<PathBuf>,
    on_click: Option<ClickHandler>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    build(id.into(), source.into(), file, on_click, true, window, cx)
}

fn build(
    id: ElementId,
    source: ImageSource,
    file: Option<PathBuf>,
    on_click: Option<ClickHandler>,
    inline: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let state = window.use_keyed_state(id.clone(), cx, |_, cx| {
        let mut z = Zoom::new();
        z.focus = Some(cx.focus_handle());
        z.natural = file
            .as_ref()
            .filter(|_| !inline)
            .and_then(|f| imagesize::size(f).ok())
            .map(|s| (s.width as f32, s.height as f32));
        z
    });
    let z = state.read(cx);
    let focus = z.focus.clone();
    let note = z.note;
    let zoomed = z.zoomed();
    let dragging = z.drag.is_some() && z.dragged > 3.;
    let percent = z.own_percent(window.scale_factor()).unwrap_or(z.scale * 100.);
    let scale = z.scale;
    let (ox, oy) = z.offset;
    let size = z.bounds.size;

    let image = img(source).object_fit(ObjectFit::Contain).with_fallback(|| {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(ui::TEXT_MD))
            .text_color(theme::text_subtle())
            .child("Could not load the image")
            .into_any_element()
    });
    let image = if zoomed {
        image.absolute().left(px(ox)).top(px(oy)).w(size.width * scale).h(size.height * scale)
    } else {
        image.size_full()
    };

    let badge_state = state.clone();
    let lift = file.clone().filter(|_| inline && !zoomed);
    div()
        .id(id)
        .relative()
        .size_full()
        .overflow_hidden()
        .when_some(focus.clone(), |d, f| d.track_focus(&f))
        .when(zoomed, |d| if dragging { d.cursor_grabbing() } else { d.cursor_grab() })
        .on_key_down({
            let state = state.clone();
            move |ev: &KeyDownEvent, _, cx| {
                let key = ev.keystroke.key.as_str();
                let m = &ev.keystroke.modifiers;
                if crate::platform::command_key(m) && key == "c" {
                    let Some(path) = file.clone() else { return };
                    cx.stop_propagation();
                    copy(state.clone(), path, cx);
                    return;
                }
                if m.control || m.alt || m.platform {
                    return;
                }
                let factor = match key {
                    "+" | "=" => 1.25,
                    "-" => 0.8,
                    "0" => 0.,
                    _ => return,
                };
                cx.stop_propagation();
                state.update(cx, |z, cx| {
                    if factor == 0. {
                        z.reset();
                    } else {
                        z.zoom_center(factor);
                    }
                    cx.notify();
                });
            }
        })
        .child(
            canvas(
                {
                    let state = state.clone();
                    move |bounds, window, cx| {
                        let scale_factor = window.scale_factor();
                        let placed = state.update(cx, |z, _| {
                            z.bounds = bounds;
                            let first = !z.placed && bounds.size.height > px(0.);
                            if first {
                                z.place(scale_factor);
                            }
                            first
                        });
                        if placed {
                            // A refresh asked for while drawing is dropped.
                            window.on_next_frame(|window, _| window.refresh());
                        }
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(image)
        .on_scroll_wheel({
            let state = state.clone();
            move |ev: &ScrollWheelEvent, _, cx| {
                let zoom_key = ev.modifiers.control || (cfg!(target_os = "macos") && ev.modifiers.platform);
                if inline && !zoom_key {
                    return;
                }
                // In its own tab, a zoomed image scrolls with the wheel; Ctrl+wheel zooms.
                if !inline && !zoom_key && state.read(cx).zoomed() {
                    let (dx, dy) = match ev.delta {
                        ScrollDelta::Lines(d) => (d.x * 40., d.y * 40.),
                        ScrollDelta::Pixels(d) => (f32::from(d.x), f32::from(d.y)),
                    };
                    cx.stop_propagation();
                    state.update(cx, |z, cx| {
                        z.pan(dx, dy);
                        cx.notify();
                    });
                    return;
                }
                // A wheel notch arrives as one line; touchpads send small pixel steps.
                let factor = match ev.delta {
                    ScrollDelta::Lines(d) => 1.2_f32.powf(d.y),
                    ScrollDelta::Pixels(d) => (f32::from(d.y) * 0.005).exp(),
                };
                if factor == 1. {
                    return;
                }
                cx.stop_propagation();
                state.update(cx, |z, cx| {
                    z.zoom_at(factor, ev.position);
                    cx.notify();
                });
            }
        })
        .on_mouse_down(MouseButton::Left, {
            let state = state.clone();
            let focus = focus.clone();
            move |ev, window, cx| {
                if let Some(f) = &focus {
                    window.focus(f, cx);
                }
                state.update(cx, |z, _| {
                    z.drag = Some(ev.position);
                    z.dragged = 0.;
                })
            }
        })
        .on_mouse_move({
            let state = state.clone();
            move |ev: &MouseMoveEvent, _, cx| {
                state.update(cx, |z, cx| {
                    let Some(from) = z.drag else { return };
                    if ev.pressed_button != Some(MouseButton::Left) {
                        z.drag = None;
                        cx.notify();
                        return;
                    }
                    let dx = f32::from(ev.position.x - from.x);
                    let dy = f32::from(ev.position.y - from.y);
                    z.dragged += dx.abs() + dy.abs();
                    z.drag = Some(ev.position);
                    if z.zoomed() {
                        z.pan(dx, dy);
                        cx.notify();
                    }
                })
            }
        })
        .on_mouse_up(MouseButton::Left, {
            let state = state.clone();
            move |_, _, cx| {
                state.update(cx, |z, cx| {
                    z.drag = None;
                    cx.notify();
                })
            }
        })
        .on_click({
            let state = state.clone();
            move |ev: &ClickEvent, window, cx| {
                cx.stop_propagation();
                let (zoomed, dragged) = {
                    let z = state.read(cx);
                    (z.zoomed(), z.dragged > 3.)
                };
                if dragged {
                    return;
                }
                if ev.click_count() >= 2 {
                    state.update(cx, |z, cx| {
                        if z.zoomed() {
                            z.reset();
                        } else {
                            z.zoom_at(DOUBLE_CLICK_SCALE, ev.position());
                        }
                        cx.notify();
                    });
                } else if !zoomed
                    && let Some(f) = &on_click {
                        f(window, cx);
                    }
            }
        })
        .when_some(lift, |d, path| {
            d.on_drag(DraggedImage { path, offset: Point::default() }, |d, offset, window, cx| {
                LIFTED.with(|l| *l.borrow_mut() = Some(d.path.clone()));
                window.refresh();
                let mut d = d.clone();
                d.offset = offset;
                cx.new(|_| d)
            })
        })
        .when_some(note, |d, note| {
            d.child(
                div()
                    .absolute()
                    .left(px(12.))
                    .bottom(px(12.))
                    .flex()
                    .items_center()
                    .h(px(ui::ICON_BUTTON))
                    .px(px(8.))
                    .rounded(px(ui::RADIUS))
                    .bg(gpui::black().opacity(0.65))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(gpui::white().opacity(0.9))
                    .child(note),
            )
        })
        .when(zoomed, |d| {
            d.child(
                div()
                    .id("zoom-badge")
                    .absolute()
                    .right(px(12.))
                    .bottom(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(ui::ICON_BUTTON))
                    .px(px(8.))
                    .rounded(px(ui::RADIUS))
                    .bg(gpui::black().opacity(0.65))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(gpui::white().opacity(0.9))
                    .cursor_pointer()
                    .hover(|s| s.bg(gpui::black().opacity(0.8)))
                    .child(format!("{percent:.0}%"))
                    .child(div().text_color(gpui::white().opacity(0.55)).child("Fit"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        badge_state.update(cx, |z, cx| {
                            z.reset();
                            cx.notify();
                        });
                    }),
            )
        })
        .into_any_element()
}

fn copy(state: gpui::Entity<Zoom>, path: PathBuf, cx: &mut App) {
    let task = cx.background_spawn(async move { crate::platform::copy_image(&path) });
    state.update(cx, |_, cx| {
        cx.spawn(async move |this, cx| {
            let note = if task.await.is_ok() { "Copied" } else { "Could not copy the image" };
            let _ = this.update(cx, |z, cx| {
                z.note = Some(note);
                cx.notify();
            });
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |z, cx| {
                z.note = None;
                cx.notify();
            });
        })
        .detach();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn zoom(w: f32, h: f32) -> Zoom {
        let mut z = Zoom::new();
        z.bounds = Bounds::new(point(px(100.), px(50.)), size(px(w), px(h)));
        z
    }

    #[test]
    fn keeps_the_point_under_the_cursor() {
        let mut z = zoom(400., 300.);
        z.zoom_at(2., point(px(200.), px(110.)));
        // Cursor at (100, 60) inside the viewer: image point (50, 30) stays under it.
        assert_eq!(z.scale, 2.);
        assert_eq!(z.offset, (-100., -60.));
    }

    #[test]
    fn stays_within_the_image() {
        let mut z = zoom(400., 300.);
        z.zoom_at(2., point(px(100.), px(50.)));
        z.pan(500., 500.);
        assert_eq!(z.offset, (0., 0.));
        z.pan(-5000., -5000.);
        assert_eq!(z.offset, (-400., -300.));
    }

    #[test]
    fn never_shrinks_below_fit() {
        let mut z = zoom(400., 300.);
        z.zoom_at(0.5, point(px(300.), px(200.)));
        assert_eq!(z.scale, 1.);
        assert_eq!(z.offset, (0., 0.));
    }
}
