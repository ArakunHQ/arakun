//! The macOS menu bar: the app menu (Settings, Services, Hide, Quit), the
//! shell's main commands, Edit, and the Window menu. macOS shows each item's
//! shortcut from the bindings in main.rs and input.rs, and Cmd+Q, Cmd+H and
//! Cmd+M only exist through it. Ctrl+Cmd+F, the older full screen chord, is
//! bound here too, so it works over the terminal.

use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, Window};

use crate::{input, shell};

gpui::actions!(
    app_menu,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        ToggleFullScreen
    ]
);

pub fn install(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| on_active_window(cx, Window::minimize_window));
    cx.on_action(|_: &Zoom, cx| on_active_window(cx, Window::zoom_window));
    cx.on_action(|_: &ToggleFullScreen, cx| on_active_window(cx, Window::toggle_fullscreen));
    cx.set_menus([
        Menu {
            name: "Arakun".into(),
            items: vec![
                MenuItem::action("Settings…", shell::OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
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
                MenuItem::action("New Environment", shell::NewEnvironment),
                MenuItem::action(
                    "New Environment with Options…",
                    shell::NewEnvironmentWithOptions,
                ),
                MenuItem::separator(),
                MenuItem::action("Close Tab", shell::CloseTab),
                MenuItem::action("Reopen Closed Tab", shell::ReopenClosedTab),
            ],
            disabled: false,
        },
        // Named "Edit" so macOS adds its own items (Emoji & Symbols, AutoFill,
        // Dictation). The items act on text fields; elsewhere they are greyed out,
        // and the terminal and browser keep their own Cmd+C and Cmd+V.
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Go to…", shell::QuickSwitch),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", shell::ToggleSidebar),
                MenuItem::action("Customize…", shell::OpenCustomize),
                MenuItem::action("Customize Sidebar…", shell::CustomizeSidebar),
                MenuItem::action("Toggle Workspace Panel", shell::TogglePanel),
                MenuItem::action("Toggle Subagents Panel", shell::ToggleSubagents),
                MenuItem::action("Toggle Terminal", shell::ToggleDock),
                MenuItem::separator(),
                MenuItem::action("Zoom In", shell::ZoomIn),
                MenuItem::action("Zoom Out", shell::ZoomOut),
                MenuItem::action("Actual Size", shell::ZoomReset),
                // macOS adds Enter Full Screen here itself, on Globe+F.
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

/// Runs `f` on the front window once the current update is over. A key press
/// or menu click reaches these actions while gpui is still updating that
/// window, and updating it again from here would fail without a word. If the
/// window closes before the deferred update runs, there is nothing left to do,
/// so that error is ignored.
fn on_active_window(cx: &mut App, f: fn(&Window)) {
    if let Some(handle) = cx.active_window() {
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, _| f(window));
        });
    }
}
