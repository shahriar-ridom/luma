//! GIO-based filesystem access. No UI dependencies other than glib/gio.

use gtk::gio::{self, prelude::*};
use std::{cell::Cell, rc::Rc};

pub const LIST_ATTRS: &str = "standard::name,standard::display-name,standard::type,\
standard::content-type,standard::size,standard::icon,standard::is-hidden,\
standard::is-backup,standard::is-symlink,time::modified,thumbnail::path";

const BATCH: i32 = 200;

/// Cancels in-flight directory loads when a newer one starts.
#[derive(Clone, Default)]
pub struct LoadToken(Rc<Cell<u64>>);

impl LoadToken {
    pub fn next(&self) -> u64 {
        let n = self.0.get() + 1;
        self.0.set(n);
        n
    }
    pub fn is_current(&self, n: u64) -> bool {
        self.0.get() == n
    }
}

/// Incrementally enumerate `dir`, calling `on_batch` per chunk on the main
/// context. Stops silently if `token` is superseded. Returns Err on failure
/// to open the directory.
pub async fn load_dir(
    dir: &gio::File,
    token: &LoadToken,
    id: u64,
    mut on_batch: impl FnMut(Vec<gio::FileInfo>),
) -> Result<(), gtk::glib::Error> {
    let en = dir
        .enumerate_children_future(
            LIST_ATTRS,
            // Follow symlinks so a link to a folder lists (and navigates) as a
            // folder. GIO falls back to the link itself when the target is gone.
            gio::FileQueryInfoFlags::NONE,
            gtk::glib::Priority::DEFAULT,
        )
        .await?;
    loop {
        let infos = en.next_files_future(BATCH, gtk::glib::Priority::DEFAULT).await?;
        if !token.is_current(id) {
            return Ok(());
        }
        if infos.is_empty() {
            return Ok(());
        }
        on_batch(infos);
    }
}

/// Whether `path` is a folder (links followed), without blocking the caller.
/// A stale network mount stalls a worker thread, never the UI.
pub async fn is_dir_async(path: &std::path::Path) -> bool {
    gio::File::for_path(path)
        .query_info_future("standard::type", gio::FileQueryInfoFlags::NONE, gtk::glib::Priority::DEFAULT)
        .await
        .is_ok_and(|i| i.file_type() == gio::FileType::Directory)
}

/// Rename `from` to `to` without ever replacing something that is already
/// there (`ErrorKind::AlreadyExists` instead). Uses the kernel's
/// `RENAME_NOREPLACE`, so there is no gap between checking and renaming;
/// where that is unavailable it falls back to checking first.
pub fn rename_noreplace(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use nix::{
            errno::Errno,
            fcntl::{AT_FDCWD, RenameFlags, renameat2},
        };
        match renameat2(AT_FDCWD, from, AT_FDCWD, to, RenameFlags::RENAME_NOREPLACE) {
            Ok(()) => return Ok(()),
            Err(Errno::EEXIST) => return Err(std::io::ErrorKind::AlreadyExists.into()),
            // The filesystem can't do an atomic no-replace rename: check first instead.
            Err(Errno::EINVAL | Errno::ENOSYS | Errno::ENOTSUP) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// Give the owner full access to `dir` and every folder below it, so a
/// read-only folder (mode 555) cannot stop its contents being deleted.
fn make_deletable(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(md) = std::fs::symlink_metadata(dir) else { return };
    if !md.is_dir() {
        return;
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(md.permissions().mode() | 0o700));
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            make_deletable(&e.path());
        }
    }
}

/// Delete a file, link or whole folder tree (links are not followed). Folders
/// that are read-only are unlocked first, which is only done for what is
/// being deleted anyway.
pub fn remove_tree(p: &std::path::Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => match std::fs::remove_dir_all(p) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                make_deletable(p);
                std::fs::remove_dir_all(p)
            }
            r => r,
        },
        Ok(_) => std::fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Icon name from our own family, chosen by content type, so files look the
/// same whatever icon theme the system uses.
pub fn icon_name(mime: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "luma-folder";
    }
    let m = mime;
    if m.starts_with("image/") {
        "luma-file-image"
    } else if m.starts_with("audio/") {
        "luma-file-audio"
    } else if m.starts_with("video/") {
        "luma-file-video"
    } else if ["zip", "tar", "compress", "7z", "rar", "gzip", "xz", "zstd", "bzip", "cpio", "iso9660"]
        .iter()
        .any(|k| m.contains(k))
    {
        "luma-file-archive"
    } else if m == "text/markdown"
        || m == "application/pdf"
        || m == "application/rtf"
        || m == "application/epub+zip"
        || m.starts_with("application/vnd.")
        || m == "application/msword"
        || m == "text/plain"
    {
        "luma-file-document"
    } else if m.starts_with("text/")
        || m.contains("json")
        || m.contains("xml")
        || m.contains("shellscript")
        || m.contains("javascript")
        || m.contains("toml")
        || m.contains("yaml")
        || m.contains("x-sql")
        || m.contains("x-desktop")
    {
        "luma-file-code"
    } else {
        "luma-file"
    }
}

pub fn icon_for(mime: &str, is_dir: bool) -> gio::Icon {
    // Deliberately no fallback names: GTK would look them up in the user's
    // theme first and our own icons would never win.
    gio::ThemedIcon::new(icon_name(mime, is_dir)).upcast()
}

/// The entry's icon, with a link emblem for symbolic links.
pub fn file_icon(info: &gio::FileInfo) -> Option<gio::Icon> {
    let mime = info.content_type().map(|c| c.to_string()).unwrap_or_default();
    let icon = icon_for(&mime, is_dir(info));
    if info.is_symlink() {
        let emblem = gio::Emblem::new(&gio::ThemedIcon::new("emblem-symbolic-link"));
        return Some(gio::EmblemedIcon::new(&icon, Some(&emblem)).upcast());
    }
    Some(icon)
}

pub fn is_dir(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

pub fn human_size(bytes: i64) -> String {
    gtk::glib::format_size(bytes.max(0) as u64).to_string()
}

/// File-name order as people expect it: case-insensitive, and runs of digits
/// compared by value, so "file2" sorts before "file10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b), // equal ignoring case: fall back to a stable tie-break
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let run = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        s.push(c);
                    }
                    s
                };
                let (m, n) = (run(&mut x), run(&mut y));
                let (mt, nt) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let o = mt.len().cmp(&nt.len()).then_with(|| mt.cmp(nt)).then_with(|| m.len().cmp(&n.len()));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    #[test]
    fn noreplace_rename_never_overwrites_and_remove_tree_unlocks() {
        use std::os::unix::fs::PermissionsExt;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/noreplace");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a"), "A").unwrap();
        fs::write(root.join("b"), "B").unwrap();
        let err = rename_noreplace(&root.join("a"), &root.join("b")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "B");
        assert!(root.join("a").exists());
        rename_noreplace(&root.join("a"), &root.join("c")).unwrap();
        assert_eq!(fs::read_to_string(root.join("c")).unwrap(), "A");

        fs::create_dir_all(root.join("ro/inner")).unwrap();
        fs::write(root.join("ro/inner/f"), "x").unwrap();
        fs::set_permissions(root.join("ro/inner"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(root.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
        remove_tree(&root.join("ro")).unwrap();
        assert!(!root.join("ro").exists());
        remove_tree(&root.join("ro")).unwrap(); // already gone is fine
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["file10.txt", "File2.txt", "file1.txt", "file02.txt", "b", "A", "file2.txt"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["A", "b", "file1.txt", "File2.txt", "file2.txt", "file02.txt", "file10.txt"]);
    }

    #[test]
    fn icon_names_by_type() {
        assert_eq!(icon_name("inode/directory", true), "luma-folder");
        assert_eq!(icon_name("image/png", false), "luma-file-image");
        assert_eq!(icon_name("application/gzip", false), "luma-file-archive");
        assert_eq!(icon_name("application/x-compressed-tar", false), "luma-file-archive");
        assert_eq!(icon_name("application/pdf", false), "luma-file-document");
        assert_eq!(icon_name("text/x-rust", false), "luma-file-code");
        assert_eq!(icon_name("application/x-shellscript", false), "luma-file-code");
        assert_eq!(icon_name("application/octet-stream", false), "luma-file");
    }

    #[test]
    fn symlinked_folders_list_as_folders_and_broken_links_survive() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/symlinks");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("file.txt"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link-to-dir")).unwrap();
        std::os::unix::fs::symlink(root.join("file.txt"), root.join("link-to-file")).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), root.join("broken")).unwrap();

        let en = gio::File::for_path(&root)
            .enumerate_children(LIST_ATTRS, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            .unwrap();
        let infos: std::collections::HashMap<String, gio::FileInfo> =
            en.flatten().map(|i| (i.name().to_string_lossy().into_owned(), i)).collect();
        assert!(is_dir(&infos["link-to-dir"]) && infos["link-to-dir"].is_symlink());
        assert!(!is_dir(&infos["link-to-file"]));
        assert!(infos.contains_key("broken"), "dangling links must still be listed");
    }
}
