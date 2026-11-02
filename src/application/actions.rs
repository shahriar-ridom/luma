//! Registry of user-facing window actions and their default shortcuts.
//! Every id is also the name of a `win.<id>` action.

pub struct ActionDef {
    pub id: &'static str,
    pub label: &'static str,
    /// GTK accelerator string; empty means unbound by default.
    pub default: &'static str,
    /// Whether the shortcut editor lists it.
    pub configurable: bool,
}

const fn a(id: &'static str, label: &'static str, default: &'static str) -> ActionDef {
    ActionDef { id, label, default, configurable: true }
}

pub const ACTIONS: &[ActionDef] = &[
    a("undo", "Undo", "<Control>z"),
    a("tags", "Edit tags", "<Alt>t"),
    a("compare", "Compare two files", ""),
    a("find-duplicates", "Find duplicate files", ""),
    a("new-window", "New window", "<Control>n"),
    a("new-tab", "New tab", "<Control>t"),
    a("close-tab", "Close tab", "<Control>w"),
    a("reopen-tab", "Reopen closed tab", "<Control><Shift>t"),
    a("duplicate-tab", "Duplicate tab", ""),
    a("next-tab", "Next tab", "<Control>Page_Down"),
    a("prev-tab", "Previous tab", "<Control>Page_Up"),
    a("split-view", "Toggle split view", "F3"),
    a("focus-location", "Focus location entry", "<Control>l"),
    a("find", "Filter / search", "<Control>f"),
    a("toggle-hidden", "Toggle hidden files", "<Control>h"),
    a("select-all", "Select all", "<Control>a"),
    a("copy", "Copy", "<Control>c"),
    a("cut", "Cut", "<Control>x"),
    a("paste", "Paste", "<Control>v"),
    a("new-folder", "New folder", "<Control><Shift>n"),
    a("new-file", "New file", ""),
    a("rename", "Rename", "F2"),
    a("trash", "Move to trash", "Delete"),
    a("delete", "Delete permanently", "<Shift>Delete"),
    a("back", "Back", "<Alt>Left"),
    a("forward", "Forward", "<Alt>Right"),
    a("parent", "Parent directory", "<Alt>Up"),
    a("refresh", "Refresh", "F5"),
    a("view-grid", "Grid view", "<Control>1"),
    a("view-list", "List view", "<Control>2"),
    a("view-column", "Column view", "<Control>3"),
    a("view-gallery", "Gallery view", "<Control>4"),
    a("quick-preview", "Quick preview panel", "space"),
    a("toggle-sidebar", "Toggle sidebar", "F9"),
    a("connect-server", "Connect to server", "<Control>k"),
    a("storage", "Storage and disk usage", ""),
    a("bookmark-add", "Add folder to Favorites", "<Control>d"),
    a("favorite-remove", "Remove folder from Favorites", ""),
    a("properties", "Properties", "<Alt>Return"),
    a("search", "Search files", "<Control><Shift>f"),
    a("compress", "Compress to archive", ""),
    a("extract-here", "Extract archive here", ""),
    a("extract-to", "Extract archive to…", ""),
    a("browse-archive", "Browse archive contents", ""),
    a("open-terminal", "Open terminal here", "F4"),
    a("copy-path", "Copy path", "<Control><Shift>c"),
];

/// Actions that exist but are not rebindable (menus, internal).
pub const EXTRA_ACTIONS: &[&str] = &[
    "open",
    "empty-trash",
    "open-with",
    "extensions",
    "git-stage",
    "git-unstage",
    "git-discard",
    "git-commit",
    "git-pull",
    "git-push",
    "git-refresh",
    "tab-duplicate",
    "tab-close",
    "tab-close-others",
    "shortcuts",
    "about",
];
