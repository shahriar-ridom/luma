# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.3.0] - 2026-11-02

### Added
- **Activity island**: a pill in the middle of the title bar, always visible.
  - Shows where you are. At the top of a drive (Home, `/`, a mount point) it shows the drive's free
    space, and its ring shows how full the drive is. Inside any other folder it shows that folder's
    size, measured in the background, and the ring shows its share of the drive.
  - Shows the selection: one file's name and size, or the count and total size for several.
  - Click it for a card with details: the path and a usage bar with **Disk Usage…**; for a selection,
    its kind, size and modified date with **Properties**. A selected folder's size is measured when
    the card opens.
  - While operations run, it shows a live progress ring. Its card lists each operation with
    progress, speed, time left, pause and cancel. When the work is done, it says what happened
    ("Copied 24 items").
  - Failures stay in the island with the reason and **Retry** until dismissed.
  - Short messages ("Path copied") appear in the island, replacing toasts.
  - Matches the app's theme (light, dark and the system accent). When it needs more room, the
    title bar buttons shrink and slide aside smoothly.
- **Trash** in Favorites: every deleted item from the Home trash and each mounted drive's trash, with
  its original location, deletion time and size. Right-click to **Restore** or **Delete
  Permanently**. **Empty Trash** from the sidebar, the Trash view or the island. Dropping files on
  Trash moves them there.
- **Favorite folders**: **Add to Favorites** / **Remove from Favorites** in a folder's right-click
  menu and in the background menu; <kbd>Ctrl</kbd>+<kbd>D</kbd> adds the selected folders or the
  current one. Favorites appear under Home and the user folders, and can be renamed, removed and
  reordered. Existing bookmarks carry over.
- **Default apps**: files open straight in the default app for their type. The first time a type
  has no default, a chooser asks once, with "Always use for this file type" already ticked. **Open
  With** uses the same chooser and can change the default.
- The location bar accepts `~`, `~/…`, `file://` addresses and paths relative to the current folder.

### Changed
- The Operations button and toasts are gone; the activity island does both jobs.
- Bookmarks are now shown as Favorites, in the Favorites section.
- In the list view, columns adapt to each pane's width: Kind, then Size, then Modified step out so
  names stay readable (split view in particular).
- Names sort naturally ("file2" before "file10"); ties in Size, Kind and Date fall back to the name.

### Fixed
- Clicking empty space in the list or grid view now clears the selection.
- Quick Look closes when you click outside it, but not when the pointer merely leaves its window
  (focus-follows-mouse).
- Folders stay listed before files when sorting in descending order.
- Going up a folder plays the "back" animation instead of the "forward" one.
- Pressing Escape in the location bar restores the current path and returns focus to the files.
- Invalid names in New Folder, New File and Rename (empty, `.`, `..`, containing `/`, too long) now
  explain the problem and keep what you typed, instead of silently doing nothing.
- Progress text no longer starts with a stray " · " before totals are known.
- In split view, file names no longer shrink to a few letters, and the status bars shorten instead
  of pushing the right-hand pane off the window.

## [0.1.0] - 2026-11-02

First release.

### Added
- Browsing: icon, list, column and gallery views; background thumbnails; sorting, filtering,
  hidden files; breadcrumb and editable location bar; back/forward history; mouse side buttons.
- Tabs with restore and reopen, split view, bookmarks, recent folders and files, tags.
- File operations on background threads: copy, move, trash, delete, rename, batch rename,
  new folder/file, drag and drop, clipboard interoperability, conflict resolution,
  pause/cancel/retry, undo.
- Quick Look floating preview with autoplay for audio and video; EXIF, audio-tag and PDF/media metadata.
- Archives (zip, tar.*, 7z, rar read): create, extract, browse, passphrase prompt; Enter extracts.
- Search by name, content, type, size and date; duplicate finder; file compare; properties,
  checksums and a permissions editor.
- Git status badges, branch summary and stage/unstage/discard/commit/pull/push.
- Google Drive through rclone, network locations through GVfs, removable drives, storage overview
  and folder usage analysis.
- Custom actions and declarative plugins; configurable keyboard shortcuts.
- Custom icon family, light/dark themes, animations and responsive layout.
- Packages: Debian, Arch, Fedora, AppImage and a portable binary tarball.
