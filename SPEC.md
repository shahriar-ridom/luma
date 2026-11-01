# Linux File Manager — Product Vision & Technical Specification

## 1. Project Overview

### Vision

Build a modern, powerful, and beautifully designed Linux file manager that combines the elegant user experience of macOS Finder with the extensive functionality of KDE Dolphin.

The application should prioritize:

- Beautiful, intuitive, and minimal interface.
- Powerful filesystem management.
- Excellent performance.
- Native Linux integration.
- Keyboard-first navigation.
- Extensibility and customization.
- Reliability when handling large-scale file operations.

### Design Philosophy

Three fundamental principles define the application:

1. **Finder-inspired aesthetics:** Clean layouts, elegant typography, refined spacing, intuitive navigation, and polished animations.
2. **Dolphin-level functionality:** Advanced file operations, split views, tabs, previews, archives, permissions, search, and integrations.
3. **Linux-native engineering:** Native filesystem integration, Wayland compatibility, desktop environment integration, and support for Linux-specific capabilities.

The goal is not to clone Finder or Dolphin, but to create an independent product that combines their strongest qualities.

---

# 2. Technology Stack

The application will be written primarily in Rust.

| Component | Technology |
|---|---|
| Programming language | Rust |
| GUI framework | GTK4 |
| UI design system | Libadwaita |
| Filesystem abstraction | GIO / GFile |
| Async operations | GLib Main Context + Tokio where appropriate |
| Database | SQLite |
| Image thumbnails | GdkPixbuf / glycin |
| Video thumbnails | GStreamer |
| Archive management | libarchive |
| Filesystem monitoring | GIO / inotify |
| Configuration | GSettings |
| Serialization | Serde |
| Logging | tracing |
| Error handling | anyhow + thiserror |
| Build system | Cargo |

### Important Architectural Decision

Use GIO/GFile as the primary filesystem abstraction instead of building everything directly around `std::fs`.

This provides support for:

- Local filesystems.
- Remote filesystems.
- SMB and SFTP locations through available GIO backends.
- Mount management.
- Trash operations.
- Filesystem metadata.
- Asynchronous file operations.
- Virtual filesystem locations.

Use `std::fs` where direct local filesystem access is appropriate.

---

# 3. User Interface Design

## 3.1 Main Window

The main window should consist of five primary areas:

1. Header bar / Toolbar.
2. Navigation sidebar.
3. Main file browser.
4. Optional preview panel.
5. Status bar.

### Layout

```text
+--------------------------------------------------------------+
| Back | Forward | Location | Search | View | Sort | Settings |
+----------------+---------------------------------------------+
|                |                                             |
| Favorites      |                                             |
|                |                                             |
| Home           |              Main Content                   |
| Desktop        |                                             |
| Documents      |          Files and Folders                   |
| Downloads      |                                             |
| Pictures       |                                             |
| Videos         |                                             |
| Music          |                                             |
|                |                                             |
| Locations      |                                             |
|                |                                             |
| Devices        |                                             |
| Network        |                                             |
|                |                                             |
+----------------+---------------------------------------------+
| Selected: 3 items                    Free space: 120 GB       |
+--------------------------------------------------------------+
```

## 3.2 Design Language

The visual experience should feature:

- Finder-inspired layout.
- Minimalist header bar.
- Translucent or subtly elevated sidebar.
- Rounded containers.
- Subtle borders and separators.
- Smooth navigation transitions.
- Beautiful file and folder thumbnails.
- Adaptive grid spacing.
- Dynamic accent colors.
- Light and dark themes.
- Consistent iconography.
- Context-sensitive toolbars.
- Responsive layouts.

Avoid excessive blur, shadows, and animations. Performance and readability take priority.

## 3.3 Navigation Sidebar

Sections:

### Favorites
- Home
- Desktop
- Documents
- Downloads
- Pictures
- Videos
- Music
- Custom bookmarks

### Locations
- Internal drives.
- External drives.
- Mounted partitions.
- Network locations.

### Network
- SMB shares.
- SFTP connections.
- Other supported GIO locations.

Features:
- Drag-and-drop bookmarks.
- Reorderable sections.
- Custom bookmark names.
- Context menus.
- Mount and unmount devices.
- Expandable device entries.

## 3.4 Toolbar

The toolbar should provide:

- Back and forward navigation.
- Up one directory.
- Breadcrumb navigation.
- Editable location entry.
- Search.
- View mode selector.
- Sorting controls.
- Selection actions.
- New folder.
- More actions menu.

Toolbar actions should adapt to the current context and available space.

---

# 4. File Browsing Features

## 4.1 View Modes

### Grid View
- Large file icons.
- Adjustable icon sizes.
- File and folder labels.
- Thumbnail support.
- Configurable grid spacing.
- Multi-selection.

### List View
- Name.
- Size.
- Type.
- Modified date.
- Created date where available.
- Permissions.
- Owner.
- Group.

Features:
- Sortable columns.
- Resizable columns.
- Configurable visible columns.
- Multi-selection.
- Expandable directory sizes where practical.

### Column View

Inspired by Finder.

- Hierarchical directory navigation.
- Multiple columns.
- Horizontal scrolling.
- Selected folder contents appear in the next column.
- Preview of selected files.
- Adjustable column widths.

### Gallery View

- Large preview area.
- Thumbnail strip.
- Previous and next navigation.
- Metadata information.
- Support for images and other previewable files.

## 4.2 Navigation

- Back and forward history.
- Breadcrumb navigation.
- Direct path entry.
- Multiple tabs.
- Tab duplication.
- Tab reordering.
- Tab restoration.
- Split view.
- Independent navigation per pane.
- Keyboard shortcuts.
- Recent locations.

## 4.3 File Selection

- Single selection.
- Multiple selection.
- Range selection.
- Select all.
- Invert selection.
- Selection by pattern.
- Selection by file extension.
- Selection by file type.

---

# 5. File Operations

This is one of the application's most important subsystems.

## 5.1 Basic Operations

- Copy.
- Cut.
- Paste.
- Move.
- Rename.
- Batch rename.
- Delete.
- Permanent delete.
- Create folder.
- Create empty file.
- Create symbolic link.
- Open with another application.
- Open in terminal.
- Show file properties.

## 5.2 Advanced Operations

- Archive creation.
- Archive extraction.
- File permissions management.
- Ownership management.
- Recursive folder size calculation.
- Checksum calculation.
- File comparison.
- Duplicate detection.
- File tagging.
- File metadata inspection.

## 5.3 File Operation Manager

All potentially expensive operations must run outside the GTK main thread.

The operation manager should support:

- Background execution.
- Progress tracking.
- Multiple concurrent operations.
- Operation queues.
- Cancellation.
- Pause and resume where supported.
- Error reporting.
- Conflict resolution.
- Operation history.
- Retry failed operations.

### File Conflict Resolution

When a destination contains an existing file, offer:

- Replace.
- Skip.
- Rename.
- Replace all.
- Skip all.
- Compare files.

### Reliability Requirements

- Never silently overwrite files.
- Handle partial failures.
- Report inaccessible files.
- Preserve metadata where possible.
- Handle symbolic links correctly.
- Avoid blocking the UI.
- Support large directories.
- Use atomic operations where appropriate.

---

# 6. Advanced Features

## 6.1 Search

- Filename search.
- File extension filtering.
- MIME type filtering.
- File size filtering.
- Modified date filtering.
- Recursive directory search.
- Search within file contents.
- Search by metadata.
- Search history.

## 6.2 Preview System

A quick preview panel should support:

- Images.
- Videos.
- Audio.
- PDF documents.
- Text files.
- Markdown.
- Source code.
- Archives.

Preview generation must be asynchronous and resource-limited.

## 6.3 Archive Management

Support common archive formats:

- ZIP.
- TAR.
- TAR.GZ.
- TAR.XZ.
- TAR.ZST.
- 7Z.
- RAR where supported by available libraries.

Features:

- Extract here.
- Extract to directory.
- Browse archive contents.
- Create archives.
- Compression level selection.
- Password-protected archive support where available.

## 6.4 Storage Management

- Disk usage visualization.
- Directory size calculation.
- Filesystem information.
- Available storage.
- Mount and unmount.
- Device information.
- Filesystem type detection.

## 6.5 Permissions

Display and manage:

- Read permissions.
- Write permissions.
- Execute permissions.
- Owner.
- Group.
- Special permission bits.
- Recursive permission changes.

Respect Linux security boundaries. Do not silently escalate privileges.

## 6.6 Terminal Integration

- Open current directory in terminal.
- Configurable terminal emulator.
- Optional embedded terminal panel.
- Copy directory path.
- Run custom commands.

## 6.7 Git Integration

Optional integration:

- Git repository detection.
- Modified file indicators.
- Untracked file indicators.
- Branch information.
- Repository status.
- Context menu Git actions.

## 6.8 Network Integration

Support network locations through GIO backends:

- SFTP.
- SMB.
- FTP where supported.
- Other GVfs-supported protocols.

Features:
- Save connections.
- Reconnect.
- Browse remote directories.
- Copy between local and remote locations.
- Authentication handling.

---

# 7. Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| Ctrl + N | New window |
| Ctrl + T | New tab |
| Ctrl + W | Close tab |
| Ctrl + L | Focus location entry |
| Ctrl + F | Search |
| Ctrl + H | Toggle hidden files |
| Ctrl + A | Select all |
| Ctrl + C | Copy |
| Ctrl + X | Cut |
| Ctrl + V | Paste |
| Ctrl + Z | Undo where supported |
| Ctrl + Shift + N | New folder |
| F2 | Rename |
| Delete | Move to trash |
| Shift + Delete | Permanent delete |
| Alt + Left | Back |
| Alt + Right | Forward |
| Alt + Up | Parent directory |
| Space | Quick preview |
| F5 | Refresh |
| Ctrl + 1 | Grid view |
| Ctrl + 2 | List view |
| Ctrl + 3 | Column view |

Shortcuts should be configurable.

---

# 8. Architecture

The application should follow a modular architecture with a clear separation between UI, application logic, filesystem operations, and platform integrations.

## 8.1 Project Structure

```text
src/
├── main.rs
│
├── application/
│   ├── mod.rs
│   ├── app.rs
│   ├── actions.rs
│   └── state.rs
│
├── ui/
│   ├── mod.rs
│   ├── window.rs
│   ├── toolbar.rs
│   ├── sidebar.rs
│   ├── file_grid.rs
│   ├── file_list.rs
│   ├── column_view.rs
│   ├── gallery_view.rs
│   ├── preview.rs
│   ├── tabs.rs
│   ├── split_view.rs
│   └── dialogs/
│
├── core/
│   ├── filesystem/
│   ├── navigation/
│   ├── operations/
│   ├── search/
│   ├── metadata/
│   ├── permissions/
│   └── selection/
│
├── services/
│   ├── thumbnail.rs
│   ├── archive.rs
│   ├── mounts.rs
│   ├── indexing.rs
│   ├── preview.rs
│   └── storage.rs
│
├── integrations/
│   ├── terminal.rs
│   ├── git.rs
│   ├── network.rs
│   └── plugins.rs
│
├── settings/
│   ├── preferences.rs
│   └── persistence.rs
│
└── database/
    ├── mod.rs
    ├── schema.rs
    └── repository.rs
```

## 8.2 Architectural Principles

1. UI components must not directly execute expensive filesystem operations.
2. Filesystem operations must be independent of UI implementation.
3. Navigation state must be independent of the current view mode.
4. Every file operation should expose a consistent progress and error interface.
5. UI updates must happen on the GTK main thread.
6. Long-running tasks must be asynchronous.
7. Filesystem changes should be monitored and reflected in the interface.
8. Platform-specific functionality should be isolated.
9. Services should be replaceable and testable.
10. Avoid unnecessary abstraction and premature optimization.

---

# 9. Database and Persistence

SQLite should store application-specific metadata and preferences.

Potential tables:

- bookmarks
- recent_locations
- recent_files
- tabs
- window_state
- user_preferences
- custom_actions
- saved_searches
- file_tags
- operation_history

Do not attempt to duplicate the entire filesystem database.

Filesystem metadata should generally be queried from the actual filesystem or GIO.

---

# 10. Performance Requirements

Performance is a first-class requirement.

### Directory Loading

- Load directory contents asynchronously.
- Avoid blocking the main thread.
- Support incremental loading for large directories.
- Avoid generating all thumbnails immediately.
- Virtualize large file lists.
- Cache reusable metadata.

### Thumbnail Generation

- Generate thumbnails in background workers.
- Cache thumbnails.
- Avoid repeated generation.
- Respect memory limits.
- Support cancellation when navigating away.

### File Operations

- Stream large file copies.
- Avoid loading entire files into memory.
- Support cancellation.
- Report accurate progress where possible.
- Handle disk space exhaustion.
- Handle permission failures.

### Resource Management

- Limit concurrent thumbnail jobs.
- Limit concurrent previews.
- Avoid excessive filesystem monitoring.
- Clean up stale cache entries.

---

# 11. Linux Integration

The application should support:

- Wayland.
- X11.
- Desktop entry integration.
- MIME associations.
- Default application handling.
- File manager D-Bus integration where appropriate.
- Freedesktop Trash specification.
- Freedesktop thumbnail conventions where applicable.
- XDG user directories.
- System themes.
- Desktop notifications.
- Mount management through GIO.
- Drag-and-drop using GTK facilities.

Special attention should be given to GNOME, KDE Plasma, and wlroots-based environments such as Hyprland.

---

# 12. Development Roadmap

## Phase 1 — Foundation

- Rust project initialization.
- GTK4 and Libadwaita setup.
- Main window.
- Navigation sidebar.
- Filesystem abstraction.
- Directory loading.
- Basic file listing.

## Phase 2 — Browsing Experience

- Grid view.
- List view.
- Column view.
- File thumbnails.
- Sorting.
- Filtering.
- Breadcrumb navigation.
- Hidden files.

## Phase 3 — File Operations

- Copy.
- Move.
- Rename.
- Delete.
- Create directory.
- Drag and drop.
- Clipboard integration.
- Operation manager.
- Progress dialogs.
- Conflict resolution.

## Phase 4 — Navigation

- Multiple tabs.
- Split view.
- Navigation history.
- Bookmarks.
- Recent files.
- Keyboard shortcuts.

## Phase 5 — Visual Polish

- Animations.
- Light and dark themes.
- Custom icons.
- Preview panel.
- Gallery view.
- Responsive layouts.
- UI refinement.

## Phase 6 — Advanced Functionality

- Archive management.
- Search.
- File properties.
- Permissions.
- Disk usage.
- Network locations.
- Terminal integration.
- Storage management.

## Phase 7 — Extensibility

- Plugin architecture.
- Custom actions.
- Git integration.
- Advanced metadata.
- Additional integrations.

---

# 13. MVP Definition

The first usable release should contain:

- Beautiful main window.
- Navigation sidebar.
- Breadcrumb navigation.
- Grid and list views.
- Directory navigation.
- File thumbnails.
- Sorting and filtering.
- Hidden file support.
- Copy, move, rename, and delete.
- New folder creation.
- Multiple tabs.
- Context menus.
- Basic file properties.
- Dark and light themes.

Everything else can follow.

**Do not attempt to implement every advanced feature before releasing a functional file manager.**

---

# 14. Product Quality Goals

The finished application should aim for:

- Fast directory navigation.
- Responsive UI regardless of directory size.
- Reliable large file operations.
- Minimal resource consumption.
- Consistent keyboard navigation.
- Excellent accessibility.
- Predictable behavior.
- Clean visual hierarchy.
- Native Linux integration.
- Maintainable Rust architecture.

## Final Product Identity

A Linux-native file manager that combines the visual elegance of Finder, the functionality of Dolphin, and the performance and reliability expected from a well-engineered Rust application.

**Design beautifully. Navigate effortlessly. Manage everything.**
