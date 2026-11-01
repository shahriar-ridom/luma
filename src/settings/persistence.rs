//! Loads and saves `Preferences` through the database.

use crate::database::Database;
use crate::settings::preferences::Preferences;

const SHORTCUT: &str = "shortcut.";

pub fn load(db: &Database) -> Preferences {
    let p = Preferences::default();
    for (k, v) in db.preferences().unwrap_or_default() {
        if let Some(id) = k.strip_prefix(SHORTCUT) {
            p.set_override(id, &v);
        } else if k == "default_view" {
            *p.default_view.borrow_mut() = v;
        } else if k == "git_off" {
            *p.git_off.borrow_mut() = v == "1";
        } else if let Some(id) = k.strip_prefix("plugin.").and_then(|r| r.strip_suffix(".enabled")) {
            if v == "1" {
                p.plugins_on.borrow_mut().insert(id.to_string());
            }
        } else if k == "terminal" {
            *p.terminal.borrow_mut() = v;
        } else if k == "color_scheme" {
            *p.color_scheme.borrow_mut() = v;
        } else if k == "show_hidden" {
            *p.show_hidden.borrow_mut() = v == "1";
        }
    }
    p
}

pub fn save_shortcut(db: &Database, prefs: &Preferences, id: &str, accel: &str) {
    prefs.set_override(id, accel);
    if let Err(e) = db.set_preference(&format!("{SHORTCUT}{id}"), accel) {
        tracing::warn!("saving shortcut: {e:#}");
    }
}

pub fn reset_shortcut(db: &Database, prefs: &Preferences, id: &str) {
    prefs.clear_override(id);
    if let Err(e) = db.delete_preference(&format!("{SHORTCUT}{id}")) {
        tracing::warn!("resetting shortcut: {e:#}");
    }
}

pub fn save_view(db: &Database, prefs: &Preferences, view: &str) {
    *prefs.default_view.borrow_mut() = view.to_string();
    let _ = db.set_preference("default_view", view);
}

pub fn save_hidden(db: &Database, prefs: &Preferences, hidden: bool) {
    *prefs.show_hidden.borrow_mut() = hidden;
    let _ = db.set_preference("show_hidden", if hidden { "1" } else { "0" });
}

pub fn save_color_scheme(db: &Database, prefs: &Preferences, scheme: &str) {
    *prefs.color_scheme.borrow_mut() = scheme.to_string();
    let _ = db.set_preference("color_scheme", scheme);
}

pub fn save_git_enabled(db: &Database, prefs: &Preferences, on: bool) {
    *prefs.git_off.borrow_mut() = !on;
    let _ = db.set_preference("git_off", if on { "0" } else { "1" });
}

pub fn save_plugin_enabled(db: &Database, prefs: &Preferences, id: &str, on: bool) {
    if on {
        prefs.plugins_on.borrow_mut().insert(id.to_string());
    } else {
        prefs.plugins_on.borrow_mut().remove(id);
    }
    let _ = db.set_preference(&format!("plugin.{id}.enabled"), if on { "1" } else { "0" });
}
