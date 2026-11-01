use std::{cell::RefCell, path::PathBuf, rc::Rc};

use crate::integrations::plugins::{Extensions, user_plugin_dir};

use crate::core::operations::undo::UndoStack;
use crate::database::Database;
use crate::settings::{persistence, preferences::Preferences};

/// Application-wide state shared by all windows (main thread only).
pub struct AppState {
    pub db: Database,
    pub prefs: Preferences,
    /// Reversible actions, most recent last (shared by all windows).
    pub undo: UndoStack,
    /// Custom actions + enabled plugins; rebuilt by `reload_extensions`.
    pub ext: RefCell<Extensions>,
}

impl AppState {
    pub fn open() -> Rc<Self> {
        let path = gtk::glib::user_data_dir().join("file-manager").join("fm.sqlite");
        let db = Database::open(&path).unwrap_or_else(|e| {
            tracing::warn!("database unavailable ({e:#}); using a temporary in-memory one");
            Database::open_in_memory().expect("in-memory sqlite")
        });
        let prefs = persistence::load(&db);
        let s = Rc::new(Self { db, prefs, undo: UndoStack::default(), ext: RefCell::default() });
        s.reload_extensions();
        s
    }

    /// Re-read custom actions and plugins (after the manager changed them).
    pub fn reload_extensions(&self) {
        let custom = self.db.custom_actions().unwrap_or_default();
        let dirs: Vec<PathBuf> = vec![user_plugin_dir(), PathBuf::from("/usr/share/file-manager/plugins")];
        *self.ext.borrow_mut() = Extensions::load(&custom, &dirs, |id| self.prefs.plugin_enabled(id));
    }
}
