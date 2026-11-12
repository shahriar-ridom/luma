//! Freedesktop trash: listing, restore, delete and empty (the Trash view),
//! and lookup by original path (Undo).
//!
//! GIO can trash but cannot say where a file went, so for undo we read the
//! spec'd layout directly: `<trash>/files/NAME` + `<trash>/info/NAME.trashinfo`.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Trashed {
    pub file: PathBuf,
    pub info: PathBuf,
    pub deleted: String,
}

fn parse_info(text: &str) -> Option<(String, String)> {
    let mut path = None;
    let mut date = String::new();
    for l in text.lines() {
        if let Some(p) = l.strip_prefix("Path=") {
            path = Some(p.to_string());
        } else if let Some(d) = l.strip_prefix("DeletionDate=") {
            date = d.to_string();
        }
    }
    Some((gtk::glib::Uri::unescape_string(&path?, None)?.to_string(), date))
}

/// Latest trashed item that originally lived at `orig`. `topdir` is set for
/// per-volume trash dirs, whose `Path=` entries are relative to it.
pub fn find(trash_dir: &Path, topdir: Option<&Path>, orig: &Path) -> Option<Trashed> {
    let mut best: Option<Trashed> = None;
    for e in fs::read_dir(trash_dir.join("info")).ok()?.flatten() {
        let info = e.path();
        if info.extension().and_then(|x| x.to_str()) != Some("trashinfo") {
            continue;
        }
        let Some((p, deleted)) = fs::read_to_string(&info).ok().and_then(|t| parse_info(&t)) else { continue };
        let full = match topdir {
            Some(t) if !p.starts_with('/') => t.join(&p),
            _ => PathBuf::from(&p),
        };
        if full != orig {
            continue;
        }
        let name = info.file_stem()?.to_owned();
        let file = trash_dir.join("files").join(&name);
        if fs::symlink_metadata(&file).is_err() {
            continue;
        }
        if best.as_ref().is_none_or(|b| deleted > b.deleted) {
            best = Some(Trashed { file, info, deleted });
        }
    }
    best
}

/// Move the trashed item back; refuses to overwrite.
pub fn restore(t: &Trashed, orig: &Path) -> io::Result<()> {
    if fs::symlink_metadata(orig).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", orig.display())));
    }
    fs::rename(&t.file, orig)?;
    let _ = fs::remove_file(&t.info);
    Ok(())
}

/// Highest ancestor on the same device as `p` (the volume's top directory).
fn topdir_of(p: &Path) -> Option<PathBuf> {
    let dev = fs::symlink_metadata(p.parent()?).ok()?.dev();
    let mut cur = p.parent()?;
    while let Some(up) = cur.parent() {
        match fs::symlink_metadata(up) {
            Ok(m) if m.dev() == dev => cur = up,
            _ => break,
        }
    }
    Some(cur.to_path_buf())
}

/// `p` with symlinks in its parent folders resolved. The item itself is gone
/// (it is in the trash), so only the parent can be resolved.
fn resolved(p: &Path) -> PathBuf {
    match (p.parent().and_then(|d| fs::canonicalize(d).ok()), p.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => p.to_path_buf(),
    }
}

/// Look for `orig` in the home trash, then in the volume's own trash.
/// `orig` may have been reached through a symlink (`~/storage -> /mnt/storage`):
/// the home trash records the path as given, a volume trash records it
/// relative to the volume's real top directory.
pub fn find_anywhere(orig: &Path) -> Option<Trashed> {
    let real = resolved(orig);
    let home = gtk::glib::user_data_dir().join("Trash");
    if let Some(t) = find(&home, None, orig).or_else(|| find(&home, None, &real)) {
        return Some(t);
    }
    let top = topdir_of(&real)?;
    let uid = nix::unistd::Uid::current().as_raw();
    [top.join(".Trash").join(uid.to_string()), top.join(format!(".Trash-{uid}"))]
        .iter()
        .find_map(|d| find(d, Some(&top), &real))
}

/// The path the Trash view lives at. Not a real folder: panes treat it as
/// "show every trash".
pub const TRASH_URI: &str = "trash:///";

pub fn is_trash(p: &Path) -> bool {
    p.as_os_str() == TRASH_URI
}

/// One trash directory and the volume top its `Path=` entries are relative to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashDir {
    pub dir: PathBuf,
    pub topdir: Option<PathBuf>,
}

/// The home trash plus each mounted volume's own trash that exists
/// (`$top/.Trash/$uid` or `$top/.Trash-$uid`), as the spec lays them out.
pub fn trash_dirs() -> Vec<TrashDir> {
    let mut out = vec![TrashDir { dir: gtk::glib::user_data_dir().join("Trash"), topdir: None }];
    let uid = nix::unistd::Uid::current().as_raw();
    let mut seen = std::collections::HashSet::new();
    for m in crate::services::storage::proc_mounts() {
        let top = PathBuf::from(&m.target);
        if !seen.insert(top.clone()) {
            continue;
        }
        for dir in [top.join(".Trash").join(uid.to_string()), top.join(format!(".Trash-{uid}"))] {
            if dir.join("files").is_dir() && !out.iter().any(|t| t.dir == dir) {
                out.push(TrashDir { dir, topdir: Some(top.clone()) });
            }
        }
    }
    out
}

/// A trashed item, for the Trash view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Where it sits now: `<trash>/files/NAME`.
    pub file: PathBuf,
    /// Its `.trashinfo` (may be missing for stray files).
    pub info: PathBuf,
    /// Where it came from (empty if unknown).
    pub orig: PathBuf,
    /// `YYYY-MM-DDThh:mm:ss`, local time, as written by the spec.
    pub deleted: String,
}

impl Item {
    /// Name as it was before deletion.
    pub fn name(&self) -> String {
        self.orig.file_name().or(self.file.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Everything in `t`, newest first.
pub fn list(t: &TrashDir) -> Vec<Item> {
    let Ok(rd) = fs::read_dir(t.dir.join("files")) else { return vec![] };
    let mut items: Vec<Item> = rd
        .flatten()
        .map(|e| {
            let file = e.path();
            let mut info = t.dir.join("info").join(e.file_name());
            info.as_mut_os_string().push(".trashinfo");
            let (orig, deleted) = fs::read_to_string(&info)
                .ok()
                .and_then(|text| parse_info(&text))
                .map(|(p, d)| {
                    let full = match &t.topdir {
                        Some(top) if !p.starts_with('/') => top.join(&p),
                        _ => PathBuf::from(&p),
                    };
                    (full, d)
                })
                .unwrap_or_default();
            Item { file, info, orig, deleted }
        })
        .collect();
    items.sort_by(|a, b| b.deleted.cmp(&a.deleted));
    items
}

/// Everything in every trash, newest first.
pub fn list_all() -> Vec<Item> {
    let mut all: Vec<Item> = trash_dirs().iter().flat_map(list).collect();
    all.sort_by(|a, b| b.deleted.cmp(&a.deleted));
    all
}

/// Put an item back where it came from (refuses to overwrite).
pub fn restore_item(it: &Item) -> io::Result<()> {
    if it.orig.as_os_str().is_empty() {
        return Err(io::Error::other("its original location is unknown"));
    }
    if let Some(parent) = it.orig.parent() {
        fs::create_dir_all(parent)?;
    }
    restore(&Trashed { file: it.file.clone(), info: it.info.clone(), deleted: it.deleted.clone() }, &it.orig)
}

/// Delete an item for good, with its info file.
pub fn delete_item(it: &Item) -> io::Result<()> {
    crate::core::filesystem::remove_tree(&it.file)?;
    let _ = fs::remove_file(&it.info);
    Ok(())
}

/// Delete everything in every trash. Stops early if `cancel` is set.
/// Returns how many items were removed and the first error, if any.
pub fn empty_all(cancel: &std::sync::atomic::AtomicBool) -> (usize, Option<io::Error>) {
    let (mut n, mut first) = (0, None);
    for it in list_all() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match delete_item(&it) {
            Ok(()) => n += 1,
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    // Spec cache of directory sizes; stale once emptied.
    for t in trash_dirs() {
        let _ = fs::remove_file(t.dir.join("directorysizes"));
    }
    (n, first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_latest_and_restore_without_overwrite() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/trash");
        let _ = fs::remove_dir_all(&root);
        let trash = root.join("Trash");
        fs::create_dir_all(trash.join("files")).unwrap();
        fs::create_dir_all(trash.join("info")).unwrap();
        let orig = root.join("docs/my file.txt");
        fs::create_dir_all(orig.parent().unwrap()).unwrap();
        let enc = format!("{}/docs/my%20file.txt", root.display());
        for (name, date, body) in
            [("my file.txt", "2024-01-01T10:00:00", "old"), ("my file.txt.2", "2024-06-01T10:00:00", "new")]
        {
            fs::write(trash.join("files").join(name), body).unwrap();
            fs::write(
                trash.join("info").join(format!("{name}.trashinfo")),
                format!("[Trash Info]\nPath={enc}\nDeletionDate={date}\n"),
            )
            .unwrap();
        }
        let t = find(&trash, None, &orig).unwrap();
        assert_eq!(fs::read_to_string(&t.file).unwrap(), "new");
        fs::write(&orig, "occupied").unwrap();
        assert!(restore(&t, &orig).is_err());
        assert_eq!(fs::read_to_string(&orig).unwrap(), "occupied");
        fs::remove_file(&orig).unwrap();
        restore(&t, &orig).unwrap();
        assert_eq!(fs::read_to_string(&orig).unwrap(), "new");
        assert!(!t.info.exists());
        assert!(find(&trash, None, &root.join("nope")).is_none());
    }

    #[test]
    fn list_restore_and_delete_items() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/trash-list");
        let _ = fs::remove_dir_all(&root);
        let dir = root.join(".Trash-1000");
        fs::create_dir_all(dir.join("files/folder")).unwrap();
        fs::create_dir_all(dir.join("info")).unwrap();
        fs::write(dir.join("files/a.txt"), "a").unwrap();
        fs::write(dir.join("files/folder/inner"), "x").unwrap();
        fs::write(dir.join("files/stray"), "s").unwrap();
        // Per-volume trash: paths are relative to the volume top.
        fs::write(
            dir.join("info/a.txt.trashinfo"),
            "[Trash Info]\nPath=docs/a.txt\nDeletionDate=2024-02-01T09:00:00\n",
        )
        .unwrap();
        fs::write(dir.join("info/folder.trashinfo"), "[Trash Info]\nPath=folder\nDeletionDate=2024-03-01T09:00:00\n")
            .unwrap();
        let t = TrashDir { dir: dir.clone(), topdir: Some(root.clone()) };

        let items = list(&t);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].orig, root.join("folder"), "newest first");
        let a = items.iter().find(|i| i.name() == "a.txt").unwrap();
        assert_eq!(a.orig, root.join("docs/a.txt"));
        let stray = items.iter().find(|i| i.file.ends_with("stray")).unwrap();
        assert!(stray.orig.as_os_str().is_empty());
        assert!(restore_item(stray).is_err(), "unknown origin can't be restored");

        restore_item(a).unwrap();
        assert_eq!(fs::read_to_string(root.join("docs/a.txt")).unwrap(), "a");
        assert!(!a.info.exists());

        let folder = items.iter().find(|i| i.name() == "folder").unwrap();
        delete_item(folder).unwrap();
        assert!(!folder.file.exists() && !folder.info.exists());
        assert_eq!(list(&t).len(), 1);
        assert!(is_trash(Path::new(TRASH_URI)) && !is_trash(Path::new("/tmp")));
    }

    #[test]
    fn symlinked_parents_are_resolved_and_read_only_folders_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/trash-links");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real/sub")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let real = fs::canonicalize(root.join("real")).unwrap();
        assert_eq!(resolved(&root.join("link/sub/gone.txt")), real.join("sub/gone.txt"));
        assert_eq!(resolved(Path::new("/no/such/dir/x")), PathBuf::from("/no/such/dir/x"));

        let locked = root.join("locked");
        fs::create_dir_all(locked.join("inner")).unwrap();
        fs::write(locked.join("inner/f"), "x").unwrap();
        fs::set_permissions(locked.join("inner"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        crate::core::filesystem::remove_tree(&locked).unwrap();
        assert!(!locked.exists());
    }
}
