# Luma code review

Review of v0.3.0 (branch `main`, clean tree at the time of review).

**Status:** findings 1–9, 11, 14 and 18 are fixed in the working tree. Still open: 10, 12, 13 (partly: archive extraction and creation now use a no-replace rename; trash restore and batch rename do not), 15, 16, 17.

## Scope and limits

- **Read in full:** `core/*` (operations, undo, trash, rename, search, duplicates, permissions, filesystem, metadata), `integrations/*`, `services/archive`, `thumbnail`, `mounts`, the DB schema, `ui/operations.rs`, `ui/clipboard.rs`, CI, release and packaging files.
- **Spot-checked:** `window.rs`, `pane.rs`, the properties and cloud dialogs, `git_actions`.
- **Not read in depth:** `island*`, `sidebar`, `preview`, `quick_look`, the remaining dialogs, most of `window.rs` and `pane.rs`.
- **Not done:** the GUI was never run, so UI behaviour is judged from code only. Items marked *likely* are not reproduced.

## What works

- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are clean, 66/66 tests pass, `tools/check-version.sh` agrees.
- **File operations:** nothing is overwritten without an explicit Replace; a move that falls back to copy only deletes the source after a full successful copy; symlinks are copied as links; undo refuses to overwrite; `rename_many` stages through temporary names so swaps and cycles work.
- **Archives:** extraction never lands on an existing path, and `bsdtar` is run with argument lists.
- **Plugins and commands:** plugins are opt-in and run as argument lists, never shell strings.
- **Storage:** SQL is parameterised and migrations run in transactions.
- **Live updates:** folder watching is debounced and falls back to a full reload for large batches.
- **Desktop integration:** `org.freedesktop.FileManager1` is implemented, and CI builds on Ubuntu, Fedora and Arch.

## Findings (most severe first)

### 1. High: Git runs untrusted repo config — **Fixed**
`integrations/git.rs:115,237`. `git status` runs in any repo you browse, with no `-c core.fsmonitor=false`. A malicious `.git/config` from an extracted archive can execute commands. `status()` also has no timeout.
**Fix:** pass `-c core.fsmonitor=false -c core.hooksPath=/dev/null`, add a timeout, consider a "trust this repo" prompt.

### 2. High: closing the window kills running operations — **Fixed**
`ui/window.rs:693`. The close handler only saves window size. Closing the last window kills copy/move worker threads and leaves partial files.
**Fix:** block close, or confirm, while operations are running.

### 3. Medium: disk-full detection depends on English text — **Fixed**
`core/operations/mod.rs:377` matches "no space". GIO messages are localized.
**Fix:** keep the `IOErrorEnum` kind in `OpError` and test for `NoSpace`.

### 4. Medium: blocking filesystem calls on the main thread — **Fixed**
`ui/window.rs:713,1585-1589`, `ui/pane.rs:136,267`, `ui/pane/actions.rs:11`. `is_dir`/`exists`/`metadata` run on the UI thread (startup tab restore, Recent popover, location bar, drop). A stale network mount freezes the UI, which breaks the "never block GTK" rule.
**Fix:** use `spawn_blocking` or `query_info_future`.

### 5. Medium: recursive permission apply order and semantics — **Fixed**
`core/permissions/mod.rs:79`. The folder is chmodded before its children, so a mode without `x` makes every child fail. One mode is applied to files and folders alike, so "All" with 755 makes every file executable.
**Fix:** chmod children first; use separate file and folder modes (or `X` semantics).

### 6. Medium, *likely*: trash undo through symlinked paths — **Fixed**
`core/operations/trash.rs:73`. `topdir_of` works from the path string. On this machine `~/storage -> /mnt/storage` and the trash is `/mnt/storage/.Trash-1000`, so undo of a trash done via `~/storage/...` probably cannot find the item.
**Fix:** `fs::canonicalize` the parent first.

### 7. Medium: archive passphrase on the command line — **Fixed**
`services/archive.rs:203`. The passphrase is passed as `--passphrase`, visible in `ps` and `/proc/*/cmdline`.
**Fix:** use an option that reads it from a file descriptor, or another extractor.

### 8. Medium: partial extraction left behind — **Fixed**
`services/archive.rs:271-295`. In the single-top-level case, a failed extraction leaves partial output in the destination.
**Fix:** extract to a temp folder, then rename.

### 9. Medium: read-only folders block delete and Empty Trash — **Fixed**
`trash.rs:200`, `operations/mod.rs:544`. `remove_dir_all` and `delete_tree` fail on 0555 directories.
**Fix:** `chmod u+w` the failing directory and retry.

### 10. Medium: duplicates and search walk cloud mounts
`core/duplicates`, `core/search`. Neither skips cloud or gvfs mounts. Duplicates would hash all of Google Drive; search enumerates it (only content reads are skipped). Not confirmed whether the dialogs pass an exclusion.
**Fix:** exclusion list, defaulting to `cloud::base_dir()` and `/run/user/*/gvfs`.

### 11. Low-Medium: `unstage` fallback can stage deletions — **Fixed**
`integrations/git.rs:293-303`. It falls back to `git rm --cached -r` on any error, not only "no HEAD".
**Fix:** check `rev-parse --verify HEAD` first.

### 12. Low-Medium: thumbnails
`services/thumbnail.rs:85`. Images are fully decoded each time, with no on-disk XDG thumbnail cache. The cache is wiped wholesale at 600 entries. No PDF or video thumbnails unless the system already made them.
**Fix:** follow the freedesktop thumbnail spec; LRU eviction.

### 13. Low: check-then-rename races
`trash.rs:67`, `rename.rs:217`, `archive.rs:332`. The existence check and `rename(2)` are separate, and `rename(2)` overwrites silently if the target appears in between.
**Fix:** `renameat2(RENAME_NOREPLACE)`, or `link` + `unlink`.

### 14. Low: archive listing truncates some names — **Fixed**
`archive.rs:232`. `parse_line` cuts at " -> " even for non-symlinks, so `a -> b.txt` is truncated.
**Fix:** cut only when the permissions start with `l`.

### 15. Low: progress overshoot on cross-device moves
`operations/mod.rs`. The fallback copy makes progress exceed the total, since the total counts only top-level sources.
**Fix:** run `scan()` for moves too.

### 16. Low: plugin placeholders
`integrations/plugins.rs:134-139`. `%n`/`%b` can start with `-` (option injection). `to_string_lossy` mangles non-UTF-8 names.
**Fix:** document it, or offer a `--` convention.

### 17. Low: unbounded rclone cache
`integrations/cloud.rs:65`. `--vfs-cache-mode full` has no size limit.
**Fix:** add `--vfs-cache-max-size`.

### 18. Low: legacy temp-file prefix — **Fixed**
`rename.rs:204`. `.fm-rename-…` is from the old app name; a stale file from a crashed run could be overwritten.
**Fix:** rename the prefix and use `create_new`.

## Hygiene and docs

- `bugs.md` sits in the repo root although it says "all fixed". Move it to `docs/` or fold it into the changelog.
- CHANGELOG has no 0.2.0, and 0.1.0 and 0.3.0 share a date.
- Leftover `fm-` names: CSS classes `fm-git-*`, thread names, `.fm-build/` in `.gitignore`.
- CI `audit` is `continue-on-error`, so advisories never fail a build.
- All 66 tests are on non-UI code. Untested: `undo::perform` for `Restore` and `TrashCreated`, cancel and pause, disk-full, symlinks inside `transfer`, `Pane` and `window`.
- All strings are hard-coded English (no gettext). Only 26 tooltip or accessible-label sites in `ui/`.
- Panics are mostly GTK downcasts, plus `expect("spawn operation thread")` and the `unreachable!` in `unique_name`. Low risk.
- SPEC lists an `operation_history` table that does not exist, and "freedesktop thumbnail conventions" (see finding 12).

## Suggested new features

1. **Operation history:** persist finished operations with "reveal" and "undo again".
2. **Pre-flight checks:** free-space check before a copy, a "Trash unsupported, delete permanently?" prompt for FUSE and remote mounts, optional verify-after-copy checksum.
3. **Duplicates actions:** delete, hardlink or move selected copies, keeping one.
4. **Tags as xattrs** (`user.xdg.tags`), so tags survive outside-Luma renames and are visible to other tools.
5. **Folder compare and sync,** extending the `compare` module from files to folders.
6. **Per-folder view and sort memory.**
7. **i18n** with gettext, and a screen-reader pass.
8. **Flatpak** packaging (portal-aware trash, mounts, terminal).
9. **"Trust this repo" toggle** for Git integration (ties into finding 1).
10. **Explicit, polkit-based "open as admin"** if wanted, keeping the rule against silent escalation.

## Suggested order of work

1. Quick, low-risk fixes: 1, 2, 3, 14, 18.
2. Next: 4, 5, 6, 9, 11, then 7 and 8.
3. After that: 10, 12, the test gaps, the hygiene items.
4. New features once the above is stable.
