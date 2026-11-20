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
        scrub_saved_connections(&db);
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

/// Older versions saved `sftp://user:password@host` as typed, in the database
/// and as the sidebar label. Rewrite those rows without the password.
pub fn scrub_saved_connections(db: &Database) {
    use crate::integrations::network::{is_generated_label, label_for, strip_uri_password};
    let mut changed = false;
    for c in db.connections().unwrap_or_default() {
        let clean = strip_uri_password(&c.uri);
        if clean == c.uri {
            continue;
        }
        // A generated name carried the password too; one the user chose is left alone.
        let name = if is_generated_label(&c.name, &c.uri) { label_for(&clean) } else { c.name.clone() };
        match db.update_connection(c.id, &name, &clean) {
            Ok(()) => changed = true,
            Err(e) => tracing::warn!("could not remove a saved password: {e:#}"),
        }
    }
    if changed {
        db.purge_deleted(); // the old text must not stay in free pages or the WAL
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
    fn saved_passwords_are_scrubbed_at_startup() {
        let db = Database::open_in_memory().unwrap();
        db.add_connection("me:hunter2@box/srv (SFTP)", "sftp://me:hunter2@box/srv").unwrap();
        db.add_connection("My NAS", "smb://u:pw@nas/share").unwrap();
        db.add_connection("dup (SFTP)", "sftp://me@dup/x").unwrap();
        db.add_connection("dup with pw", "sftp://me:pw@dup/x").unwrap();
        db.add_connection("fine", "ftp://host").unwrap();
        scrub_saved_connections(&db);
        let rows: Vec<(String, String)> = db.connections().unwrap().into_iter().map(|c| (c.name, c.uri)).collect();
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            rows,
            [
                s("me@box/srv (SFTP)", "sftp://me@box/srv"),
                s("My NAS", "smb://u@nas/share"),
                s("dup (SFTP)", "sftp://me@dup/x"),
                s("fine", "ftp://host"),
            ]
        );
        scrub_saved_connections(&db); // idempotent
        assert_eq!(db.connections().unwrap().len(), 4);
    }

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
