//! Icon packs. By default Luma uses the icons of whatever pack the system has
//! selected (`~/.local/share/icons`, `/usr/share/icons`, ...) and falls back
//! to its own family for any glyph the pack lacks. The user can also force the
//! built-in family or pick any installed pack.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Pref value for the built-in family.
pub const BUILTIN: &str = "luma";

static BUILTIN_ONLY: AtomicBool = AtomicBool::new(false);

/// Freedesktop names to try, in order, before our own glyph.
const ALIASES: &[(&str, &[&str])] = &[
    ("luma-activity-symbolic", &["utilities-system-monitor-symbolic"]),
    ("luma-back-symbolic", &["go-previous-symbolic"]),
    ("luma-bolt-symbolic", &["system-run-symbolic"]),
    ("luma-check-symbolic", &["object-select-symbolic"]),
    ("luma-close-symbolic", &["window-close-symbolic"]),
    ("luma-edit-symbolic", &["document-edit-symbolic"]),
    ("luma-eject-symbolic", &["media-eject-symbolic"]),
    ("luma-eye-symbolic", &["view-reveal-symbolic", "view-visible-symbolic"]),
    ("luma-forward-symbolic", &["go-next-symbolic"]),
    ("luma-history-symbolic", &["document-open-recent-symbolic"]),
    ("luma-lock-symbolic", &["changes-prevent-symbolic", "system-lock-screen-symbolic"]),
    ("luma-more-symbolic", &["view-more-symbolic", "open-menu-symbolic"]),
    ("luma-options-symbolic", &["emblem-system-symbolic", "preferences-system-symbolic"]),
    ("luma-pause-symbolic", &["media-playback-pause-symbolic"]),
    ("luma-plus-symbolic", &["list-add-symbolic"]),
    ("luma-search-symbolic", &["edit-find-symbolic"]),
    ("luma-sidebar-symbolic", &["sidebar-show-symbolic"]),
    ("luma-sort-asc-symbolic", &["view-sort-ascending-symbolic"]),
    ("luma-sort-desc-symbolic", &["view-sort-descending-symbolic"]),
    ("luma-trash-symbolic", &["user-trash-symbolic"]),
    ("luma-undo-symbolic", &["edit-undo-symbolic"]),
    ("luma-up-symbolic", &["go-up-symbolic"]),
    ("luma-view-columns-symbolic", &["view-column-symbolic"]),
    ("luma-view-grid-symbolic", &["view-grid-symbolic"]),
    ("luma-view-list-symbolic", &["view-list-symbolic"]),
    ("luma-bookmark-symbolic", &["user-bookmarks-symbolic"]),
    ("luma-cloud-symbolic", &["folder-remote-symbolic"]),
    ("luma-desktop-symbolic", &["user-desktop-symbolic"]),
    ("luma-documents-symbolic", &["folder-documents-symbolic"]),
    ("luma-downloads-symbolic", &["folder-download-symbolic"]),
    ("luma-drive-symbolic", &["drive-harddisk-symbolic"]),
    ("luma-folder-symbolic", &["folder-symbolic"]),
    ("luma-home-symbolic", &["user-home-symbolic"]),
    ("luma-music-symbolic", &["folder-music-symbolic"]),
    ("luma-network-symbolic", &["network-workgroup-symbolic"]),
    ("luma-pictures-symbolic", &["folder-pictures-symbolic"]),
    ("luma-removable-symbolic", &["drive-removable-media-symbolic"]),
    ("luma-server-symbolic", &["network-server-symbolic"]),
    ("luma-tag-symbolic", &["tag-symbolic"]),
    ("luma-videos-symbolic", &["folder-videos-symbolic"]),
];

/// First candidate the pack has, else our own glyph.
fn pick(luma: &'static str, has: impl Fn(&str) -> bool, builtin_only: bool) -> &'static str {
    if builtin_only {
        return luma;
    }
    ALIASES.iter().find(|(n, _)| *n == luma).and_then(|(_, alts)| alts.iter().copied().find(|a| has(a))).unwrap_or(luma)
}

/// Icon name to use for one of our glyphs under the active pack. Main thread only.
pub fn themed(luma: &'static str) -> &'static str {
    if BUILTIN_ONLY.load(Ordering::Relaxed) {
        return luma;
    }
    let Some(d) = gtk::gdk::Display::default() else { return luma };
    let theme = gtk::IconTheme::for_display(&d);
    pick(luma, |n| theme.has_icon(n), false)
}

/// Whether file and folder icons come from our own family.
pub fn builtin_only() -> bool {
    BUILTIN_ONLY.load(Ordering::Relaxed)
}

/// Apply the saved choice: "" = system pack, "luma" = built-in, else a pack name.
pub fn apply(choice: &str) {
    BUILTIN_ONLY.store(choice == BUILTIN, Ordering::Relaxed);
    let Some(s) = gtk::Settings::default() else { return };
    match choice {
        "" | BUILTIN => s.reset_property("gtk-icon-theme-name"),
        name => s.set_gtk_icon_theme_name(Some(name)),
    }
}

/// Names of icon packs found under the XDG icon directories, sorted.
pub fn installed_packs() -> Vec<String> {
    let mut dirs: Vec<PathBuf> = vec![gtk::glib::home_dir().join(".icons"), gtk::glib::user_data_dir().join("icons")];
    dirs.extend(gtk::glib::system_data_dirs().into_iter().map(|d| d.join("icons")));
    let mut out: Vec<String> = dirs.iter().flat_map(|d| packs_in(d)).collect();
    out.sort_by_key(|n| n.to_lowercase());
    out.dedup();
    out
}

/// Packs in one directory: a folder with an `index.theme` that lists icon
/// directories (cursor themes and the `hicolor` fallback are not packs).
fn packs_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    rd.filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let index = fs::read_to_string(e.path().join("index.theme")).ok()?;
            let usable = index.lines().any(|l| l.starts_with("Directories="))
                && !index.lines().any(|l| l.trim() == "Hidden=true");
            (usable && name != "hicolor" && name != "default").then_some(name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_pack_then_falls_back() {
        assert_eq!(pick("luma-back-symbolic", |n| n == "go-previous-symbolic", false), "go-previous-symbolic");
        assert_eq!(pick("luma-back-symbolic", |_| false, false), "luma-back-symbolic");
        assert_eq!(pick("luma-eye-symbolic", |n| n == "view-visible-symbolic", false), "view-visible-symbolic");
    }

    #[test]
    fn builtin_only_ignores_the_pack() {
        assert_eq!(pick("luma-back-symbolic", |_| true, true), "luma-back-symbolic");
    }

    #[test]
    fn every_alias_names_a_shipped_glyph() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/icons/hicolor/scalable");
        for (n, _) in ALIASES {
            let found = ["actions", "places"].iter().any(|d| root.join(d).join(format!("{n}.svg")).is_file());
            assert!(found, "{n}");
        }
    }
}
