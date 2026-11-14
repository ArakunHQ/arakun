//! The macOS menu bar: the app menu (Settings, Hide, Quit), the shell's main
//! commands and the Window menu. macOS shows each item's shortcut from the
//! bindings in main.rs, and Cmd+Q, Cmd+H and Cmd+M only exist through it.

use gpui::{App, KeyBinding, Menu, MenuItem};

use crate::shell;

gpui::actions!(app_menu, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom]);

pub fn install(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });
    cx.set_menus([
        Menu {
            name: "Arakun".into(),
            items: vec![
                MenuItem::action("Settings…", shell::OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Hide Arakun", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Arakun", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Session", shell::NewSession),
                MenuItem::action("New Terminal", shell::NewTerminal),
                MenuItem::action("New Browser Tab", shell::NewBrowserTab),
                MenuItem::separator(),
                MenuItem::action("Add Project…", shell::AddProject),
                MenuItem::action("New Environment…", shell::NewEnvironment),
                MenuItem::separator(),
                MenuItem::action("Close Tab", shell::CloseTab),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Go to…", shell::QuickSwitch),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", shell::ToggleSidebar),
                MenuItem::action("Customize Sidebar…", shell::CustomizeSidebar),
                MenuItem::action("Toggle Terminal", shell::ToggleDock),
                MenuItem::separator(),
                MenuItem::action("Zoom In", shell::ZoomIn),
                MenuItem::action("Zoom Out", shell::ZoomOut),
                MenuItem::action("Actual Size", shell::ZoomReset),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
            disabled: false,
        },
    ]);
}
