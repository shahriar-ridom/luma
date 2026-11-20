# Bugs

Audit of v0.4.0 (`main` at `85291a7`, 2026-11-20). The earlier list (items 1–11, all fixed in v0.3.0) is in git history at `49cb044`.

## How this was checked

- **Baseline:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (75/75 pass) and `tools/check-version.sh` are all clean.
- **Code read in full:** `core/*`, `integrations/*`, `services/archive`, `services/thumbnail`, `services/mounts`, `application/*`, `ui/operations.rs`, `ui/globalmenu.rs`, `ui/menubar.rs`, `ui/breadcrumb.rs`, `ui/pane/actions.rs`, the delete, rename, conflict, connect, compress and properties dialogs.
- **Spot-checked:** `window.rs`, `pane.rs`, `sidebar.rs`, `trash_view.rs`, `preview.rs`.
- **Reproduced:** each item marked **Confirmed** was reproduced, either by a throw-away test against the real code or by a shell repro. The probes were removed afterwards, so the tree is unchanged. The suggested regression tests below are based on those probes.
- **GUI:** the debug build was run with its `FM_ISLAND_DEMO` snapshot hook, using separate `XDG_*` folders so no user data was touched. The screenshots were used for the UI items.
- **Not done:** no `cargo audit` (not installed locally, and CI runs it with `continue-on-error`). Case-insensitive file systems were not mounted. Items marked **Likely** were not reproduced.

## Summary

| # | Severity | Area | Status |
|---|----------|------|--------|
| 1 | High | Git status runs commands from an untrusted repo's config | Confirmed |
| 2 | Medium | Network passwords stored in plain text and shown in the sidebar | Confirmed |
| 3 | Medium | Moving a folder onto a folder with the same name fails instead of merging | Confirmed |
| 4 | Medium | Undo of a folder moved across drives always fails | Confirmed |
| 5 | Medium | Compress silently overwrites, then deletes, an existing `NAME.part` file | Confirmed |
| 6 | Medium | Recursive permission change: no confirmation, makes every file executable | Code |
| 7 | Low-Med | Compress fails for files whose names start with `@` | Confirmed |
| 8 | Low-Med | Compare (and Properties checksum) hangs on a FIFO | Confirmed |
| 9 | Low-Med | Breadcrumb hides the current folder after a resize or split | Confirmed (screenshot) |
| 10 | Low-Med | Trash on a drive without trash support: only "Retry", which fails again | Code |
| 11 | Low-Med | Case-only rename fails on FAT/exFAT/NTFS drives | Likely |
| 12 | Low | "Open Terminal" leaves a zombie process each time | Confirmed |
| 13 | Low | Main-thread `stat` in the Properties, Compare and Tags dialogs | Code |
| 14 | Low | Compress dialog drops invalid names silently; Connect dialog forgets fields on retry | Code |
| 15 | Low | Rename and New prompts trim spaces, so names change unasked | Code |
| 16 | Low | Conflict dialog has 7 buttons, 2 of them destructive | UI |
| 17 | Low | No Quit / Close Window in the menu bar, no Ctrl+Q | UI |
| 18 | Low | Image thumbnails have no pixel-count limit (decompression bomb) | Likely |
| 19 | Low | Global menu registers the process id as the X11 window id | Likely |
| 20 | Low | Every `luma` launch opens the database and scans plugins, even when it only hands off | Code |
| 21 | Low | Test `move_and_skip_keeps_source` passes for the wrong reason | Confirmed |
| — | — | Still open from `docs/review.md`: 10, 12, 13, 15, 16, 17 | Code |

---

## Security

### 1. High: Git status runs commands from an untrusted repo's config

**Where:** `src/integrations/git.rs:115` (`git()`), used by `status()` every time you open a folder inside a repository.

**Problem:** `core.fsmonitor` and hooks are switched off, but filter drivers are not. A repository can set `filter.<name>.clean` (or `.process`) in `.git/config` and assign it with `.gitattributes`. `git status` runs the clean filter whenever a tracked file's stat data changed, which is what happens after an archive is extracted. Simply browsing an extracted folder runs the attacker's command. Stage/Commit/Discard go through the same `git()`.

**Repro (confirmed, same flags as Luma):**
```sh
git init evil && cd evil && echo '* filter=x' > .gitattributes && echo hi > f.txt
git add . && git commit -qm init
git config filter.x.clean "sh -c 'touch /tmp/PWNED; cat'"
echo changed >> f.txt
git -c core.fsmonitor=false -c core.hooksPath=/dev/null -c protocol.ext.allow=never \
    status --porcelain=v2 --branch -z --ignored=traditional --untracked-files=normal
ls /tmp/PWNED   # exists
```
Adding `-c filter.x.clean= -c filter.x.process=` stops it (verified).

**Fix:** blank every filter command the repository itself defines (local/worktree scope). Leave user-level filters such as `git-lfs` alone so LFS status stays correct. Skip submodules, which have their own config. If the config can't be read, fail closed.
```rust
fn base_git(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "protocol.ext.allow=never"]);
    c.arg("-C").arg(root);
    c.env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0");
    c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    c.stdin(Stdio::null());
    c
}

/// Filter commands defined by the repository itself (`local`/`worktree`
/// scope, includes followed). These run on `status`/`add`, so they are
/// switched off. `None` if the config could not be read.
fn repo_filter_keys(root: &Path) -> Option<Vec<String>> {
    let mut c = base_git(root);
    c.args(["config", "--includes", "--show-scope", "--name-only", "--get-regexp",
            r"^filter\..*\.(clean|smudge|process)$"]);
    let out = output_with_timeout(c, Duration::from_secs(5))?;
    // Exit code 1 = no matches.
    if !out.status.success() && out.status.code() != Some(1) {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(scope, _)| matches!(*scope, "local" | "worktree" | "command"))
        .map(|(_, key)| key.to_string())
        .collect())
}

fn git(root: &Path) -> Option<Command> {
    let keys = repo_filter_keys(root)?;          // fail closed
    let mut c = base_git(root);
    for k in keys {
        c.arg("-c").arg(format!("{k}="));        // empty command = filter off
    }
    Some(c)
}
```
In `status()`, also pass `--ignore-submodules=all`. Change the callers to handle `None`. For `status` that means no badges; for actions, an error like "This repository's config could not be read".

**Regression test:** the repro above as a test in `integrations::git::tests`. Point the filter at a file in `target/test-tmp`, call `status()`, and assert the file was not created.

### 2. Medium: Network passwords stored in plain text and shown in the sidebar

**Where:** `src/integrations/network.rs:21` (`normalize`), `src/ui/sidebar.rs:336`, `src/ui/dialogs/connect.rs`.

**Problem:** `sftp://me:hunter2@box/srv` passes validation unchanged. "Remember this server" is on by default, so the URI goes into `connections` in `luma.sqlite`, and `label_for` uses it as the sidebar label. The password then appears in plain text in the database and on screen.

**Repro (confirmed):** `normalize("sftp://me:hunter2@box/srv")` gives `sftp://me:hunter2@box/srv`, and `label_for` gives `me:hunter2@box/srv (SFTP)`.

**Fix:** remove the password from the URI before connecting. GVfs then asks through `gtk::MountOperation`, which can store it in the keyring. Also clean up rows that were already saved.
```rust
/// `user:pass@host` → `user@host`; the password goes through GVfs' prompt/keyring instead.
fn strip_password(rest: &str) -> String {
    let (auth, path) = rest.split_once('/').map_or((rest, None), |(a, p)| (a, Some(p)));
    let auth = match auth.rsplit_once('@') {
        Some((userinfo, host)) => {
            let user = userinfo.split_once(':').map_or(userinfo, |(u, _)| u);
            format!("{user}@{host}")
        }
        None => auth.to_string(),
    };
    match path { Some(p) => format!("{auth}/{p}"), None => auth }
}
// in normalize(): Ok(format!("{scheme}://{}", strip_password(rest)))
```
Startup migration: for each saved connection, if `strip_password(uri) != uri`, update both the URI and the label. Test: `normalize("sftp://me:pw@h/x") == "sftp://me@h/x"`.

---

## Correctness

### 3. Medium: Moving a folder onto a folder with the same name fails instead of merging

**Where:** `src/core/operations/mod.rs:418-447` (`transfer`).

**Problem:** for two folders, `resolve()` returns `Proceed { overwrite: false }`, which means "merge". `g_file_move` without `OVERWRITE` then fails with `Exists` right away, so the `WouldMerge` fallback is never reached. Dragging `a/Photos` into a folder that already has `Photos`, or cutting and pasting it there, always ends with "Moving failed: File exists". The user is never asked about the files inside.

**Repro (confirmed):** Move `a/dir` (holding `new.txt`) into `b/`, where `b/dir/old.txt` exists. The result is the error `Error moving file …/a/dir: File exists`, and `b/dir/new.txt` is never created.

**Fix:** handle a merge before calling `move_`. Move child by child (a rename where possible), asking about each conflict, and delete the source folder only when it is empty:
```rust
// in transfer(), after resolve():
if mv && !overwrite && file_type(&dst) == gio::FileType::Directory
    && file_type(src) == gio::FileType::Directory
{
    return move_merge(ctx, src, &dst);
}

fn move_merge(ctx: &mut Ctx, src: &gio::File, dst: &gio::File) -> bool {
    let cancel = ctx.cancel.clone();
    let en = match src.enumerate_children("standard::name", NOFOLLOW, Some(&cancel)) {
        Ok(en) => en,
        Err(e) => { ctx.error(src, &e); return false; }
    };
    let mut ok = true;
    for child in en.flatten() {
        if ctx.cancelled() { return false; }
        let csrc = src.child(child.name());
        match ctx.resolve(&csrc, dst.child(child.name())) {
            Resolved::Proceed { dst: cdst, overwrite } => {
                let both_dirs = !overwrite
                    && file_type(&cdst) == gio::FileType::Directory
                    && file_type(&csrc) == gio::FileType::Directory;
                ok &= if both_dirs { move_merge(ctx, &csrc, &cdst) } else { move_one(ctx, &csrc, &cdst, overwrite) };
            }
            Resolved::Skip => ok = false,
            Resolved::Cancel => return false,
        }
    }
    if ok {
        if let Err(e) = src.delete(Some(&cancel)) { ctx.error(src, &e); ok = false; }
    }
    ok
}
```
`move_one` is the current `move_` call plus its `WouldRecurse`/`WouldMerge` fallback, moved into its own function. A merge already sets `not_undoable`, so undo stays correct.

**Regression test:** the repro above. Assert that `b/dir/new.txt` and `b/dir/old.txt` both exist and `a/dir` is gone.

### 4. Medium: Undo of a folder moved across drives always fails

**Where:** `src/core/operations/undo.rs:77-85` (`MoveBack`).

**Problem:** the forward move falls back to copy-then-delete for folders across drives, but undo calls `g_file_move` directly. GIO can't move a folder between file systems (`WouldRecurse`). Undo then reports an error and nothing moves back.

**Repro (confirmed):** move `/dev/shm/x/folder` (tmpfs) into a folder on ext4, then run `MoveBack`. The result is `Err("0 undone; …/folder: Can’t recursively copy directory")`.

**Fix:** give undo the same fallback. Undo already runs on a worker (`run_job`, `window.rs:1552`), so blocking is fine. Expose a headless move in `core::operations` that reuses `copy_tree`/`delete_tree` and never replaces anything:
```rust
/// Move `src` to exactly `dst` (never replacing), falling back to
/// copy + delete across file systems. Blocking; call from a worker.
pub fn move_exact(src: &gio::File, dst: &gio::File) -> Result<(), String> {
    let flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
    match src.move_(dst, flags, gio::Cancellable::NONE, None) {
        Ok(()) => Ok(()),
        Err(e) if e.matches(gio::IOErrorEnum::WouldRecurse) => {
            if file_type(dst) != gio::FileType::Unknown {
                return Err(format!("{} already exists", dst.uri()));
            }
            let (tx, _rx) = async_channel::unbounded();
            let mut ctx = Ctx::headless(tx);           // policy = Some(SkipAll)
            if copy_tree(&mut ctx, src, dst, false) && delete_tree(&mut ctx, src, false) {
                Ok(())
            } else {
                Err(ctx.errors.first().map(|e| e.message.clone()).unwrap_or_else(|| "move failed".into()))
            }
        }
        Err(e) => Err(e.message().to_string()),
    }
}
```
(`Ctx::headless` is a constructor like the one in `spawn()`, with `policy: Some(Resolution::SkipAll)`.) Then `MoveBack` calls `move_exact`.

### 5. Medium: Compress silently overwrites, then deletes, an existing `NAME.part` file

**Where:** `src/services/archive.rs:350` (`create`).

**Problem:** the temporary output is always `<out>.part`. If a file with that name already exists (a partial download, for example), bsdtar truncates it and writes the archive into it. After the rename, or after a failure, it is deleted. The user's file is lost with no prompt, which breaks the rule against silent overwrites.

**Repro (confirmed):** create `out.zip.part` containing "PRECIOUS USER DATA", then compress `a.txt` to `out.zip`. Afterwards `out.zip.part` no longer exists.

**Fix:** reserve a hidden temp name that is unique and created atomically:
```rust
fn temp_output(out: &Path) -> Result<PathBuf, ArchiveError> {
    let dir = out.parent().unwrap_or(Path::new("."));
    let pid = std::process::id();
    for n in 0u32..1000 {
        let p = dir.join(format!(".luma-archive-{pid}-{n}.part"));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),          // bsdtar -cf truncates and reuses it
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ArchiveError::Failed(e.to_string())),
        }
    }
    Err(ArchiveError::Failed("could not create a temporary file".into()))
}
// let part = temp_output(out)?;
```
Test: the repro above. Assert that `out.zip.part` still holds its original content.

### 6. Medium (UI/safety): recursive permission change has no confirmation and makes every file executable

**Where:** `src/core/permissions/mod.rs:71` (`apply`), `src/ui/dialogs/properties.rs:395-433`.

**Problem:**
- With scope "All", the folder's mode is applied to every file. Setting a folder to `755` recursively makes every document executable. Folders get `X` semantics (`folder_mode`), but files don't.
- One click on **Apply** changes possibly thousands of files. There is no confirmation, no count of what will change, and no undo.
- `cancel` is tied to the dialog's `closed` signal. Closing the dialog during a long apply stops it halfway without saying so, because the result toast lives in the dialog that was just closed.

**Fix:**
1. Give files `X` semantics too: keep an execute bit only where the file already had one.
   ```rust
   pub fn file_mode(new: u32, current: u32) -> u32 {
       (new & !0o111) | (new & current & 0o111)
   }
   // in apply(): for files, set_mode(&p, file_mode(mode, md.mode()))
   ```
   Alternatively, show separate "Files" and "Folders" mode rows when the scope is recursive.
2. Before any recursive scope, count items with `dir_stats` and ask: *"Change permissions of 1,204 items in “Photos”? This can't be undone."* Use `dialogs::confirm(..., destructive = true)`.
3. Don't cancel on close. Run the apply as `ops.run_job`, which has its own island entry, cancel button and toast.

### 7. Low-Med: Compress fails for files whose names start with `@`

**Where:** `src/services/archive.rs:369-370`.

**Problem:** in create mode, bsdtar treats an argument `@x` as "copy the entries of archive `x`", even after `--`. Compressing `@notes.txt` fails with `bsdtar: Failed to open 'notes.txt'`. If a file named `notes.txt` exists, the result is wrong without any error.

**Repro (confirmed):** compressing `@notes.txt` gives `Err(Failed("bsdtar: Failed to open 'notes.txt'"))`.

**Fix:** pass the names on stdin as a NUL-separated list. `-T` reads them literally (verified: entries come out as `@n.txt`, with no `./` prefix):
```rust
cmd.arg("-cf").arg(&part).arg("-C").arg(base_dir).args(["--null", "-T", "-"]);
cmd.stdin(Stdio::piped());
// after spawn: write each name followed by '\0' to child.stdin, then drop it.
```
`run()` and `spawn_bsdtar()` need a small change to accept stdin bytes, not only a passphrase.

### 8. Low-Med: Compare (and Properties checksum) hangs on a FIFO

**Where:** `src/ui/pane/actions.rs:135` (`can_compare`), `src/core/compare/mod.rs:76`, `src/core/metadata/mod.rs` (`checksum_prefix`).

**Problem:** `can_compare` only excludes folders, so FIFOs, sockets and device nodes pass. `File::open` on a FIFO blocks until a writer appears. Nothing checks the cancel flag before the open, so the worker thread hangs forever and the Compare dialog spins forever. Properties is safe for checksums because it requires `md.is_file()`, but `compare` isn't.

**Repro (confirmed):** `compare(fifo, file)` still had not returned after 2 s.

**Fix:**
```rust
// can_compare(): only regular files
infos.len() == 2 && infos.iter().all(|i| i.file_type() == gio::FileType::Regular)

// core::compare::compare(), before opening anything:
let (ma, mb) = (fs::metadata(a)?, fs::metadata(b)?);
if !ma.is_file() || !mb.is_file() {
    return Err(io::Error::other("only regular files can be compared"));
}
```
Add the same `is_file()` check to `checksum_prefix`, so any future caller is protected too.

### 9. Low-Med: Breadcrumb hides the current folder after a resize or split

**Where:** `src/ui/breadcrumb.rs:57-59`.

**Problem:** the "scroll to the end" runs once, in an idle callback after `set_path`. If the pane has no final width yet (a new split, startup), or it is resized later, the crumbs stay scrolled to the left. The current folder, the most important crumb, is cut off. The scroll policy is `External`, so there is no scrollbar to show that more is hidden.

**Seen (screenshot `10-split.png`):** the left pane shows `/ › mnt › storage › code › file-manag` with the current folder `src` invisible and the name cut mid-word.

**Fix:** keep the view pinned to the end whenever the content or width changes:
```rust
let adj = widget.hadjustment();
let pin = |a: &gtk::Adjustment| a.set_value(a.upper() - a.page_size());
adj.connect_upper_notify(pin);
adj.connect_page_size_notify(pin);
```
Add a fade or "…" at the left edge when it's scrolled, so cut-off crumbs are visible as such.

### 10. Low-Med (UI): Trash on a drive without trash support offers only "Retry"

**Where:** `src/ui/operations.rs:273-304`, `src/core/operations/mod.rs:364`.

**Problem:** on FAT USB sticks without `.Trash-UID`, some FUSE or network mounts, and read-only media, `g_file_trash` fails with `NotSupported`. The island offers **Retry**, which can only fail the same way. The user has to know about **Delete Permanently** in a submenu.

**Fix:** keep the error kind in `OpError` (`pub not_supported: bool`, set in `Ctx::error` from `e.matches(gio::IOErrorEnum::NotSupported)`). In `finished()`, for `OpKind::Trash` when every failure is `not_supported`, offer **"Delete Permanently…"** in place of Retry. It goes through `dialogs::confirm_delete` and then submits `OpKind::Delete` for `failed_sources`.

### 11. Low-Med (Likely): Case-only rename fails on FAT/exFAT/NTFS drives

**Where:** `src/ui/pane.rs:957-976` (single rename via `set_display_name`), `src/core/operations/rename.rs:201,224` (`rename_many`).

**Problem:** on a case-insensitive file system, `A.TXT` already "exists" when renaming `a.txt`, because it is the same file. GIO's `set_display_name` returns `Exists`, and `rename_many` returns "A.TXT already exists". It is impossible to change only the case of a name on most USB sticks. Nautilus has special handling for this. Not reproduced here, because it needs a vfat mount.

**Fix:** treat "the target is the source itself" as free, and go through a temporary name:
```rust
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    matches!((fs::symlink_metadata(a), fs::symlink_metadata(b)),
             (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino())
}
// rename_many: `if fs::symlink_metadata(to).is_ok() && !sources.contains(to) && !same_file(from, to)`
// phase 2: same exception (the temp-name staging already makes the case flip work).
```
For a single rename, send case-only changes (`new.to_lowercase() == old.to_lowercase()`) through `rename_many` instead of `set_display_name`.

### 12. Low: "Open Terminal" leaves a zombie process each time

**Where:** `src/integrations/terminal.rs:58-66`.

**Problem:** the `Child` is dropped without being waited on. Each terminal you close stays a zombie until Luma exits. (`plugins::spawn_detached` already reaps its child correctly.)

**Repro (confirmed):** `open_in("/tmp", "true")` raised the zombie count from 0 to 1.

**Fix:** reap in the background, as `spawn_detached` does:
```rust
let mut child = Command::new(&term)/* … */.spawn().map_err(|e| format!("Could not start {term}: {e}"))?;
std::thread::spawn(move || { let _ = child.wait(); });
Ok(term)
```

### 13. Low: Main-thread `stat` in dialogs (breaks the "never block GTK" rule)

**Where:** `src/ui/dialogs/properties.rs:113,122` (`symlink_metadata`, `read_link`), `src/ui/dialogs/compare.rs:17` (`facts`), `src/ui/dialogs/tags.rs:166-167` (`exists()` per row), `src/ui/pane/actions.rs:9-14` (`unique_file`, called on a worker in one place).

**Problem:** on a stale SMB/NFS/SSHFS mount, opening Properties, Compare or a tag list freezes the whole window.

**Fix:** gather the facts with `gio::spawn_blocking` (or `query_info_future`) before building the rows, with a "Loading…" row meanwhile, as the Recent popover already does (`window.rs:1660`).

---

## UI practice

### 14. Low: Compress drops invalid names silently; Connect forgets fields on retry

**Where:** `src/ui/dialogs/archive.rs:78-81`, `src/ui/dialogs/connect.rs:13-60`.

**Problem:**
- In Compress, an empty name or one with `/` returns `None`. The dialog closes and nothing happens. This is the same problem as the old bug #9, fixed for New/Rename but not here.
- In Connect, when the address is invalid the dialog reopens with the address kept. The **Name** field and the **Remember** box are rebuilt empty and reset to their defaults.

**Fix:** use the same loop as `prompt_name`: on `invalid_name_reason(&format!("{nm}.{ext}"))`, reopen with the reason and the `error` CSS class, keeping all fields. In Connect, keep `name` and `remember` in variables outside the loop, as `last` already is.

### 15. Low: Rename and New prompts trim spaces, so names change unasked

**Where:** `src/ui/dialogs/mod.rs:155` (`entry.text().trim()`).

**Problem:** renaming `" notes.txt"` and pressing Enter without editing renames it to `notes.txt`. The `new == old` check compares the trimmed text, so it doesn't catch this. Names with leading or trailing spaces can't be created at all.

**Fix:** validate the trimmed text but keep the raw text:
```rust
let raw = entry.text().to_string();
text = raw.clone();
match invalid_name_reason(if raw.trim().is_empty() { "" } else { &raw }) { None => return Some(raw), … }
```
Optionally show a warning ("Name starts or ends with a space") without blocking.

### 16. Low: Conflict dialog has 7 buttons, 2 of them destructive

**Where:** `src/ui/dialogs/mod.rs:57-67`.

**Problem:** Cancel / Compare / Skip / Skip All / Keep Both / Replace / Replace All. On a normal-width window libadwaita stacks them vertically. Two red buttons sit next to each other, which invites a mis-click on **Replace All**. The dialog also doesn't say how many conflicts are left.

**Fix:** use **Skip**, **Keep Both** and **Replace** (destructive), with Cancel as the close response. Add a check box **"Do this for all N remaining conflicts"**, which maps to `SkipAll`/`ReplaceAll`. Show Compare as an expander or a link in the body, not as a response. The worker can send the remaining conflict count with `Event::Conflict` so the label is right.

### 17. Low: No Quit / Close Window in the menu bar, no Ctrl+Q

**Where:** `src/ui/menubar.rs:37-73`, `src/application/actions.rs`.

**Problem:** the File menu (also exported as the global menu) ends with Delete Permanently. There is no Close Window or Quit item and no `<Control>q`. Most desktop apps have both, and global-menu users expect them.

**Fix:** add `app.quit` (with `<Control>q`). It should go through each window's close-request so the "operations still running" guard still applies. Also add `win.close` (`<Control><Shift>w`). Put both in a final File section.

---

## Lower-risk findings

### 18. Low (Likely): Image thumbnails have no pixel-count limit

**Where:** `src/services/thumbnail.rs:44,104`.

**Problem:** only the file size is capped (64 MB). A tiny PNG or GIF can declare 50 000 × 50 000 pixels. gdk-pixbuf's `from_file_at_scale` decodes it at full size for several formats, about 10 GB of RAM, and up to 4 decodes run at once. Just opening a folder with such a file can make the system run out of memory. Not reproduced here.

**Fix:**
```rust
if let Some((_, w, h)) = gdk_pixbuf::Pixbuf::file_info(path) {
    if (w as u64) * (h as u64) > 100_000_000 { return None; }   // > 100 MP: no thumbnail
}
```
Also skip thumbnails under `/run/user/<uid>/gvfs` and other network file systems, as is already done for cloud mounts (see the review's item 12).

### 19. Low (Likely): Global menu registers the process id as the X11 window id

**Where:** `src/ui/globalmenu.rs:289-308`.

**Problem:** `RegisterWindow(u windowId, o path)` expects an X11 window id. Sending the pid means a registrar that follows the protocol (Plasma under X11) links the menu to whatever window has that XID, or to none. On Wayland, Plasma uses the `org_kde_kwin_appmenu` protocol, not the registrar. The menu only works with hosts that map by sender pid.

**Fix:** under X11, register the real XID of each toplevel (`gdk4_x11::X11Surface::xid`) when it is mapped, and register again when the window changes. Under Wayland, set the KDE appmenu properties on the surface if possible, otherwise document that only pid-matching hosts are supported.

### 20. Low: Every `luma` launch opens the database and scans plugins, even when it only hands off

**Where:** `src/application/app.rs:18` (`AppState::open()` before `app.run()`).

**Problem:** `luma file.txt` from a terminal, or a desktop `%U` launch while Luma is already running, starts a second process. That process runs `migrate_legacy_data`, opens and migrates `luma.sqlite`, and loads every plugin manifest, only to hand the files to the main instance and exit. It is slower, and two processes can race on the legacy migration.

**Fix:** create `AppState` lazily in `connect_startup`, which runs only in the primary instance. Hold it in a `OnceCell<Rc<AppState>>` that the activate, open and D-Bus handlers read.

### 21. Low: Test `move_and_skip_keeps_source` passes for the wrong reason

**Where:** `src/core/operations/mod.rs:658-669`.

**Problem:** it expects the source to be kept after a Skip. That happens only because the move fails outright with `Exists` (bug 3), so no conflict is ever asked. Once bug 3 is fixed, the test must answer Skip for `f.txt` and check that `a/dir/f.txt` is still there and `b/dir/sub/g.txt` was moved. It should also check that the conflict count is 1.

---

## Still open from `docs/review.md`

Checked against the current code:

- **10. Duplicates and search walk cloud and GVfs mounts:** still open. `core/duplicates` has no exclusion list. Fix: skip `cloud::base_dir()` and `/run/user/*/gvfs` by default, and don't cross file systems unless asked (compare `md.dev()` with the root's).
- **12. Thumbnails:** still open. There is no XDG on-disk cache, the cache is wiped whole at 600 entries, and network mounts get thumbnails (see 18).
- **13. Check-then-rename races:** partly open. `trash::restore` (`trash.rs:63-67`) and phase 2 of `rename_many` (`rename.rs:224-227`) still check and then `fs::rename`. Fix: both should call the existing `filesystem::rename_noreplace`.
- **15. Progress overshoot on cross-device moves:** still open. Fix: run `scan()` for `OpKind::Move` too.
- **16. Plugin `%n` / `%b` option injection:** still open. A file named `-rf` becomes an option to the plugin's program. Fix: expand `%n`/`%b` as `./name` when the name starts with `-`, and document `--` in `docs/plugins.md`.
- **17. Unbounded rclone cache:** still open (`cloud.rs:60-71`). Fix: add `--vfs-cache-max-size 10G` (or a preference) and `--vfs-cache-max-age 24h`.
- **Hygiene:** the CI `audit` job still has `continue-on-error: true`, so RustSec advisories never fail a build.

## Suggested order

1. **Now:** 1 (security), 2, 5, 3 with 21 (one change plus its test), 12. All are small.
2. **Next:** 4, 6, 7, 8, 9, 10.
3. **Then:** 11, 13–17, and the remaining review items.
