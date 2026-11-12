//! Application menubar, exported on the session bus for global-menu hosts.
//!
//! GTK4 publishes the model given to `Application::set_menubar` as
//! `org.gtk.Menus` at `/dev/ridom/Luma/menus/menubar`, and the window's
//! actions as `org.gtk.Actions`. Menu hosts (e.g. KDE's gmenudbusmenuproxy)
//! pick it up from there. Nothing is drawn inside the window.
//!
//! Every entry targets an existing `win.*` action, so enabled/disabled state
//! follows the focused window and the selection automatically.

use gtk::{gio, prelude::*};

use crate::application::state::AppState;

fn item(label: &str, id: &str, state: &AppState) -> gio::MenuItem {
    let it = gio::MenuItem::new(Some(label), Some(&format!("win.{id}")));
    let accel = state.prefs.accel(id);
    if !accel.is_empty() {
        // Display hint for the host; Luma's own shortcut controllers do the
        // actual key handling, so no GTK accelerator is registered.
        it.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    it
}

fn section(state: &AppState, entries: &[(&str, &str)]) -> gio::Menu {
    let m = gio::Menu::new();
    for (label, id) in entries {
        m.append_item(&item(label, id, state));
    }
    m
}

pub fn build(state: &AppState) -> gio::Menu {
    let bar = gio::Menu::new();

    let file = gio::Menu::new();
    file.append_section(
        None,
        &section(
            state,
            &[
                ("New Window", "new-window"),
                ("New Tab", "new-tab"),
                ("Close Tab", "close-tab"),
                ("Reopen Closed Tab", "reopen-tab"),
            ],
        ),
    );
    file.append_section(
        None,
        &section(
            state,
            &[("New Folder", "new-folder"), ("New File", "new-file"), ("Open", "open"), ("Open With…", "open-with")],
        ),
    );
    file.append_section(
        None,
        &section(
            state,
            &[
                ("Rename…", "rename"),
                ("Compress…", "compress"),
                ("Extract Here", "extract-here"),
                ("Extract To…", "extract-to"),
                ("Browse Archive…", "browse-archive"),
                ("Tags…", "tags"),
                ("Properties", "properties"),
            ],
        ),
    );
    file.append_section(None, &section(state, &[("Move to Trash", "trash"), ("Delete Permanently…", "delete")]));
    bar.append_submenu(Some("_File"), &file);

    let edit = gio::Menu::new();
    edit.append_section(None, &section(state, &[("Undo", "undo")]));
    edit.append_section(
        None,
        &section(
            state,
            &[
                ("Cut", "cut"),
                ("Copy", "copy"),
                ("Paste", "paste"),
                ("Select All", "select-all"),
                ("Copy Path", "copy-path"),
            ],
        ),
    );
    edit.append_section(None, &section(state, &[("Find", "find"), ("Search Files…", "search")]));
    edit.append_section(
        None,
        &section(
            state,
            &[
                ("Open Terminal Here", "open-terminal"),
                ("Compare Files…", "compare"),
                ("Find Duplicate Files…", "find-duplicates"),
            ],
        ),
    );
    bar.append_submenu(Some("_Edit"), &edit);

    let view = gio::Menu::new();
    view.append_section(
        None,
        &section(
            state,
            &[("Grid", "view-grid"), ("List", "view-list"), ("Columns", "view-column"), ("Gallery", "view-gallery")],
        ),
    );
    let appearance = gio::Menu::new();
    for (label, val) in [("System", "default"), ("Light", "light"), ("Dark", "dark")] {
        let it = gio::MenuItem::new(Some(label), None);
        it.set_action_and_target_value(Some("win.color-scheme"), Some(&val.to_variant()));
        appearance.append_item(&it);
    }
    let toggles = section(
        state,
        &[
            ("Show Hidden Files", "toggle-hidden"),
            ("Toggle Sidebar", "toggle-sidebar"),
            ("Split View", "split-view"),
            ("Quick Look", "quick-preview"),
        ],
    );
    toggles.append(Some("Show Git Status"), Some("win.git-enabled"));
    view.append_section(None, &toggles);
    let theme = gio::Menu::new();
    theme.append_submenu(Some("Appearance"), &appearance);
    view.append_section(None, &theme);
    view.append_section(None, &section(state, &[("Refresh", "refresh")]));
    bar.append_submenu(Some("_View"), &view);

    let go = gio::Menu::new();
    go.append_section(
        None,
        &section(
            state,
            &[
                ("Back", "back"),
                ("Forward", "forward"),
                ("Enclosing Folder", "parent"),
                ("Location…", "focus-location"),
            ],
        ),
    );
    go.append_section(
        None,
        &section(
            state,
            &[
                ("Add to Favorites", "bookmark-add"),
                ("Connect to Server…", "connect-server"),
                ("Storage & Disk Usage…", "storage"),
            ],
        ),
    );
    bar.append_submenu(Some("_Go"), &go);

    let window = gio::Menu::new();
    window.append_section(
        None,
        &section(state, &[("Next Tab", "next-tab"), ("Previous Tab", "prev-tab"), ("Duplicate Tab", "duplicate-tab")]),
    );
    window.append_section(None, &section(state, &[("Custom Actions & Plugins…", "extensions")]));
    bar.append_submenu(Some("_Window"), &window);

    let help = section(state, &[("Keyboard Shortcuts", "shortcuts"), ("About Luma", "about")]);
    bar.append_submenu(Some("_Help"), &help);

    bar
}

/// (Re)publish the menubar; call again whenever shortcuts change.
pub fn install(app: &gtk::Application, state: &AppState) {
    app.set_menubar(Some(&build(state)));
}
