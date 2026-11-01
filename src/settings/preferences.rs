use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

use crate::application::actions::ACTIONS;

/// User preferences held in memory; persisted by `persistence`.
#[derive(Default)]
pub struct Preferences {
    /// action id → accelerator override ("" = explicitly unbound).
    shortcuts: RefCell<HashMap<String, String>>,
    pub default_view: RefCell<String>,
    pub show_hidden: RefCell<bool>,
    /// "default" | "light" | "dark"
    pub color_scheme: RefCell<String>,
    /// Terminal emulator command; empty = auto-detect.
    pub terminal: RefCell<String>,
    /// Show Git status in repositories (default on).
    pub git_off: RefCell<bool>,
    /// Plugins the user has switched on.
    pub plugins_on: RefCell<HashSet<String>>,
}

impl Preferences {
    pub fn git_enabled(&self) -> bool {
        !*self.git_off.borrow()
    }
    pub fn plugin_enabled(&self, id: &str) -> bool {
        self.plugins_on.borrow().contains(id)
    }
}

impl Preferences {
    pub fn accel(&self, id: &str) -> String {
        if let Some(a) = self.shortcuts.borrow().get(id) {
            return a.clone();
        }
        ACTIONS.iter().find(|a| a.id == id).map(|a| a.default.to_string()).unwrap_or_default()
    }

    pub fn set_override(&self, id: &str, accel: &str) {
        self.shortcuts.borrow_mut().insert(id.to_string(), accel.to_string());
    }

    pub fn clear_override(&self, id: &str) {
        self.shortcuts.borrow_mut().remove(id);
    }

    pub fn is_overridden(&self, id: &str) -> bool {
        self.shortcuts.borrow().contains_key(id)
    }

    /// Another action already using `accel`, if any.
    pub fn conflict(&self, id: &str, accel: &str) -> Option<&'static str> {
        if accel.is_empty() {
            return None;
        }
        ACTIONS.iter().find(|a| a.id != id && self.accel(a.id) == accel).map(|a| a.id)
    }

    pub fn view(&self) -> String {
        let v = self.default_view.borrow();
        if v.is_empty() { "list".into() } else { v.clone() }
    }
}
