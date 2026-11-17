//! Standalone harness for `src/terminal`: tabs of real shells in the app's frame.
//!
//!   cargo run --release --example terminal_demo            full-window terminal
//!   ARAKUN_TERM_DOCK=1 cargo run --release --example terminal_demo   220 px bottom dock
//!   ARAKUN_TERM_CMD="btop" ...                                 run a command instead of the shell
//!
//! Ctrl+Shift+T new tab, Ctrl+Shift+W hang up the tab, Ctrl+PageUp/PageDown switch tabs. A tab closes when its shell
//! exits; the demo quits with the last one.

#[path = "../src/assets.rs"]
mod assets;
#[path = "../src/input.rs"]
#[allow(dead_code)]
mod input;
#[allow(dead_code)]
#[path = "../src/platform.rs"]
mod platform;
#[allow(dead_code)]
#[path = "../src/redact.rs"]
mod redact;
#[path = "../src/terminal/mod.rs"]
mod terminal;
#[path = "../src/theme.rs"]
#[allow(dead_code)]
mod theme;
#[path = "../src/ui.rs"]
#[allow(dead_code, unused_imports)]
mod ui;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px,
    size,
};
use terminal::{Terminal, TerminalEvent, TerminalView};

struct Tab {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    _sub: Subscription,
}

struct Demo {
    tabs: Vec<Tab>,
    active: usize,
    dock: bool,
    focus: FocusHandle,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut demo = Demo {
            tabs: Vec::new(),
            active: 0,
            dock: std::env::var_os("ARAKUN_TERM_DOCK").is_some(),
            focus: cx.focus_handle(),
        };
        demo.add_tab(window, cx);
        demo
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let command = std::env::var("ARAKUN_TERM_CMD")
            .ok()
            .filter(|_| self.tabs.is_empty())
            .map(|c| vec!["/bin/sh".to_string(), "-c".to_string(), c]);
        let terminal = match Terminal::spawn(cwd, command, cx) {
            Ok(t) => t,
            Err(err) => {
                eprintln!("terminal_demo: spawn failed: {err:#}");
                return;
            }
        };
        eprintln!(
            "terminal_demo: pid {} in {}",
            terminal.read(cx).pid(),
            terminal.read(cx).cwd().display()
        );
        let view = cx.new(|cx| TerminalView::new(terminal.clone(), window, cx));
        let sub = cx.subscribe_in(
            &terminal,
            window,
            |this, terminal, event, window, cx| match event {
                TerminalEvent::Exited => {
                    eprintln!(
                        "terminal_demo: exited with {:?}",
                        terminal.read(cx).has_exited()
                    );
                    this.tabs.retain(|t| &t.terminal != terminal);
                    if this.tabs.is_empty() {
                        cx.quit();
                        return;
                    }
                    this.active = this.active.min(this.tabs.len() - 1);
                    this.focus_active(window, cx);
                }
                TerminalEvent::TitleChanged => cx.notify(),
                TerminalEvent::Bell | TerminalEvent::Activity => {}
            },
        );
        self.tabs.push(Tab {
            terminal,
            view,
            _sub: sub,
        });
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            let handle = tab.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = e.keystroke.modifiers;
        let key = e.keystroke.key.to_ascii_lowercase();
        if m.control && m.shift && key == "t" {
            self.add_tab(window, cx);
            cx.stop_propagation();
        } else if m.control && m.shift && key == "w" {
            if let Some(tab) = self.tabs.get(self.active) {
                tab.view.read(cx).terminal().read(cx).kill();
            }
            cx.stop_propagation();
        } else if m.control && !m.shift && (key == "pageup" || key == "pagedown") {
            let n = self.tabs.len();
            if n > 0 {
                self.active = if key == "pageup" {
                    (self.active + n - 1) % n
                } else {
                    (self.active + 1) % n
                };
                self.focus_active(window, cx);
            }
            cx.stop_propagation();
        }
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div().flex().items_center().h_full().gap(px(2.));
        for (i, tab) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            tabs = tabs.child(
                div()
                    .id(("tab", i))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .w(px(224.))
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .when(active, |d| {
                        d.bg(theme::selection()).text_color(theme::content())
                    })
                    .when(!active, |d| d.text_color(theme::mix(0.5)))
                    .child(ui::icon("computer-terminal01", 14.).text_color(if active {
                        theme::content()
                    } else {
                        theme::mix(0.5)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(tab.terminal.read(cx).title()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = i;
                        this.focus_active(window, cx);
                    })),
            );
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(36.))
            .pl(px(6.))
            .border_b_1()
            .border_color(theme::stroke())
            .child(tabs);

        // Only the active tab's view is in the tree; the others keep running unseen.
        let body = div()
            .flex_1()
            .min_h_0()
            .children(self.tabs.get(self.active).map(|t| t.view.clone()));

        let dock = div()
            .flex()
            .flex_col()
            .when(self.dock, |d| d.flex_none().h(px(220.)).border_t_1())
            .when(!self.dock, |d| d.flex_1().min_h_0())
            .border_color(theme::stroke())
            .child(header)
            .child(body);

        div()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .text_color(theme::content())
            .font_family(theme::FONT_SANS)
            .when(self.dock, |d| d.child(div().flex_1()))
            .child(dock)
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("arakun terminal".into()),
                        ..Default::default()
                    }),
                    window_background: WindowBackgroundAppearance::Opaque,
                    app_id: Some("arakun-terminal-demo".into()),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Demo::new(window, cx)),
            )
            .unwrap();
            cx.activate(true);
        });
}
