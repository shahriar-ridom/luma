use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::Database;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub id: i64,
    pub name: String,
    pub path: PathBuf,
}

/// Milliseconds; callers also force strict monotonic ordering in SQL.
fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn p2s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedConnection {
    pub id: i64,
    pub name: String,
    pub uri: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomActionRow {
    pub id: i64,
    pub name: String,
    pub command: String,
    pub patterns: String,
    pub wait: bool,
}

const MAX_RECENT_FILES: i64 = 200;
const MAX_RECENT_LOCATIONS: i64 = 200;

impl Database {
    // ---- bookmarks -------------------------------------------------------

    pub fn bookmarks(&self) -> Result<Vec<Bookmark>> {
        let mut st = self.conn.prepare("SELECT id, name, path FROM bookmarks ORDER BY position, id")?;
        let rows = st.query_map([], |r| {
            Ok(Bookmark { id: r.get(0)?, name: r.get(1)?, path: PathBuf::from(r.get::<_, String>(2)?) })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Adds a bookmark at the end; returns false if the path is already bookmarked.
    pub fn add_bookmark(&self, name: &str, path: &Path) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO bookmarks (name, path, position)
             VALUES (?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM bookmarks), 0))",
            params![name, p2s(path)],
        )?;
        Ok(n > 0)
    }

    pub fn rename_bookmark(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE bookmarks SET name = ?1 WHERE id = ?2", params![name, id])?;
        Ok(())
    }

    pub fn remove_bookmark(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM bookmarks WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Move bookmark `id` so it sits immediately before `before`.
    pub fn move_bookmark_before(&self, id: i64, before: i64) -> Result<()> {
        if id == before {
            return Ok(());
        }
        let mut ids: Vec<i64> = self.bookmarks()?.into_iter().map(|b| b.id).filter(|b| *b != id).collect();
        let at = ids.iter().position(|b| *b == before).unwrap_or(ids.len());
        ids.insert(at, id);
        let tx = self.conn.unchecked_transaction()?;
        for (pos, id) in ids.iter().enumerate() {
            tx.execute("UPDATE bookmarks SET position = ?1 WHERE id = ?2", params![pos as i64, id])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- custom actions --------------------------------------------------------

    pub fn custom_actions(&self) -> Result<Vec<CustomActionRow>> {
        let mut st =
            self.conn.prepare("SELECT id, name, command, patterns, wait FROM custom_actions ORDER BY position, id")?;
        let rows = st.query_map([], |r| {
            Ok(CustomActionRow {
                id: r.get(0)?,
                name: r.get(1)?,
                command: r.get(2)?,
                patterns: r.get(3)?,
                wait: r.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Insert (`id == 0`) or update an action; returns its id.
    pub fn save_custom_action(&self, a: &CustomActionRow) -> Result<i64> {
        if a.id == 0 {
            self.conn.execute(
                "INSERT INTO custom_actions (name, command, patterns, wait, position)
                 VALUES (?1, ?2, ?3, ?4, COALESCE((SELECT MAX(position) + 1 FROM custom_actions), 0))",
                params![a.name, a.command, a.patterns, a.wait as i64],
            )?;
            Ok(self.conn.last_insert_rowid())
        } else {
            self.conn.execute(
                "UPDATE custom_actions SET name = ?2, command = ?3, patterns = ?4, wait = ?5 WHERE id = ?1",
                params![a.id, a.name, a.command, a.patterns, a.wait as i64],
            )?;
            Ok(a.id)
        }
    }

    pub fn delete_custom_action(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM custom_actions WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- file tags -------------------------------------------------------------

    /// Tags of the direct children of `dir`, keyed by file name.
    pub fn tags_in_dir(&self, dir: &Path) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let prefix = format!("{}/", p2s(dir).trim_end_matches('/'));
        let upper = format!("{}0", prefix.trim_end_matches('/')); // '0' is '/' + 1
        let mut st =
            self.conn.prepare("SELECT path, tag FROM file_tags WHERE path >= ?1 AND path < ?2 ORDER BY tag")?;
        let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
        for row in st.query_map(params![prefix, upper], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (path, tag) = row?;
            if let Some(name) = path.strip_prefix(&prefix).filter(|n| !n.is_empty() && !n.contains('/')) {
                out.entry(name.to_string()).or_default().push(tag);
            }
        }
        Ok(out)
    }

    pub fn tags_of(&self, path: &Path) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT tag FROM file_tags WHERE path = ?1 ORDER BY tag")?;
        let rows = st.query_map([p2s(path)], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn add_tags(&self, paths: &[PathBuf], tags: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for p in paths {
            for t in tags {
                tx.execute("INSERT OR IGNORE INTO file_tags (path, tag) VALUES (?1, ?2)", params![p2s(p), t])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_tags(&self, paths: &[PathBuf], tags: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for p in paths {
            for t in tags {
                tx.execute("DELETE FROM file_tags WHERE path = ?1 AND tag = ?2", params![p2s(p), t])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// All tags with how many items carry each.
    pub fn all_tags(&self) -> Result<Vec<(String, u32)>> {
        let mut st =
            self.conn.prepare("SELECT tag, COUNT(*) FROM file_tags GROUP BY tag ORDER BY tag COLLATE NOCASE")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn files_with_tag(&self, tag: &str) -> Result<Vec<PathBuf>> {
        let mut st = self.conn.prepare("SELECT path FROM file_tags WHERE tag = ?1 ORDER BY path")?;
        let rows = st.query_map([tag], |r| Ok(PathBuf::from(r.get::<_, String>(0)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Keep tags attached when `old` (a file or folder) moves to `new`.
    pub fn move_tags(&self, old: &Path, new: &Path) -> Result<()> {
        let (o, n) = (p2s(old), p2s(new));
        self.conn.execute(
            "UPDATE OR IGNORE file_tags SET path = ?2 || substr(path, length(?1) + 1)
             WHERE path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/'",
            params![o, n],
        )?;
        Ok(())
    }

    /// Forget tags of a deleted item (and anything inside it).
    pub fn remove_tags_under(&self, path: &Path) -> Result<()> {
        let o = p2s(path);
        self.conn
            .execute("DELETE FROM file_tags WHERE path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/'", [o])?;
        Ok(())
    }

    // ---- saved network connections ------------------------------------------

    pub fn connections(&self) -> Result<Vec<SavedConnection>> {
        let mut st = self.conn.prepare("SELECT id, name, uri FROM connections ORDER BY position, id")?;
        let rows = st.query_map([], |r| Ok(SavedConnection { id: r.get(0)?, name: r.get(1)?, uri: r.get(2)? }))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Returns false if this address is already saved.
    pub fn add_connection(&self, name: &str, uri: &str) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO connections (name, uri, position)
             VALUES (?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM connections), 0))",
            params![name, uri],
        )?;
        Ok(n > 0)
    }

    pub fn remove_connection(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM connections WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- recents ---------------------------------------------------------

    pub fn record_location(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recent_locations (path, visited_at, visits)
             VALUES (?1, MAX(?2, COALESCE((SELECT MAX(visited_at) + 1 FROM recent_locations), 0)), 1)
             ON CONFLICT(path) DO UPDATE SET
               visited_at = MAX(?2, (SELECT MAX(visited_at) + 1 FROM recent_locations)), visits = visits + 1",
            params![p2s(path), now_ms()],
        )?;
        self.conn.execute(
            "DELETE FROM recent_locations WHERE path NOT IN
             (SELECT path FROM recent_locations ORDER BY visited_at DESC LIMIT ?1)",
            [MAX_RECENT_LOCATIONS],
        )?;
        Ok(())
    }

    pub fn recent_locations(&self, limit: usize) -> Result<Vec<PathBuf>> {
        let mut st =
            self.conn.prepare("SELECT path FROM recent_locations ORDER BY visited_at DESC, rowid DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok(PathBuf::from(r.get::<_, String>(0)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn record_file(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recent_files (path, opened_at)
             VALUES (?1, MAX(?2, COALESCE((SELECT MAX(opened_at) + 1 FROM recent_files), 0)))
             ON CONFLICT(path) DO UPDATE SET
               opened_at = MAX(?2, (SELECT MAX(opened_at) + 1 FROM recent_files))",
            params![p2s(path), now_ms()],
        )?;
        self.conn.execute(
            "DELETE FROM recent_files WHERE path NOT IN
             (SELECT path FROM recent_files ORDER BY opened_at DESC LIMIT ?1)",
            [MAX_RECENT_FILES],
        )?;
        Ok(())
    }

    pub fn recent_files(&self, limit: usize) -> Result<Vec<PathBuf>> {
        let mut st = self.conn.prepare("SELECT path FROM recent_files ORDER BY opened_at DESC, rowid DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok(PathBuf::from(r.get::<_, String>(0)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- search history ----------------------------------------------------

    pub fn record_search(&self, query: &str) -> Result<()> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO saved_searches (query, used_at)
             VALUES (?1, MAX(?2, COALESCE((SELECT MAX(used_at) + 1 FROM saved_searches), 0)))
             ON CONFLICT(query) DO UPDATE SET used_at = MAX(?2, (SELECT MAX(used_at) + 1 FROM saved_searches))",
            params![q, now_ms()],
        )?;
        self.conn.execute(
            "DELETE FROM saved_searches WHERE query NOT IN (SELECT query FROM saved_searches ORDER BY used_at DESC LIMIT 50)",
            [],
        )?;
        Ok(())
    }

    pub fn search_history(&self, limit: usize) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT query FROM saved_searches ORDER BY used_at DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- session (tabs) ----------------------------------------------------

    pub fn save_session(&self, paths: &[PathBuf], active: usize) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM tabs", [])?;
        for (i, p) in paths.iter().enumerate() {
            tx.execute(
                "INSERT INTO tabs (position, path, active) VALUES (?1, ?2, ?3)",
                params![i as i64, p2s(p), (i == active) as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_session(&self) -> Result<(Vec<PathBuf>, usize)> {
        let mut st = self.conn.prepare("SELECT path, active FROM tabs ORDER BY position")?;
        let rows: Vec<(String, bool)> =
            st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))?.collect::<std::result::Result<_, _>>()?;
        let active = rows.iter().position(|(_, a)| *a).unwrap_or(0);
        Ok((rows.into_iter().map(|(p, _)| PathBuf::from(p)).collect(), active))
    }

    // ---- key/value tables ------------------------------------------------

    pub fn window_state(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM window_state WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_window_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO window_state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn preferences(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare("SELECT key, value FROM user_preferences")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO user_preferences (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_preference(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM user_preferences WHERE key = ?1", [key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn bookmarks_add_dedupe_reorder_rename_remove() {
        let db = db();
        assert!(db.add_bookmark("A", Path::new("/a")).unwrap());
        assert!(db.add_bookmark("B", Path::new("/b")).unwrap());
        assert!(db.add_bookmark("C", Path::new("/c")).unwrap());
        assert!(!db.add_bookmark("A2", Path::new("/a")).unwrap());
        let ids: Vec<_> = db.bookmarks().unwrap().iter().map(|b| b.id).collect();
        db.move_bookmark_before(ids[2], ids[0]).unwrap();
        let names: Vec<_> = db.bookmarks().unwrap().into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["C", "A", "B"]);
        db.rename_bookmark(ids[0], "Alpha").unwrap();
        db.remove_bookmark(ids[1]).unwrap();
        let names: Vec<_> = db.bookmarks().unwrap().into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["C", "Alpha"]);
    }

    #[test]
    fn recents_order_and_session_roundtrip() {
        let db = db();
        db.record_location(Path::new("/x")).unwrap();
        db.record_location(Path::new("/y")).unwrap();
        db.record_location(Path::new("/x")).unwrap();
        assert_eq!(db.recent_locations(5).unwrap(), [PathBuf::from("/x"), PathBuf::from("/y")]);
        db.record_file(Path::new("/f.txt")).unwrap();
        assert_eq!(db.recent_files(5).unwrap(), [PathBuf::from("/f.txt")]);
        db.save_session(&["/a".into(), "/b".into()], 1).unwrap();
        assert_eq!(db.load_session().unwrap(), (vec![PathBuf::from("/a"), PathBuf::from("/b")], 1));
    }

    #[test]
    fn search_history_dedupes_and_orders() {
        let db = db();
        db.record_search("a").unwrap();
        db.record_search("b").unwrap();
        db.record_search("a").unwrap();
        db.record_search("  ").unwrap();
        assert_eq!(db.search_history(10).unwrap(), ["a", "b"]);
    }

    #[test]
    fn custom_actions_crud() {
        let db = db();
        let id = db
            .save_custom_action(&CustomActionRow {
                id: 0,
                name: "Edit".into(),
                command: "code %f".into(),
                patterns: "files".into(),
                wait: false,
            })
            .unwrap();
        let mut a = db.custom_actions().unwrap().remove(0);
        assert_eq!((a.id, a.name.as_str()), (id, "Edit"));
        a.name = "Edit in code".into();
        a.wait = true;
        db.save_custom_action(&a).unwrap();
        assert_eq!(db.custom_actions().unwrap(), [a]);
        db.delete_custom_action(id).unwrap();
        assert!(db.custom_actions().unwrap().is_empty());
    }

    #[test]
    fn tags_roundtrip_and_follow_moves() {
        let db = db();
        let (a, b) = (PathBuf::from("/d/a.txt"), PathBuf::from("/d/sub/b.txt"));
        db.add_tags(&[a.clone(), b.clone()], &["work".into(), "Red".into()]).unwrap();
        db.add_tags(std::slice::from_ref(&a), &["work".into()]).unwrap(); // idempotent
        let in_d = db.tags_in_dir(Path::new("/d")).unwrap();
        assert_eq!(in_d["a.txt"], ["Red", "work"]);
        assert!(!in_d.contains_key("b.txt"), "only direct children");
        assert_eq!(db.all_tags().unwrap(), [("Red".to_string(), 2), ("work".to_string(), 2)]);
        // A sibling whose name merely starts with the folder name is not under it.
        db.add_tags(&[PathBuf::from("/d/subway.txt")], &["x".into()]).unwrap();
        db.move_tags(Path::new("/d/sub"), Path::new("/e/moved")).unwrap();
        assert_eq!(db.tags_of(Path::new("/e/moved/b.txt")).unwrap(), ["Red", "work"]);
        assert_eq!(db.tags_of(Path::new("/d/subway.txt")).unwrap(), ["x"]);
        db.remove_tags(std::slice::from_ref(&a), &["work".into()]).unwrap();
        assert_eq!(db.files_with_tag("work").unwrap(), [PathBuf::from("/e/moved/b.txt")]);
        db.remove_tags_under(Path::new("/e")).unwrap();
        assert!(db.tags_of(Path::new("/e/moved/b.txt")).unwrap().is_empty());
    }

    #[test]
    fn saved_connections() {
        let db = db();
        assert!(db.add_connection("NAS", "smb://nas/share").unwrap());
        assert!(!db.add_connection("dup", "smb://nas/share").unwrap());
        assert!(db.add_connection("Box", "sftp://me@box").unwrap());
        let c = db.connections().unwrap();
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["NAS", "Box"]);
        db.remove_connection(c[0].id).unwrap();
        assert_eq!(db.connections().unwrap().len(), 1);
    }

    #[test]
    fn kv_tables() {
        let db = db();
        db.set_preference("k", "1").unwrap();
        db.set_preference("k", "2").unwrap();
        assert_eq!(db.preferences().unwrap(), [("k".to_string(), "2".to_string())]);
        db.delete_preference("k").unwrap();
        assert!(db.preferences().unwrap().is_empty());
        db.set_window_state("w", "900").unwrap();
        assert_eq!(db.window_state("w").as_deref(), Some("900"));
    }
}
