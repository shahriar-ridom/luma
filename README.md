<p align="center">
  <img src="data/icons/hicolor/scalable/apps/dev.ridom.Luma.svg" width="96" alt="Luma icon">
</p>

<h1 align="center">Luma</h1>

<p align="center">
  <b>A file manager that feels calm and gets out of your way.</b><br>
  Fast, beautiful and built for Linux – with the polish of Finder and the power of Dolphin.
</p>

<p align="center">
  <a href="../../releases/latest"><b>Download</b></a> ·
  <a href="#get-luma">Install</a> ·
  <a href="#your-first-five-minutes">Get started</a> ·
  <a href="#everyday-tasks">How do I…?</a> ·
  <a href="#questions-and-fixes">Help</a>
</p>

<p align="center"><img src="docs/screenshots/list.png" width="780" alt="Luma showing a folder in list view"></p>

---

## Why Luma?

- **It looks like it belongs on your desktop.** A clean layout, soft animations, matching icons and both light and dark themes. It follows your system accent colour.
- **It never makes you wait.** Folders open instantly, thumbnails load quietly in the background, and copying a huge folder never freezes the window.
- **It protects your files.** Nothing is overwritten without asking, and **Undo** (<kbd>Ctrl</kbd>+<kbd>Z</kbd>) takes back renames, moves, copies and deletes to the Trash.
- **Everything is built in.** Zip and unzip, search, batch rename, duplicate finder, file compare, Git status, tags, previews, Google Drive and network shares – no extra apps needed.
- **Keyboard friendly.** Every action has a shortcut, and every shortcut can be changed.

| Icons | Gallery |
|---|---|
| ![Icon view with thumbnails](docs/screenshots/grid.png) | ![Gallery view](docs/screenshots/gallery.png) |

| Right-click menu | Quick Look (press Space) |
|---|---|
| ![Context menu](docs/screenshots/context-menu.png) | ![Quick Look preview](docs/screenshots/quick-look.png) |

## Get Luma

Grab the file for your system from the **[latest release](../../releases/latest)**.

| You use… | Download | Then run |
|---|---|---|
| **Ubuntu, Debian, Mint, Pop!_OS** | `luma_<version>_amd64.deb` | `sudo apt install ./luma_*_amd64.deb` |
| **Arch, CachyOS, Manjaro, EndeavourOS** | `luma-<version>-1-x86_64.pkg.tar.zst` | `sudo pacman -U luma-*.pkg.tar.zst` |
| **Fedora** | `luma-<version>-1.<fedora>.x86_64.rpm` | `sudo dnf install ./luma-*.rpm` |
| **Anything else** | `luma-<version>-x86_64.AppImage` | `chmod +x luma-*.AppImage` then double-click it |
| **You prefer a plain download** | `luma-<version>-x86_64-linux.tar.gz` | extract it, then `./install.sh` |

Not sure which one? Use the package for your distribution; if there isn't one, use the AppImage. ARM computers (Raspberry Pi 5, Apple-silicon Linux) can use the `aarch64` files.

**What your system needs:** GTK 4.12 and libadwaita 1.5 or newer – that means Ubuntu 24.04, Debian 13, Fedora 40, or any current rolling distribution. Package installs fetch everything for you. The AppImage carries its own copies.

<details>
<summary><b>Optional extras</b> – install these to unlock more features</summary>

| Install | You get |
|---|---|
| `git` | Git badges and actions inside repositories |
| `gvfs` and `gvfs-fuse` | Network shares (SFTP, SMB, FTP, WebDAV) and password-protected drives |
| `rclone` and `fuse3` | Google Drive as a normal folder |
| `poppler-utils` | Page count and author for PDFs |
| `ffmpeg` | Duration and resolution for videos |
| GStreamer plug-ins (`gst-plugins-good`, `gst-libav`) | Playing audio and video in Quick Look |

```sh
# Ubuntu / Debian
sudo apt install git gvfs gvfs-fuse rclone fuse3 poppler-utils ffmpeg
# Arch
sudo pacman -S git gvfs rclone fuse3 poppler ffmpeg gst-plugins-good
# Fedora
sudo dnf install git gvfs gvfs-fuse rclone fuse3 poppler-utils
```
</details>

<details>
<summary><b>Verify your download</b> (optional)</summary>

Every release includes `SHA256SUMS`:

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify <file> --repo shahriar-ridom/luma     # proves it was built by this project's CI
```
</details>

To **uninstall**, remove it the way you installed it (`apt remove luma`, `pacman -R luma`, `dnf remove luma`), delete the AppImage, or run `./install.sh --uninstall` from the tarball.

## Your first five minutes

1. **Open Luma.** It starts in your home folder. Your tabs and window size come back next time.
2. **Switch the view** with the four buttons at the top: icons, list, columns or gallery (<kbd>Ctrl</kbd>+<kbd>1</kbd> … <kbd>4</kbd>).
3. **Preview anything** – click a file and press <kbd>Space</kbd>. Press it again to close.
4. **Right-click** a file for everything you can do with it. Rarer actions live under **More**.
5. **Open the menu** (the **⋯** button, top right) to find new windows and tabs, Undo, Appearance, Connect to Server, Storage, and Keyboard Shortcuts.

> **Tip:** you can't break anything by exploring. Deleting moves files to the Trash, and <kbd>Ctrl</kbd>+<kbd>Z</kbd> undoes most things.

### A quick tour

| Part | What it does |
|---|---|
| **Sidebar** (left) | Favourites, your bookmarks, drives, network servers, Google Drive and tags. Drop files on an entry to copy or move them there. Hide it with <kbd>F9</kbd>. |
| **Back / forward / up** | Also on <kbd>Alt</kbd>+arrow keys and your **mouse's side buttons**. |
| **Path bar** | Click any folder in the path to jump there, or press <kbd>Ctrl</kbd>+<kbd>L</kbd> to type a path. |
| **Filter box** | Type to narrow the folder instantly (<kbd>Ctrl</kbd>+<kbd>F</kbd>). |
| **View options** (sliders button) | Sort order and showing hidden files. |
| **Clock button** | Folders and files you opened recently. |
| **Status bar** (bottom) | How many items, what's selected, the Git branch, and free space. |
| **Arrows button** | Appears while Luma is copying or compressing – watch, pause or cancel. |

## Everyday tasks

### Browse and find things

- **Many folders at once:** <kbd>Ctrl</kbd>+<kbd>T</kbd> opens a tab, <kbd>Ctrl</kbd>+<kbd>W</kbd> closes it, <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>T</kbd> brings back the one you just closed.
- **Two folders side by side:** press <kbd>F3</kbd> for **split view**. Each side has its own location, view and sorting.
- **Bookmark a folder:** <kbd>Ctrl</kbd>+<kbd>D</kbd>. Right-click a bookmark to rename or remove it, or drag to reorder.
- **Search:** <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> searches by name (use `*` as a wildcard), by text *inside* files, by type, size and date, with or without subfolders.
- **Show hidden files:** <kbd>Ctrl</kbd>+<kbd>H</kbd>.

### Copy, move and tidy up

- **Copy / cut / paste:** <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>X</kbd> / <kbd>V</kbd>. It even works with other apps like Nautilus or Dolphin.
- **Drag and drop:** drop on a folder, on a sidebar entry, or on empty space. Dragging within a drive **moves**; between drives it **copies**. Hold <kbd>Ctrl</kbd> to always copy.
- **If a file already exists**, Luma asks: *Replace*, *Skip*, *Keep Both*, or apply your choice to all (*Replace All* / *Skip All*). *Compare* shows which one is newer.
- **Rename:** <kbd>F2</kbd>. **Select several files and press <kbd>F2</kbd>** to rename them all – find and replace, add a prefix or suffix, number them (`Holiday 01`, `Holiday 02`…) or change case, with a live preview.
- **Delete:** <kbd>Delete</kbd> moves to the Trash. <kbd>Shift</kbd>+<kbd>Delete</kbd> deletes for good, after asking.
- **Changed your mind?** <kbd>Ctrl</kbd>+<kbd>Z</kbd>.
- **Find duplicate files** and free up space: *⋯ menu → Find Duplicate Files…*. Luma only offers files that are genuinely identical, sends extras to the Trash, and always keeps one copy.
- **Compare two files:** select both, right-click → **More → Compare Files…**.

### Zip, unzip and archives

- **Unzip:** double-click or press <kbd>Enter</kbd> on a `.zip`, `.tar.gz`, `.7z`, `.rar` and friends. Luma extracts next to the archive and never overwrites – if there are several items inside, it puts them in a new folder named after the archive.
- **Look inside first:** right-click → **Archive → Browse Contents…**.
- **Make an archive:** select files, right-click → **Compress…**, pick a name, format (zip, tar.gz, tar.xz, tar.zst, 7z) and how small to make it.
- **Password-protected archive?** Luma asks for the password when needed.

### Preview with Quick Look

Press <kbd>Space</kbd> on a file. Luma shows images (with camera details), text and code, Markdown, folders, and plays **audio and video automatically**. Use the arrow keys to flip through files, <kbd>Enter</kbd> to open, <kbd>Space</kbd> or <kbd>Esc</kbd> to close. Playback stops the moment you close it.

### Organise with tags and Git

- **Tags:** <kbd>Alt</kbd>+<kbd>T</kbd> adds coloured tags to the selection. They show as dots next to the name, and each tag gets a sidebar entry that lists everything carrying it.
- **Git:** inside a repository, files show small badges (**M** modified, **A** added, **U** untracked…), the status bar shows your branch, and right-click → **More → Git** can stage, unstage, discard, commit, pull and push.

### See file details

Select a file (or nothing, for the current folder) and press <kbd>Alt</kbd>+<kbd>Enter</kbd>:

- size (folders are measured in the background), dates, owner and permissions
- photo camera settings, audio tags, PDF and video details
- **Checksums** (SHA-256, SHA-1, MD5, SHA-512) to verify a download
- **Permissions** – tick boxes for read/write/execute, optionally for everything inside a folder

### Terminal and "Open with"

<kbd>F4</kbd> opens a terminal right here. Right-click → **Open With…** picks any app. <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies the file's path.

## Connect to the cloud, servers and drives

### Google Drive

1. Install `rclone` and `fuse3` (see [Optional extras](#get-luma)).
2. In the sidebar choose **Add Google Drive…**, name it, and pick *Full access* or *Read-only*.
3. Your browser opens so you can sign in to Google. When you return, Drive appears as a normal folder.

Copy, paste, drag and drop and previews all work on it. Luma never sees your Google password; the sign-in is handled by rclone. To disconnect, use the eject button next to the drive; to sign out completely, right-click it → **Remove Account…**. Your files on Drive are never touched.

### Network shares

*⋯ menu → **Connect to Server…*** (<kbd>Ctrl</kbd>+<kbd>K</kbd>) and enter an address such as:

```text
sftp://you@myserver/home/you       smb://nas/media       ftp://example.com       davs://cloud.example.com/remote.php/webdav
```

Tick **Remember this server** and it appears in the sidebar for one-click reconnects. Needs `gvfs` and `gvfs-fuse`.

### USB drives and disks

Plugged-in drives appear under **Devices** automatically. Click an unmounted one to mount it, use the eject button to remove it safely. For capacity bars and a "what's using my space?" view, open *⋯ menu → **Storage & Disk Usage…***.

## Make it yours

- **Light or dark:** *⋯ menu → Appearance*. Luma follows your system by default.
- **Change any shortcut:** *⋯ menu → Keyboard Shortcuts*. Click a shortcut, press the new keys. <kbd>Backspace</kbd> turns one off.
- **Your own commands:** *⋯ menu → Custom Actions & Plugins…* adds items to the right-click menu, such as "Open in VS Code" or "Resize image". See the [guide](docs/plugins.md).
- **Choose your terminal:** Luma uses your default (or `$TERMINAL`). To force one: `sqlite3 ~/.local/share/luma/luma.sqlite "insert or replace into user_preferences values ('terminal','kitty');"`

### From the command line

```sh
luma                      # reopen where you left off
luma ~/Pictures ~/Music   # open folders in tabs
luma ~/Pictures/a.png     # open the folder with the file selected
```

Luma also answers "show in folder" requests from browsers and chat apps (when no other file manager is handling them).

## Cheat sheet

| | | | |
|---|---|---|---|
| <kbd>Space</kbd> Quick Look | <kbd>F2</kbd> Rename | <kbd>Ctrl</kbd>+<kbd>Z</kbd> Undo | <kbd>F3</kbd> Split view |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> New tab | <kbd>Ctrl</kbd>+<kbd>L</kbd> Type a path | <kbd>Ctrl</kbd>+<kbd>F</kbd> Filter | <kbd>F4</kbd> Terminal here |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>–<kbd>4</kbd> Views | <kbd>Ctrl</kbd>+<kbd>H</kbd> Hidden files | <kbd>Ctrl</kbd>+<kbd>D</kbd> Bookmark | <kbd>Alt</kbd>+<kbd>Enter</kbd> Properties |
| <kbd>Alt</kbd>+<kbd>←</kbd> <kbd>→</kbd> <kbd>↑</kbd> Navigate | <kbd>Delete</kbd> Trash | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> New folder | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd> Search |

The [full list of shortcuts](docs/shortcuts.md) is also in the app under *⋯ menu → Keyboard Shortcuts*.

## Your files are safe

- Luma **never overwrites** a file without you choosing *Replace*.
- Moving between drives only removes the original **after** everything was copied successfully.
- Extracting, renaming and compressing pick a new name rather than replace anything.
- Removing duplicates sends them to the Trash and keeps at least one copy.
- Changing permissions never follows shortcuts (symlinks) and Luma never asks for administrator rights on its own.
- Custom commands run directly, not through a shell, and plugins stay **off** until you turn them on.

## Questions and fixes

<details><summary><b>Icons look odd or are missing</b></summary>
Delete <code>~/.cache/luma</code>. Luma recreates its icons on the next start.
</details>

<details><summary><b>I picked Light but it stays dark</b></summary>
Some desktop theming tools write their own <code>~/.config/gtk-4.0/gtk.css</code> that overrides the colours. Luma follows it.
</details>

<details><summary><b>Network folders are empty or won't connect</b></summary>
Install <code>gvfs</code> and <code>gvfs-fuse</code>, then log out and back in so the background service starts.
</details>

<details><summary><b>Google Drive says rclone is needed or won't mount</b></summary>
Install <code>rclone</code> and <code>fuse3</code>. If it still fails, check that <code>fusermount3</code> is installed.
</details>

<details><summary><b>Audio or video won't play in Quick Look</b></summary>
Install GStreamer plug-ins (<code>gst-plugins-good</code>, <code>gst-libav</code>) and your distribution's "GTK 4 media GStreamer" package.
</details>

<details><summary><b>Zip/unzip does nothing</b></summary>
Install <code>bsdtar</code> (<code>libarchive-tools</code> on Ubuntu/Debian, <code>libarchive</code> on Arch). The Debian, Arch and Fedora packages already depend on it; the AppImage includes it.
</details>

<details><summary><b>My mouse's back/forward buttons don't work</b></summary>
They need to reach apps as buttons 8 and 9. If a remapping tool (such as <code>input-remapper</code>) is changing them, make sure it passes them through.
</details>

<details><summary><b>Where does Luma keep my settings?</b></summary>
In <code>~/.local/share/luma/</code> (bookmarks, tags, tabs, preferences, plugins). Delete <code>luma.sqlite</code> to reset everything. If you used an earlier build under another name, your data is moved over automatically.
</details>

<details><summary><b>Something's wrong – how do I report it?</b></summary>
Open an <a href="../../issues">issue</a> and include your distribution, how you installed Luma, and what you did. Running <code>RUST_LOG=debug luma</code> in a terminal prints helpful details.
</details>

## What's not there yet

Luma is young. Planned but not built: an embedded terminal panel, video thumbnails, extra list columns (created date, permissions, owner), selecting files by pattern, and a settings window (preferences are currently edited through menus). The full list is in [docs/development.md](docs/development.md#status-against-the-specification).

## For contributors

Want to build Luma, add a feature or cut a release? Start with **[docs/development.md](docs/development.md)** – it covers building from source, the architecture, tests, and how the CI and release pipeline work.

## License

Luma is free software, released under the **GNU General Public License v3.0** – see [LICENSE](LICENSE).
Contact: <info@ridom.dev>.
