//! Browser tabs outside the shell: `cargo run --release --example browser_demo -- [url...]`.
//! No URL → no browser tab (CEF stays unloaded until "+" is clicked).
//! Alt+1..9 switch tabs, Ctrl+T opens a blank tab, Ctrl+W closes the active one, Ctrl+Q
//! quits (shutting CEF down cleanly).

#[path = "../src/assets.rs"]
mod assets;
#[path = "../src/browser/mod.rs"]
mod browser;
#[allow(dead_code)]
#[path = "../src/platform.rs"]
mod platform;
#[path = "../src/theme.rs"]
#[allow(dead_code)]
mod theme;
#[path = "../src/ui.rs"]
#[allow(dead_code)]
mod ui;

use browser::{BrowserEvent, BrowserView};
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, WindowBounds, WindowOptions, div,
    prelude::FluentBuilder as _, px, size,
};

struct Tab {
    view: Entity<BrowserView>,
    _events: Subscription,
}

struct Demo {
    tabs: Vec<Tab>,
    active: usize,
    /// Keeps the window's shortcuts reachable when no tab holds focus.
    focus: gpui::FocusHandle,
}

impl Demo {
    fn open(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| BrowserView::new(url, window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, view, ev: &BrowserEvent, window, cx| match ev {
                BrowserEvent::OpenInNewTab(url) => this.open(url.clone(), window, cx),
                BrowserEvent::CloseRequested => {
                    if let Some(i) = this.tabs.iter().position(|t| &t.view == view) {
                        this.close(i, window, cx);
                    }
                }
                BrowserEvent::TitleChanged(t) => log(format_args!("title {t}"), cx),
                BrowserEvent::UrlChanged(u) => log(format_args!("url {u}"), cx),
                BrowserEvent::LoadingChanged(l) => {
                    let v = view.read(cx);
                    let state = format!(
                        "loading={l} back={} forward={}",
                        v.can_go_back(),
                        v.can_go_forward()
                    );
                    log(format_args!("{state}"), cx)
                }
                BrowserEvent::FaviconChanged => log(format_args!("favicon"), cx),
            },
        );
        self.tabs.push(Tab {
            view,
            _events: events,
        });
        self.switch(self.tabs.len() - 1, window, cx);
    }

    fn switch(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        for (i, tab) in self.tabs.iter().enumerate() {
            tab.view.update(cx, |v, cx| v.set_visible(i == index, cx));
        }
        let view = self.tabs[index].view.clone();
        if view.read(cx).url().is_empty() {
            view.update(cx, |v, cx| v.focus_address(window, cx));
        } else {
            window.focus(&view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(index);
        tab.view.update(cx, |v, cx| v.close(cx));
        if self.tabs.is_empty() {
            window.focus(&self.focus, cx);
        } else {
            self.switch(self.active.min(self.tabs.len() - 1), window, cx);
        }
        cx.notify();
    }

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.alt && !ks.modifiers.control {
            if let Ok(n @ 1..=9) = ks.key.parse::<usize>() {
                self.switch(n - 1, window, cx);
                cx.stop_propagation();
            }
        } else if ks.modifiers.control && ks.key == "t" {
            self.open(String::new(), window, cx);
            cx.stop_propagation();
        } else if ks.modifiers.control && ks.key == "w" && !self.tabs.is_empty() {
            self.close(self.active, window, cx);
            cx.stop_propagation();
        } else if ks.modifiers.control && ks.key == "q" {
            cx.quit();
        }
    }
}

fn log(what: std::fmt::Arguments, cx: &mut Context<Demo>) {
    if std::env::var_os("ARAKUN_DEMO_EVENTS").is_some() {
        eprintln!("browser_demo: {what}");
    }
    cx.notify();
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(34.))
            .px(px(6.))
            .bg(theme::sidebar())
            .border_b_1()
            .border_color(theme::stroke())
            .children(self.tabs.iter().enumerate().map(|(i, tab)| {
                let v = tab.view.read(cx);
                let active = i == self.active;
                div()
                    .id(("tab", i))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(8.))
                    .max_w(px(220.))
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .text_color(if active {
                        theme::content()
                    } else {
                        theme::mix(0.5)
                    })
                    .when(active, |d| d.bg(theme::mix(0.08)))
                    .hover(|d| d.bg(theme::mix(0.06)))
                    .cursor_pointer()
                    .when_some(v.favicon(), |d, f| {
                        d.child(gpui::img(f).size(px(14.)).flex_none())
                    })
                    .when(v.is_loading(), |d| {
                        d.child(ui::icon("loading03", 12.).text_color(theme::mix(0.5)))
                    })
                    .child(div().truncate().child(v.title()))
                    .child(
                        div()
                            .id(("close", i))
                            .child(ui::icon("cancel01", 10.).text_color(theme::mix(0.4)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close(i, window, cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.switch(i, window, cx)))
            }))
            .child(
                ui::icon_button("new-tab", "add01", 14.).on_click(
                    cx.listener(|this, _, window, cx| this.open(String::new(), window, cx)),
                ),
            )
            .child(div().flex_1())
            .child(div().text_size(px(11.)).text_color(theme::mix(0.35)).child(
                match browser::cdp_port() {
                    Some(p) => format!("CDP 127.0.0.1:{p}"),
                    None if browser::ENABLED => "CEF not started".into(),
                    None => "built without the browser feature".into(),
                },
            ));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .text_color(theme::content())
            .font_family(theme::FONT_SANS)
            .track_focus(&self.focus)
            // Capture phase: the page consumes Ctrl+key presses it receives.
            .capture_key_down(cx.listener(Self::key_down))
            .child(strip)
            .children(
                self.tabs
                    .get(self.active)
                    .map(|t| div().flex_1().min_h_0().child(t.view.clone())),
            )
    }
}

fn main() {
    browser::handle_subprocess();
    let started = std::time::Instant::now();
    let urls: Vec<String> = std::env::args().skip(1).collect();
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some("arakun-browser-demo".into()),
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        let mut demo = Demo {
                            tabs: Vec::new(),
                            active: 0,
                            focus: cx.focus_handle(),
                        };
                        window.focus(&demo.focus, cx);
                        for url in urls {
                            demo.open(url, window, cx);
                        }
                        if !demo.tabs.is_empty() {
                            demo.switch(0, window, cx);
                        }
                        demo
                    })
                },
            )
            .unwrap();
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
            if std::env::var_os("ARAKUN_TIMING").is_some() {
                eprintln!("browser_demo: window opened in {:?}", started.elapsed());
            }
        });
}
