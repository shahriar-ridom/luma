//! Icons are compiled into the binary and unpacked to the user's cache on
//! first run, so a bare binary or AppImage looks right without installing
//! files into `/usr/share`. Packages also install them system-wide; the
//! system copy is simply found first.

use include_dir::{Dir, DirEntry, include_dir};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

static ICONS: Dir = include_dir!("$CARGO_MANIFEST_DIR/data/icons");

const MARKER: &str = ".complete";

fn walk<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
    for e in dir.entries() {
        match e {
            DirEntry::Dir(d) => walk(d, out),
            DirEntry::File(f) => out.push(f),
        }
    }
}

/// Short content hash, so a changed icon set never reuses a stale cache.
fn fingerprint(files: &[&include_dir::File]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for f in files {
        for b in f.path().to_string_lossy().bytes().chain(f.contents().iter().copied()) {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016x}")
}

/// Write the embedded icon theme under `root` (as `<root>/hicolor/...`).
pub fn extract_icons(root: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    walk(&ICONS, &mut files);
    for f in files {
        let dest = root.join(f.path());
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(dest, f.contents())?;
    }
    fs::write(root.join(MARKER), b"")
}

/// Unpack (if needed) and return the directory to add to GTK's icon search path.
pub fn icon_search_path(cache_home: &Path) -> io::Result<PathBuf> {
    let mut files = Vec::new();
    walk(&ICONS, &mut files);
    let root = cache_home.join("luma").join(format!("icons-{}", fingerprint(&files)));
    if !root.join(MARKER).exists() {
        extract_icons(&root)?;
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_whole_family_once() {
        let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/icons-cache");
        let _ = fs::remove_dir_all(&cache);
        let root = icon_search_path(&cache).unwrap();
        for f in [
            "hicolor/scalable/places/luma-folder.svg",
            "hicolor/scalable/mimetypes/luma-file-image.svg",
            "hicolor/scalable/actions/luma-back-symbolic.svg",
            "hicolor/scalable/apps/dev.ridom.Luma.svg",
        ] {
            assert!(root.join(f).is_file(), "{f}");
        }
        // Second call reuses the same directory.
        assert_eq!(icon_search_path(&cache).unwrap(), root);
    }
}
