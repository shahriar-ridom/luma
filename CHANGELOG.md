# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

- **Fixed:** previewing an MP3 could crash Luma (in Quick Look, the preview panel and the gallery). GTK's media backend plays through GStreamer's `playbin3`, whose decodebin3 aborts on MP3 streams when they are seeked or restarted (GStreamer 1.28). Audio now plays on Luma's own `playbin` pipeline, which never reaches that code.
- Quick Look redesign: bezel-less, the window takes the shape of its content.
  - Images fill the window over a blurred copy of themselves; videos play edge to edge, sized to their aspect ratio (F or a double-click for full screen).
  - Audio opens a compact player with the cover art, title, artist, a seek bar and ±10 s skips. The same player is used in the preview panel and the gallery.
  - Documents read in a centred column; everything else shows a card with its icon or thumbnail, kind, size and date.
  - Archives (ZIP, TAR and its compressed forms, 7z, RAR, ISO, …) show their contents as a tree with file and folder sizes and totals, read with bsdtar without extracting anything.
  - A floating bar with the name, Open and Close fades out over images and video while the pointer rests.
- Keyboard focus rings are gone from the whole interface.
- Frost design: a redesigned dark mode with the same glass look, and the accent colour now follows the system accent (it was fixed to periwinkle).
- New dependency: GStreamer (`gstreamer`, `gst-plugins-base`) is linked directly for audio playback.

## [0.5.0] - 2026-11-21

- Disk Manager (menu → Disk Manager…, the Storage dialog, or right-click a drive in the sidebar):
  - **Cleanup:** finds the Trash, application caches, thumbnails, old logs, the package cache, the system journal and crash dumps, plus large files and duplicates on request. Shows exactly what will happen before anything is removed, and how much space was really freed.
  - **Health:** reads each drive's SMART data (SATA and NVMe), starts short self-tests, and runs read-only filesystem checks (ext4, vfat, XFS) or a btrfs scrub.
  - **Schedule:** optional unattended cleanup on a systemd user timer, running `luma --cleanup-run` (also usable by hand, with `--dry-run`). It only cleans the user-level things you tick.
  - Administrator tasks use `pkexec`, only from a fixed list, and always show the exact command and ask first. Nothing is repaired.
  - New optional packages: `smartmontools`, `polkit`, `btrfs-progs`, `xfsprogs`, `pacman-contrib`.
- New app icon: frosted-glass folder with papers and the L mark.
- Icon packs: Luma now uses the system's icon pack (including `~/.local/share/icons`) for UI glyphs, folders and file types, and falls back to its own icons where the pack has none. Pick a pack, or the built-in Luma icons, under Appearance → Icon Pack.
- Higher contrast: stronger borders, selection and dimmed text across the UI. No blur or transparency was added.
- Light/dark and accent colour keep following the system setting.

## [0.4.1] - 2026-11-20

This pull request introduces several improvements and fixes across the codebase, focusing on security, stability, and user experience. The most notable changes include preventing passwords from being stored in saved network connections, improving file comparison reliability, enhancing duplicate file search with new traversal controls, and clarifying documentation and UI behaviors.

**Security and privacy improvements:**

* Saved network connection passwords are now scrubbed from both the database and sidebar labels at startup, ensuring that sensitive information is not retained. This includes a migration for existing saved connections and an idempotent test to verify the behavior. (`src/application/state.rs`, [[1]](diffhunk://#diff-89dae648b606502d0486cede5b191006a642ac6415e9ea44ee0e17961494f442R28) [[2]](diffhunk://#diff-89dae648b606502d0486cede5b191006a642ac6415e9ea44ee0e17961494f442R43-R64) [[3]](diffhunk://#diff-89dae648b606502d0486cede5b191006a642ac6415e9ea44ee0e17961494f442R93-R116)

**Stability and correctness:**

* The file comparison function now refuses to compare special files (such as FIFOs), preventing hangs or unexpected behavior. A test is added to ensure that special files are rejected immediately. (`src/core/compare/mod.rs`, [[1]](diffhunk://#diff-efd083547959e1cb88c9220aa872a9ebcf4377700f77949d6930c5cb074f07baR76-R80) [[2]](diffhunk://#diff-efd083547959e1cb88c9220aa872a9ebcf4377700f77949d6930c5cb074f07baR140-R155)

**Duplicate file search enhancements:**

* Introduced the `WalkLimits` struct to control recursive directory traversal, allowing certain trees to be skipped and enforcing same-filesystem restrictions. The duplicate search logic and tests have been updated to use these new controls, ensuring more predictable and efficient scans. (`src/core/duplicates/mod.rs`, [[1]](diffhunk://#diff-60a77ca89f3ad156bd304fcb761b1ca9dc084f5ccbb2020a142546796defc3f5R12) [[2]](diffhunk://#diff-60a77ca89f3ad156bd304fcb761b1ca9dc084f5ccbb2020a142546796defc3f5L30-R39) [[3]](diffhunk://#diff-60a77ca89f3ad156bd304fcb761b1ca9dc084f5ccbb2020a142546796defc3f5R55-R57) [[4]](diffhunk://#diff-60a77ca89f3ad156bd304fcb761b1ca9dc084f5ccbb2020a142546796defc3f5R118-R126) [[5]](diffhunk://#diff-60a77ca89f3ad156bd304fcb761b1ca9dc084f5ccbb2020a142546796defc3f5L147-R159) [[6]](diffhunk://#diff-60a77ca89f3ad156bd304fcb761b1ca9dc084f5ccbb2020a142546796defc3f5R171-R191); `src/core/filesystem/mod.rs`, [[7]](diffhunk://#diff-72dc378c1b47e84fcffc04c3d60b4a1ebae36d36b5026622074527fc26f0cb04R27-R60)

**Application lifecycle and actions:**

* Refactored application state management to use `OnceCell` and `Rc` for safer, more robust initialization and sharing of state between handlers. Added a new "Close window" action with a keyboard shortcut, and improved the quit action to close all windows gracefully. (`src/application/app.rs`, [[1]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02R3-R14) [[2]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02L18-R34) [[3]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02R47-R65) [[4]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02R75) [[5]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02L56) [[6]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02L69-R99) [[7]](diffhunk://#diff-afc8270887cdf193d3d5221ab0f91f759d2aaf6343be3a35df9ec11e0bc68b02R115); `src/application/actions.rs`, [[8]](diffhunk://#diff-6404faef0d7fbd78ca6f05daf6198ccadac8068b9d9a5e77b84a8514b393be0cR25)

**Documentation and UI improvements:**

* Updated documentation to clarify command-line placeholder handling for filenames beginning with `-`, and to note that new security advisories now fail the build. (`docs/plugins.md`, [[1]](diffhunk://#diff-416ed26bd34c304ad067cce7817d8cd1eb513c3b096a3761217b2d0297c6ffb1R15-R16); `docs/development.md`, [[2]](diffhunk://#diff-97db29a7915320e63d41d38a0440360a87055ee8ed03757aa263116dbbb4aabeL88-R88)
* Added a visual fade effect for truncated breadcrumbs in the UI for a smoother appearance. (`data/style.css`, [data/style.cssR135-R136](diffhunk://#diff-64b04ed67ae0ddec074a7f09c5ea46fca5ab8ae4f8e1343126f06ebcffd92a80R135-R136))
* The CI workflow now treats dependency advisories as build failures instead of warnings. (`.github/workflows/ci.yml`, [.github/workflows/ci.ymlL101](diffhunk://#diff-b803fcb7f17ed9235f1e5cb1fcd2f5d3b2838429d4368ae4c57ce4436577f03fL101))

## [0.4.0] - 2026-11-12

- TODO: describe this release.

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
