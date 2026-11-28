// The code shared with `arakun-server` and the examples (core/), named as if it were here.
use arakun_core::{
    accept, account, agent, assets, browser, caret, cli, clone_source, cloud, durable, explorer,
    fetch, frame_timer, git, git_call, guard, highlight, housekeep, input, island, mascot,
    password_manager, paths, phone, platform, poll, private_socket, redact, schedule, secret,
    secret_store, secrets, server, settings, signals, terminal, theme, ui, when, widget, zoom,
};
#[cfg(test)]
mod a11y_lint;
mod account_view;
mod app_menu;
mod arakun_teams;
mod ascii_face;
mod automations_view;
mod background;
mod branch_history;
mod branches;
mod branches_view;
#[cfg(feature = "coderlabs")]
mod canvas;
#[cfg(feature = "coderlabs")]
mod canvas_view;
mod ci;
mod cleanup;
mod cloudflare;
mod cloudflare_view;
#[cfg(feature = "coderlabs")]
mod coderspace;
mod control;
mod crash;
mod customize_view;
#[cfg(test)]
mod design_lint;
mod dev;
mod dialogs;
mod doctor;
mod doop;
mod doop_view;
mod draft;
mod env_name;
mod extensions_view;
mod flags;
mod gallery_view;
mod git_dialogs;
mod github;
mod github_view;
mod headless;
mod history;
mod http;
mod idle;
mod island_host;
mod iso;
mod iso_view;
mod keymap;
#[cfg(feature = "coderlabs")]
mod lab;
mod linear;
mod linear_view;
mod loadable;
mod logos;
mod look;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod mallinfo;
mod memory;
mod memory_view;
mod model;
mod notices;
mod onboarding;
mod page_drive;
mod palette;
mod palettes;
mod phone_view;
mod picker;
mod posthog;
mod posthog_view;
mod pr_writer;
mod profile_dialog;
mod prompts;
mod repeats;
mod running;
mod runs_view;
mod secret_mcp;
#[cfg(test)]
mod secrets_lint;
mod servers;
mod servers_view;
mod settings_registry;
mod settings_view;
mod settle;
mod sheet;
mod shell;
mod shrink;
mod sidebar;
#[cfg(feature = "coderlabs")]
mod site;
#[cfg(feature = "coderlabs")]
mod site_view;
mod state;
mod stored;
mod subnav;
mod survey_dialog;
mod surveys;
mod tab_layout;
#[cfg(feature = "coderlabs")]
mod tasks_view;
#[cfg(feature = "team")]
mod team;
#[cfg(feature = "team")]
mod team_view;
mod telemetry;
mod telemetry_dialog;
mod ui_zoom;
mod update;
mod update_dialog;
mod usage_history;
mod usage_view;
mod usage_where;
mod voice;
mod widget_blocks;
mod window_error;
#[cfg(test)]
mod words_lint;
#[cfg(feature = "coderlabs")]
mod xlog;

use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowOptions, point, px, size,
};

/// The saved window and the display to open it on; see `WindowState::place`.
fn saved_place(cx: &App) -> Option<(WindowBounds, Option<gpui::DisplayId>)> {
    let saved = state::saved_window()?;
    let displays: Vec<_> = cx
        .displays()
        .iter()
        .map(|d| (d.id(), d.uuid().ok().map(|u| u.to_string()), d.bounds()))
        .collect();
    Some(match saved.place(&displays)? {
        state::Place::At(id) => (saved.to_bounds(), Some(id)),
        state::Place::Centred(id) => {
            let visible = cx.find_display(id)?.visible_bounds();
            (saved.centred_in(visible), Some(id))
        }
    })
}

fn main() {
    arakun_core::host::set(arakun_core::host::Host {
        memory_prompt: memory::prompt_for,
        memory_root: memory::root,
        sandbox_socket: control::sandbox_socket_path,
        shim_dir: control::shim_dir,
        github_read_only: github::pr::READ_ONLY,
        idle_for: idle::idle_for,
        secret_mcp: secret_mcp::command,
    });
    browser::handle_subprocess();
    if control::run_cli() {
        return;
    }
    platform::import_login_path();
    control::export_env();
    platform::refresh_login_path_async();
    platform::detach_own_console();
    // After the CLI: `arakun <command>` runs in agents' shells and reports nothing.
    crash::install();
    // After install, so a panic from here on leaves the marker; reads the last run's first.
    crash::mark_running();
    // Sends only what the user agreed to; the first launch asks (`ask_telemetry`).
    telemetry::init();
    telemetry::capture(telemetry::events::APP_LAUNCHED, &[]);
    update::clean_up();
    let started = std::time::Instant::now();
    // Without a display server gpui runs headless, and nothing would ever show.
    #[cfg(target_os = "linux")]
    if let Err(e) = window_error::check_display() {
        window_error::exit(&e);
    }
    let platform =
        window_error::catch(ui_zoom::platform).unwrap_or_else(|e| window_error::exit(&e));
    let app = gpui::Application::with_platform(platform).with_assets(assets::Assets);
    // A Dock click (or `open -a Arakun`) while no window shows, say the window
    // is minimized. Without a window there is nothing to bring back, so a new
    // Arakun takes over once this one has quit.
    app.on_reopen(|cx| {
        let shell = cx
            .windows()
            .into_iter()
            .find_map(|w| w.downcast::<shell::Shell>());
        let shown = shell.is_some_and(|shell| {
            shell
                .update(cx, |_, window, _| background::show(window))
                .is_ok()
        });
        if !shown {
            if let Err(e) = platform::relaunch() {
                eprintln!("arakun: could not start again: {e}");
            }
            cx.quit();
        }
    });
    app.run(move |cx: &mut App| {
        assets::load_fonts(cx);
        terminal::on_link(shell::open_terminal_link);
        #[cfg(target_os = "macos")]
        if let Some(icon) = assets::Assets::get("brand/arakun-macos.png") {
            platform::set_dock_icon(&icon.data);
        }
        keymap::bind_keys(cx);
        input::bind_keys(cx);
        app_menu::install(cx);
        signals::quit_on_signals(cx);
        // Registered before any other quit step, so the deadline covers them all.
        cx.on_app_quit(|_| {
            signals::end_if_stuck();
            async {}
        })
        .detach();
        ui::motion::follow_system(cx);
        ui::contrast::follow_system(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                // Now: the shell saves as it is released, which can come before
                // the quit steps that would start the deadline.
                signals::end_if_stuck();
                cx.quit();
            }
        })
        .detach();
        let mut display_id = None;
        let bounds = match dev::size() {
            // The test arakun fills its virtual screen, so screenshot pixels are window points.
            Some((w, h)) => {
                WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(w), px(h))))
            }
            // Where it was last time, unless that screen is gone.
            None => match saved_place(cx) {
                Some((bounds, display)) => {
                    display_id = display;
                    bounds
                }
                None => {
                    WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(800.)), cx))
                }
            },
        };
        let shell = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    display_id,
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
            // Fails without a usable GPU driver; say so instead of panicking.
            .unwrap_or_else(|e| window_error::exit(&format!("{e:#}")));
        let _ = shell.update(cx, |shell, window, cx| {
            // The setup asks about reports itself; the dialog is for everyone else.
            shell.onboard_if_new(window, cx);
            shell.ask_telemetry(window, cx);
            shell.ask_after_crash(cx);
            shell.sync_island(window, cx);
            shell.start_survey_card(window, cx);
            #[cfg(feature = "team")]
            shell.sync_team(window, cx);
            // After the first frame, so warming the syntax colors doesn't
            // compete with it, and off the UI thread, so no render waits on it.
            window.on_next_frame(|_, cx| {
                cx.background_executor()
                    .spawn(async { highlight::warm() })
                    .detach();
            });
        });
        // A click on one of Arakun's notifications shows what it was about, as
        // "View" on the island does. Outside a bundle they come from osascript.
        #[cfg(target_os = "macos")]
        if platform::in_app_bundle() {
            cx.on_system_notification_response(move |response, cx| {
                cx.activate(true);
                let _ = shell.update(cx, |shell, window, cx| {
                    shell.open_from_island(response.tag, window, cx)
                });
            });
        }
        // The test arakun of `arakun dev` shares the user's desktop on macOS:
        // taking the focus would send their typing into it. Its input comes
        // through the control socket instead.
        if !(cfg!(target_os = "macos") && dev::size().is_some()) {
            cx.activate(true);
        }
        if std::env::var_os("ARAKUN_TIMING").is_some() {
            eprintln!("arakun: window opened in {:?}", started.elapsed());
        }
    });
}
