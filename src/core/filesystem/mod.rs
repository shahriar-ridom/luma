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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

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
