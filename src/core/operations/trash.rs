//! Freedesktop trash lookup and restore (used by Undo).
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

/// Look for `orig` in the home trash, then in the volume's own trash.
pub fn find_anywhere(orig: &Path) -> Option<Trashed> {
    let home = gtk::glib::user_data_dir().join("Trash");
    if let Some(t) = find(&home, None, orig) {
        return Some(t);
    }
    let top = topdir_of(orig)?;
    let uid = nix::unistd::Uid::current().as_raw();
    [top.join(".Trash").join(uid.to_string()), top.join(format!(".Trash-{uid}"))]
        .iter()
        .find_map(|d| find(d, Some(&top), orig))
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
}
