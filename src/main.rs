mod account;
mod account_view;
mod agent;
mod app_menu;
mod ascii_face;
mod assets;
mod attach_picker;
mod automations_view;
mod branches;
mod branches_view;
mod browser;
#[cfg(feature = "coderlabs")]
mod canvas;
#[cfg(feature = "coderlabs")]
mod canvas_view;
mod ci;
mod cloudflare;
mod cloudflare_view;
#[cfg(feature = "coderlabs")]
mod coderspace;
mod control;
#[cfg(test)]
mod design_lint;
mod dev;
mod dialogs;
mod draft;
mod durable;
mod env_name;
mod explorer;
mod extensions_view;
mod fetch;
mod gallery_view;
mod git;
mod git_dialogs;
mod github;
mod github_view;
mod headless;
mod highlight;
mod history;
mod input;
mod island;
mod island_host;
mod iso;
mod iso_view;
#[cfg(feature = "coderlabs")]
mod lab;
mod logos;
mod mascot;
mod memory;
mod memory_view;
mod model;
mod notices;
mod palette;
mod palettes;
mod phone;
mod phone_page;
mod phone_view;
mod picker;
mod platform;
mod posthog;
mod posthog_view;
mod prompts;
mod runs_view;
mod schedule;
mod server;
mod servers;
mod servers_view;
mod settings_view;
mod sheet;
mod shell;
mod sidebar;
mod signals;
#[cfg(feature = "coderlabs")]
mod site;
#[cfg(feature = "coderlabs")]
mod site_view;
mod state;
#[cfg(feature = "coderlabs")]
mod tasks_view;
#[cfg(feature = "team")]
mod team;
#[cfg(feature = "team")]
mod team_view;
mod terminal;
mod theme;
mod ui;
mod ui_zoom;
mod usage_history;
mod usage_view;
mod voice;
#[cfg(feature = "coderlabs")]
mod xlog;
mod zoom;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowOptions, point, px, size,
};

/// A shell binding, given the Windows and Linux way; see `platform::keystroke`.
/// The Cmd chords it makes on macOS work over the terminal too, all but Cmd+F,
/// which finds in the terminal there; Cmd+[ and Cmd+] leave a browser tab its
/// own back and forward.
fn bind<A: gpui::Action>(keys: &str, action: A, context: &str) -> KeyBinding {
    let keys = platform::keystroke(keys);
    let context = match keys.as_str() {
        "cmd-[" | "cmd-]" => format!("{} && !Browser", context.replace(" && !Terminal", "")),
        k if k.starts_with("cmd-") && k != "cmd-f" => context.replace(" && !Terminal", ""),
        _ => context.to_string(),
    };
    KeyBinding::new(&keys, action, Some(&context))
}

fn main() {
    browser::handle_subprocess();
    if control::run_cli() {
        return;
    }
    platform::import_login_path();
    control::export_env();
    platform::detach_own_console();
    let started = std::time::Instant::now();
    gpui::Application::with_platform(ui_zoom::platform())
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            assets::load_fonts(cx);
            #[cfg(target_os = "macos")]
            if let Some(icon) = assets::Assets::get("brand/arakun-macos.png") {
                platform::set_dock_icon(&icon.data);
            }
            cx.bind_keys([
                // Keys a shell needs (ctrl-w, ctrl-t, alt-arrows...) stay with the terminal;
                // on macOS `bind` makes most of them Cmd chords, which a shell never sees.
                bind("ctrl-j", shell::ToggleDock, "Shell && !Terminal"),
                bind("ctrl-`", shell::ToggleDock, "Shell"),
                bind("ctrl-shift-`", shell::NewTerminal, "Shell"),
                bind("ctrl-shift-t", shell::NewTerminal, "Shell"),
                bind("ctrl-t", shell::NewSession, "Shell && !Terminal"),
                bind("alt-down", shell::NextEnvironment, "Shell && !Terminal"),
                bind("alt-up", shell::PrevEnvironment, "Shell && !Terminal"),
                bind("ctrl-o", shell::AddProject, "Shell && !Terminal"),
                bind("ctrl-shift-n", shell::NewEnvironment, "Shell"),
                bind("ctrl-shift-b", shell::NewBrowserTab, "Shell"),
                bind("ctrl-w", shell::CloseTab, "Shell && !Terminal"),
                bind("ctrl-k", shell::QuickSwitch, "Shell && !Terminal"),
                bind("ctrl-p", shell::QuickSwitch, "Shell && !Terminal"),
                bind("ctrl-b", shell::ToggleSidebar, "Shell && !Terminal"),
                bind(
                    "ctrl-f",
                    shell::FindInChat,
                    "Shell && !Terminal && !Browser",
                ),
                bind("ctrl-,", shell::OpenSettings, "Shell"),
                bind("alt-left", shell::GoBackTab, "Shell && !Terminal"),
                bind("alt-right", shell::GoForwardTab, "Shell && !Terminal"),
                bind("ctrl-tab", shell::NextTab, "Shell"),
                bind("ctrl-shift-tab", shell::PrevTab, "Shell"),
                bind("ctrl-pagedown", shell::NextTab, "Shell"),
                bind("ctrl-pageup", shell::PrevTab, "Shell"),
                bind("ctrl-1", shell::SelectTab1, "Shell"),
                bind("ctrl-2", shell::SelectTab2, "Shell"),
                bind("ctrl-3", shell::SelectTab3, "Shell"),
                bind("ctrl-4", shell::SelectTab4, "Shell"),
                bind("ctrl-5", shell::SelectTab5, "Shell"),
                bind("ctrl-6", shell::SelectTab6, "Shell"),
                bind("ctrl-7", shell::SelectTab7, "Shell"),
                bind("ctrl-8", shell::SelectTab8, "Shell"),
                bind("ctrl-9", shell::SelectLastTab, "Shell"),
                bind("ctrl-d", shell::SplitRight, "Shell && !Terminal"),
                bind("ctrl-shift-d", shell::SplitDown, "Shell && !Terminal"),
                bind("ctrl-alt-d", shell::ToggleDance, "Shell && !Terminal"),
                bind("ctrl-alt-left", shell::FocusOtherPane, "Shell && !Terminal"),
                bind(
                    "ctrl-alt-right",
                    shell::FocusOtherPane,
                    "Shell && !Terminal",
                ),
                bind("ctrl-alt-up", shell::FocusOtherPane, "Shell && !Terminal"),
                bind("ctrl-alt-down", shell::FocusOtherPane, "Shell && !Terminal"),
                bind(
                    "ctrl-alt-shift-left",
                    shell::SwapPanes,
                    "Shell && !Terminal",
                ),
                bind(
                    "ctrl-alt-shift-right",
                    shell::SwapPanes,
                    "Shell && !Terminal",
                ),
                bind("ctrl-alt-shift-up", shell::SwapPanes, "Shell && !Terminal"),
                bind(
                    "ctrl-alt-shift-down",
                    shell::SwapPanes,
                    "Shell && !Terminal",
                ),
                // A browser tab keeps these for the page's own zoom.
                bind("ctrl-=", shell::ZoomIn, "Shell && !Terminal && !Browser"),
                bind("ctrl-+", shell::ZoomIn, "Shell && !Terminal && !Browser"),
                bind("ctrl--", shell::ZoomOut, "Shell && !Terminal && !Browser"),
                bind("ctrl-0", shell::ZoomReset, "Shell && !Terminal && !Browser"),
            ]);
            input::bind_keys(cx);
            palette::bind_keys(cx);
            // Pinned things from anywhere; inside the palette Ctrl+1..9 opens them too.
            cx.bind_keys([
                bind("ctrl-alt-1", palette::Pinned1, "Shell"),
                bind("ctrl-alt-2", palette::Pinned2, "Shell"),
                bind("ctrl-alt-3", palette::Pinned3, "Shell"),
                bind("ctrl-alt-4", palette::Pinned4, "Shell"),
                bind("ctrl-alt-5", palette::Pinned5, "Shell"),
                bind("ctrl-alt-6", palette::Pinned6, "Shell"),
                bind("ctrl-alt-7", palette::Pinned7, "Shell"),
                bind("ctrl-alt-8", palette::Pinned8, "Shell"),
                bind("ctrl-alt-9", palette::Pinned9, "Shell"),
            ]);
            app_menu::install(cx);
            signals::quit_on_signals(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = match dev::size() {
                // The test arakun fills its virtual screen, so screenshot pixels are window points.
                Some((w, h)) => Bounds::new(point(px(0.), px(0.)), size(px(w), px(h))),
                None => Bounds::centered(None, size(px(1280.), px(800.)), cx),
            };
            let shell = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Arakun".into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(
                                px(ui::TRAFFIC_LIGHTS.0),
                                px(ui::TRAFFIC_LIGHTS.1),
                            )),
                        }),
                        window_decorations: Some(WindowDecorations::Client),
                        window_background: WindowBackgroundAppearance::Opaque,
                        window_min_size: Some(size(px(800.), px(520.))),
                        app_id: Some("arakun".into()),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| shell::Shell::new(window, cx)),
                )
                .unwrap();
            let _ = shell.update(cx, |shell, window, cx| {
                shell.sync_island(window, cx);
                #[cfg(feature = "team")]
                shell.sync_team(window, cx);
            });
            cx.activate(true);
            if std::env::var_os("ARAKUN_TIMING").is_some() {
                eprintln!("arakun: window opened in {:?}", started.elapsed());
            }
        });
}
