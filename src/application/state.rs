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
        let data = gtk::glib::user_data_dir();
        migrate_legacy_data(&data);
        let path = data.join("luma").join("luma.sqlite");
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
        let dirs: Vec<PathBuf> = vec![user_plugin_dir(), PathBuf::from("/usr/share/luma/plugins")];
        *self.ext.borrow_mut() = Extensions::load(&custom, &dirs, |id| self.prefs.plugin_enabled(id));
    }
}

/// Before the app was called Luma its data lived in `file-manager/` (database
/// `fm.sqlite`). Carry the database and plugins over once so nothing is lost.
/// Google Drive mounts are left alone; they are recreated on demand.
pub fn migrate_legacy_data(data_home: &std::path::Path) {
    let (old, new) = (data_home.join("file-manager"), data_home.join("luma"));
    if !old.is_dir() || new.join("luma.sqlite").exists() {
        return;
    }
    if std::fs::create_dir_all(&new).is_err() {
        return;
    }
    for (from, to) in
        [("fm.sqlite", "luma.sqlite"), ("fm.sqlite-wal", "luma.sqlite-wal"), ("fm.sqlite-shm", "luma.sqlite-shm")]
    {
        if old.join(from).exists() {
            let _ = std::fs::rename(old.join(from), new.join(to));
        }
    }
    if old.join("plugins").is_dir() && !new.join("plugins").exists() {
        let _ = std::fs::rename(old.join("plugins"), new.join("plugins"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn legacy_data_moves_once_and_never_overwrites() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/migrate");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("file-manager/plugins/p")).unwrap();
        fs::write(base.join("file-manager/fm.sqlite"), "old-db").unwrap();
        migrate_legacy_data(&base);
        assert_eq!(fs::read_to_string(base.join("luma/luma.sqlite")).unwrap(), "old-db");
        assert!(base.join("luma/plugins/p").is_dir());
        assert!(!base.join("file-manager/fm.sqlite").exists());
        // A second run (or a fresh Luma database) is never replaced.
        fs::write(base.join("file-manager/fm.sqlite"), "newer-old").unwrap();
        migrate_legacy_data(&base);
        assert_eq!(fs::read_to_string(base.join("luma/luma.sqlite")).unwrap(), "old-db");
    }
}
