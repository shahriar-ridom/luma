# Bugs

Status: all fixed on `fix/bugs`.

## Reported

### 1. Operation progress is hard to notice
**Problem:** While an operation runs, the only sign is a static icon in the header bar.
**Solution (current):** The activity island (`src/ui/island.rs`) replaces both the Operations button and the toasts. It is always visible. When idle it shows the folder and the drive's free space. When work starts, it switches to a pill over the centre of the header with a live progress ring; clicking it opens a card with per-operation progress, speed, time left, pause/cancel and Retry for failures. It also shows short messages. `src/ui/island_layout.rs` makes the header controls shrink, close up and, if needed, hide the filter field so the island never covers them.
**Earlier fix (replaced):** a spinning, glowing Operations button.

### 2. Opening a file always asks which program to use
**Problem:** `gtk::FileLauncher` hands off to the OpenURI portal, which can ask every time.
**Solution:** `Pane::launch` reads the file's content type off the main thread and launches `gio::AppInfo::default_for_type` directly. If the type has no default, it shows a new chooser (`src/ui/dialogs/open_with.rs`) with "Always use for …" already ticked, so you only have to choose once. **Open With** shows the same chooser with the box unticked, and ticking it changes the default (`set_as_default_for_type`). If the default app fails to launch, you get a toast and the chooser opens.

### 3. Clicking blank space doesn't clear the selection
**Problem:** `ColumnView`/`GridView` keep the selection when you click empty space.
**Solution:** A capture-phase click gesture on the view stack picks the widget under the pointer. If that widget is the bare `listview`/`gridview` and not an item, the selection is cleared. It works for both left and right clicks, so the background menu then acts on the folder. Ctrl/Shift clicks keep the selection so rubberband can extend it. The gallery and column views are left alone.

### 4. Quick Look doesn't close when clicking outside it
**Problem:** Quick Look only closed with Space, Esc or its close button.
**First attempt (reverted):** hiding Quick Look when its window lost focus. With focus-follows-mouse window managers, just moving the pointer off the window closed it.
**Solution:** A capture-phase click gesture on the main window hides Quick Look when you press any mouse button there. Hovering does nothing. The click still reaches the main window, so it can select an item at the same time. A short "just dismissed" window stops that same click, e.g. on the toolbar Quick Look button, from opening it again straight away. Clicks in other apps don't close it; use Esc or Space.

## Found while reviewing

### 5. Descending sort puts folders after files
**Problem:** The "folders first" rule lived inside each column's sorter. `ColumnView` inverts the column sorter for descending order, so folders ended up last.
**Solution:** "Folders first" is now its own sorter, placed ahead of the column sorter in a `gtk::MultiSorter` (`src/ui/file_list.rs`), so the sort direction never flips it.

### 6. Name sort isn't natural ("file10" before "file2")
**Problem:** Names were compared as plain lowercased strings.
**Solution:** New `core::filesystem::natural_cmp`: it ignores case, compares runs of digits by their value, and uses the exact string as a stable tie-break. Other columns now fall back to name order when values tie (same size, same kind). Unit test: `natural_order`.

### 7. "Up" plays the forward slide animation
**Problem:** `go_up` set the back animation, then called `navigate()`, which overwrote it with forward.
**Solution:** New `navigate_with(path, anim)`. `go_up` passes `NavAnim::Back`.

### 8. Location bar rejects `~`, `file://` and relative paths, and Escape leaves typed text behind
**Problem:** Typed text was used as a `PathBuf` as-is. `~/Documents` showed an error. After Escape, the half-typed text came back the next time the bar opened, and focus stayed on the hidden entry.
**Solution:** `resolve_location` expands `~` and `~/…`, accepts `file://` URIs and resolves relative paths against the current folder. Escape now restores the current path and moves focus back to the file view. Unit test: `location_bar_paths`.

### 9. Invalid names in New Folder / New File / Rename are silently ignored
**Problem:** `prompt_name` returned `None` for names like `a/b`, `.` or `..`, so the dialog closed and nothing happened, with no explanation.
**Solution:** `invalid_name_reason` explains what's wrong (empty, reserved, `/`, NUL, over 255 bytes). The prompt reopens with the reason shown and the entry marked as an error, keeping what you typed. Unit test: `name_validation`.

### 10. Operation status starts with a stray " · "
**Problem:** When no byte or item totals were known yet, the status line read " · filename".
**Solution:** The separator is only added when there is something before it.

### 11. No way to add favorite folders
**Problem:** The sidebar's Favorites were fixed (Home + XDG folders). The only way to save a folder was Ctrl+D, which bookmarked the folder you were currently in, under a separate "Bookmarks" section. You couldn't add a folder from its right-click menu.
**Solution:** User favorites (still stored in the `bookmarks` table, so existing bookmarks carry over) now appear in the **Favorites** section after the built-in folders. **Add to Favorites** / **Remove from Favorites** appear in a folder's right-click menu (applies to all selected folders) and in the background menu (applies to the current folder). Ctrl+D adds the selected folders, or the current folder if none are selected. Built-in favorites can't be added twice. Favorites can still be renamed, removed and drag-reordered from the sidebar.
