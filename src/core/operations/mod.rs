//! Background file operations (copy / move / trash / delete).
//!
//! Each operation runs on its own worker thread using synchronous GIO calls
//! and talks to the UI exclusively through an `async_channel` of [`Event`]s.
//! Nothing here touches GTK widgets.
//!
//! Safety rules enforced here:
//! * existing files are never overwritten without an explicit `Replace`
//!   resolution; replacement goes through GIO's atomic replace;
//! * a move that falls back to copy+delete only deletes the source when every
//!   item was copied successfully;
//! * symlinks are copied as symlinks, never followed.

pub mod rename;
pub mod trash;
pub mod undo;

use gtk::gio::{self, prelude::*};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Copy,
    Move,
    Trash,
    Delete,
}

impl OpKind {
    pub fn verb(self) -> &'static str {
        match self {
            OpKind::Copy => "Copying",
            OpKind::Move => "Moving",
            OpKind::Trash => "Moving to Trash",
            OpKind::Delete => "Deleting",
        }
    }
    pub fn past(self) -> &'static str {
        match self {
            OpKind::Copy => "Copied",
            OpKind::Move => "Moved",
            OpKind::Trash => "Moved to Trash",
            OpKind::Delete => "Deleted",
        }
    }
}

/// Sources and destination are URIs so remote (GVfs) locations work and the
/// request is `Send`.
#[derive(Clone, Debug)]
pub struct OpRequest {
    pub kind: OpKind,
    pub sources: Vec<String>,
    pub dest: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Replace,
    Skip,
    Rename,
    ReplaceAll,
    SkipAll,
    Cancel,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub items_done: u64,
    pub items_total: u64,
    pub current: String,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        if self.total_bytes > 0 {
            (self.done_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
        } else if self.items_total > 0 {
            (self.items_done as f64 / self.items_total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpError {
    pub uri: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub cancelled: bool,
    pub errors: Vec<OpError>,
    /// Top-level sources that were not fully processed (for retry).
    pub failed_sources: Vec<String>,
    #[allow(dead_code)]
    pub items_done: u64,
    /// Top-level (source URI, final destination URI) of what succeeded; the
    /// destination is empty for trashed items.
    pub done: Vec<(String, String)>,
    /// False when something was replaced or merged into existing data, so
    /// the operation cannot be reversed safely.
    pub undoable: bool,
}

pub enum Event {
    Progress(Progress),
    Conflict { src: String, dst: String, reply: mpsc::Sender<Resolution> },
    Finished(Summary),
}

/// Handle for controlling a running operation from the UI thread.
#[derive(Clone)]
pub struct Control {
    pub cancel: gio::Cancellable,
    paused: Arc<AtomicBool>,
}

impl Control {
    pub fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::SeqCst);
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

pub fn spawn(req: OpRequest) -> (Control, async_channel::Receiver<Event>) {
    let (tx, rx) = async_channel::unbounded();
    let control = Control { cancel: gio::Cancellable::new(), paused: Arc::new(AtomicBool::new(false)) };
    let ctx = Ctx {
        tx,
        cancel: control.cancel.clone(),
        paused: control.paused.clone(),
        policy: None,
        p: Progress::default(),
        errors: Vec::new(),
        last_emit: Instant::now() - Duration::from_secs(1),
        done: Vec::new(),
        last_dst: None,
        not_undoable: false,
        disk_full: false,
    };
    std::thread::Builder::new()
        .name("fm-operation".into())
        .spawn(move || run(ctx, req))
        .expect("spawn operation thread");
    (control, rx)
}

struct Ctx {
    tx: async_channel::Sender<Event>,
    cancel: gio::Cancellable,
    paused: Arc<AtomicBool>,
    /// Sticky ReplaceAll / SkipAll choice.
    policy: Option<Resolution>,
    p: Progress,
    errors: Vec<OpError>,
    last_emit: Instant,
    done: Vec<(String, String)>,
    last_dst: Option<String>,
    not_undoable: bool,
    /// A write failed because the disk is full (checked by error kind, not text).
    disk_full: bool,
}

enum Resolved {
    Proceed { dst: gio::File, overwrite: bool },
    Skip,
    Cancel,
}

const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

impl Ctx {
    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn emit(&mut self, force: bool) {
        if force || self.last_emit.elapsed() >= Duration::from_millis(100) {
            self.last_emit = Instant::now();
            let _ = self.tx.send_blocking(Event::Progress(self.p.clone()));
        }
    }

    fn tick(&mut self) {
        while self.paused.load(Ordering::SeqCst) && !self.cancelled() {
            std::thread::sleep(Duration::from_millis(80));
        }
        self.emit(false);
    }

    fn error(&mut self, f: &gio::File, e: &gtk::glib::Error) {
        if e.matches(gio::IOErrorEnum::Cancelled) {
            return;
        }
        if e.matches(gio::IOErrorEnum::NoSpace) {
            self.disk_full = true;
        }
        self.errors.push(OpError { uri: f.uri().to_string(), message: e.message().to_string() });
    }

    fn error_msg(&mut self, f: &gio::File, msg: &str) {
        self.errors.push(OpError { uri: f.uri().to_string(), message: msg.to_string() });
    }

    fn ask(&mut self, src: &gio::File, dst: &gio::File) -> Resolution {
        let (rtx, rrx) = mpsc::channel();
        let ev = Event::Conflict { src: src.uri().to_string(), dst: dst.uri().to_string(), reply: rtx };
        if self.tx.send_blocking(ev).is_err() {
            return Resolution::Cancel;
        }
        loop {
            if self.cancelled() {
                return Resolution::Cancel;
            }
            match rrx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => return r,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Resolution::Cancel,
            }
        }
    }

    /// Decide what to do when `src` is about to be written to `dst`.
    fn resolve(&mut self, src: &gio::File, dst: gio::File) -> Resolved {
        let dst_ty = file_type(&dst);
        if dst_ty == gio::FileType::Unknown {
            return Resolved::Proceed { dst, overwrite: false };
        }
        let src_dir = file_type(src) == gio::FileType::Directory;
        let dst_dir = dst_ty == gio::FileType::Directory;
        if src_dir && dst_dir {
            self.not_undoable = true; // merging into existing data
            return Resolved::Proceed { dst, overwrite: false };
        }
        if src_dir != dst_dir {
            self.error_msg(&dst, "Cannot replace a folder with a file or a file with a folder");
            return Resolved::Skip;
        }
        let choice = match self.policy {
            Some(p) => p,
            None => self.ask(src, &dst),
        };
        match choice {
            Resolution::Replace => {
                self.not_undoable = true;
                Resolved::Proceed { dst, overwrite: true }
            }
            Resolution::ReplaceAll => {
                self.not_undoable = true;
                self.policy = Some(Resolution::ReplaceAll);
                Resolved::Proceed { dst, overwrite: true }
            }
            Resolution::Skip => Resolved::Skip,
            Resolution::SkipAll => {
                self.policy = Some(Resolution::SkipAll);
                Resolved::Skip
            }
            Resolution::Rename => {
                let parent = dst.parent().unwrap_or_else(|| dst.clone());
                let name = dst.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Resolved::Proceed { dst: unique_name(&parent, &name), overwrite: false }
            }
            Resolution::Cancel => {
                self.cancel.cancel();
                Resolved::Cancel
            }
        }
    }
}

fn file_type(f: &gio::File) -> gio::FileType {
    f.query_file_type(NOFOLLOW, gio::Cancellable::NONE)
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let (stem, ext) = name.split_at(i);
            if let Some(s) = stem.strip_suffix(".tar") {
                let cut = s.len();
                return (&name[..cut], &name[cut..]);
            }
            (stem, ext)
        }
        _ => (name, ""),
    }
}

/// "report.txt" → "report (copy).txt", "report (copy 2).txt", …
pub fn unique_name(dir: &gio::File, name: &str) -> gio::File {
    let (stem, ext) = split_ext(name);
    for n in 1u32.. {
        let candidate = if n == 1 { format!("{stem} (copy){ext}") } else { format!("{stem} (copy {n}){ext}") };
        let f = dir.child(&candidate);
        if file_type(&f) == gio::FileType::Unknown {
            return f;
        }
    }
    unreachable!()
}

fn scan(f: &gio::File, cancel: &gio::Cancellable, bytes: &mut u64, items: &mut u64) {
    if cancel.is_cancelled() {
        return;
    }
    let Ok(info) = f.query_info("standard::type,standard::size", NOFOLLOW, Some(cancel)) else { return };
    *items += 1;
    if info.file_type() == gio::FileType::Directory {
        if let Ok(en) = f.enumerate_children("standard::name", NOFOLLOW, Some(cancel)) {
            for child in en.flatten() {
                scan(&f.child(child.name()), cancel, bytes, items);
            }
        }
    } else {
        *bytes += info.size().max(0) as u64;
    }
}

fn run(mut ctx: Ctx, req: OpRequest) {
    let sources: Vec<gio::File> = req.sources.iter().map(|u| gio::File::for_uri(u)).collect();
    let mut failed = Vec::new();

    match req.kind {
        OpKind::Copy | OpKind::Delete => {
            let (mut b, mut n) = (0, 0);
            for s in &sources {
                scan(s, &ctx.cancel, &mut b, &mut n);
            }
            ctx.p.items_total = n;
            if req.kind == OpKind::Copy {
                ctx.p.total_bytes = b;
            }
        }
        _ => ctx.p.items_total = sources.len() as u64,
    }
    ctx.emit(true);

    let dest = req.dest.as_deref().map(gio::File::for_uri);
    for (uri, src) in req.sources.iter().zip(&sources) {
        if ctx.cancelled() {
            failed.push(uri.clone());
            continue;
        }
        ctx.p.current = src.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ctx.tick();
        ctx.last_dst = None;
        let ok = match (req.kind, &dest) {
            (OpKind::Copy, Some(d)) => transfer(&mut ctx, src, d, false),
            (OpKind::Move, Some(d)) => transfer(&mut ctx, src, d, true),
            (OpKind::Trash, _) => match src.trash(Some(&ctx.cancel.clone())) {
                Ok(()) => {
                    ctx.p.items_done += 1;
                    ctx.done.push((uri.clone(), String::new()));
                    true
                }
                Err(e) => {
                    ctx.error(src, &e);
                    false
                }
            },
            (OpKind::Delete, _) => delete_tree(&mut ctx, src, false),
            _ => false,
        };
        if !ok {
            failed.push(uri.clone());
        } else if let Some(d) = ctx.last_dst.take() {
            ctx.done.push((uri.clone(), d));
        }
        if ctx.disk_full {
            // Disk full: further work is pointless.
            ctx.cancel.cancel();
        }
    }

    let summary = Summary {
        cancelled: ctx.cancelled(),
        errors: std::mem::take(&mut ctx.errors),
        failed_sources: failed,
        items_done: ctx.p.items_done,
        done: std::mem::take(&mut ctx.done),
        undoable: !ctx.not_undoable,
    };
    ctx.emit(true);
    let _ = ctx.tx.send_blocking(Event::Finished(summary));
}

/// Copy or move one top-level `src` into `dest_dir`. Returns true if fully done.
fn transfer(ctx: &mut Ctx, src: &gio::File, dest_dir: &gio::File, mv: bool) -> bool {
    let Some(name) = src.basename() else { return false };
    let name = name.to_string_lossy().into_owned();
    let mut dst = dest_dir.child(&name);

    if src.equal(&dst) {
        if mv {
            ctx.p.items_done += 1;
            return true; // moving onto itself is a no-op
        }
        dst = unique_name(dest_dir, &name); // duplicate in place
    } else if file_type(src) == gio::FileType::Directory && dst.has_prefix(src) {
        ctx.error_msg(src, "Cannot copy or move a folder into itself");
        return false;
    }

    let (dst, overwrite) = match ctx.resolve(src, dst) {
        Resolved::Proceed { dst, overwrite } => (dst, overwrite),
        Resolved::Skip | Resolved::Cancel => return false,
    };
    ctx.last_dst = Some(dst.uri().to_string());

    if !mv {
        return copy_tree(ctx, src, &dst, overwrite);
    }

    let mut flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
    if overwrite {
        flags |= gio::FileCopyFlags::OVERWRITE;
    }
    let cancel = ctx.cancel.clone();
    match src.move_(&dst, flags, Some(&cancel), None) {
        Ok(()) => {
            ctx.p.items_done += 1;
            true
        }
        Err(e) if e.matches(gio::IOErrorEnum::WouldRecurse) || e.matches(gio::IOErrorEnum::WouldMerge) => {
            // Cross-device folder or merge: copy everything, then delete the
            // source only if the whole copy succeeded.
            if copy_tree(ctx, src, &dst, overwrite) && !ctx.cancelled() { delete_tree(ctx, src, false) } else { false }
        }
        Err(e) => {
            ctx.error(src, &e);
            false
        }
    }
}

fn copy_tree(ctx: &mut Ctx, src: &gio::File, dst: &gio::File, overwrite: bool) -> bool {
    if ctx.cancelled() {
        return false;
    }
    let cancel = ctx.cancel.clone();
    if file_type(src) == gio::FileType::Directory {
        match dst.make_directory(Some(&cancel)) {
            Ok(()) => {}
            Err(e) if e.matches(gio::IOErrorEnum::Exists) && file_type(dst) == gio::FileType::Directory => {}
            Err(e) => {
                ctx.error(dst, &e);
                return false;
            }
        }
        let mut ok = true;
        match src.enumerate_children("standard::name", NOFOLLOW, Some(&cancel)) {
            Ok(en) => {
                for child in en {
                    let Ok(child) = child else {
                        ok = false;
                        continue;
                    };
                    if ctx.cancelled() {
                        return false;
                    }
                    let (csrc, cdst) = (src.child(child.name()), dst.child(child.name()));
                    match ctx.resolve(&csrc, cdst) {
                        Resolved::Proceed { dst, overwrite } => ok &= copy_tree(ctx, &csrc, &dst, overwrite),
                        Resolved::Skip => ok = false,
                        Resolved::Cancel => return false,
                    }
                }
            }
            Err(e) => {
                ctx.error(src, &e);
                ok = false;
            }
        }
        // Best effort: permissions / timestamps of the folder itself.
        let _ = src.copy_attributes(
            dst,
            gio::FileCopyFlags::ALL_METADATA | gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
            Some(&cancel),
        );
        ctx.p.items_done += 1;
        ok
    } else {
        let mut flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
        if overwrite {
            flags |= gio::FileCopyFlags::OVERWRITE;
        }
        ctx.p.current = src.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let base = ctx.p.done_bytes;
        let size =
            src.query_info("standard::size", NOFOLLOW, Some(&cancel)).map(|i| i.size().max(0) as u64).unwrap_or(0);
        let res = {
            let mut cb = |done: i64, _total: i64| {
                ctx.p.done_bytes = base + done.max(0) as u64;
                ctx.tick();
            };
            src.copy(dst, flags, Some(&cancel), Some(&mut cb))
        };
        match res {
            Ok(()) => {
                ctx.p.done_bytes = base + size;
                ctx.p.items_done += 1;
                ctx.emit(false);
                true
            }
            Err(e) => {
                ctx.p.done_bytes = base + size;
                ctx.error(src, &e);
                false
            }
        }
    }
}

/// Let the owner read, write and enter the local folder `f` (it is about to
/// be deleted). Returns false if nothing changed or `f` isn't ours/local.
fn relax_folder(f: &gio::File) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Some(path) = f.path() else { return false };
    let Ok(md) = std::fs::symlink_metadata(&path) else { return false };
    if !md.is_dir() || md.uid() != nix::unistd::Uid::current().as_raw() || md.mode() & 0o700 == 0o700 {
        return false;
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(md.mode() | 0o700)).is_ok()
}

/// Delete `f` and everything in it. `inside` is true for anything below the
/// item the user chose: those folders are going away anyway, so a read-only
/// one may be unlocked. The folder the user chose is never touched this way
/// unless it is itself being deleted.
fn delete_tree(ctx: &mut Ctx, f: &gio::File, inside: bool) -> bool {
    if ctx.cancelled() {
        return false;
    }
    let cancel = ctx.cancel.clone();
    let mut ok = true;
    if file_type(f) == gio::FileType::Directory {
        let mut listing = f.enumerate_children("standard::name", NOFOLLOW, Some(&cancel));
        if matches!(&listing, Err(e) if e.matches(gio::IOErrorEnum::PermissionDenied)) && relax_folder(f) {
            listing = f.enumerate_children("standard::name", NOFOLLOW, Some(&cancel));
        }
        match listing {
            Ok(en) => {
                for child in en.flatten() {
                    ok &= delete_tree(ctx, &f.child(child.name()), true);
                }
            }
            Err(e) => {
                ctx.error(f, &e);
                return false;
            }
        }
        if !ok {
            return false; // would fail with "not empty"
        }
    }
    let mut res = f.delete(Some(&cancel));
    if inside
        && matches!(&res, Err(e) if e.matches(gio::IOErrorEnum::PermissionDenied))
        && f.parent().is_some_and(|p| relax_folder(&p))
    {
        res = f.delete(Some(&cancel));
    }
    match res {
        Ok(()) => {
            ctx.p.items_done += 1;
            ctx.emit(false);
            ok
        }
        Err(e) => {
            ctx.error(f, &e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    fn tmp(name: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn uri(p: &std::path::Path) -> String {
        gio::File::for_path(p).uri().to_string()
    }

    /// Run an op, answering every conflict with `answer`. Returns (summary, #conflicts).
    fn run_op(req: OpRequest, answer: Resolution) -> (Summary, usize) {
        let (_c, rx) = spawn(req);
        let mut conflicts = 0;
        loop {
            match rx.recv_blocking().unwrap() {
                Event::Conflict { reply, .. } => {
                    conflicts += 1;
                    reply.send(answer).unwrap();
                }
                Event::Finished(s) => return (s, conflicts),
                Event::Progress(_) => {}
            }
        }
    }

    fn setup(name: &str) -> (PathBuf, PathBuf) {
        let root = tmp(name);
        let (a, b) = (root.join("a"), root.join("b"));
        fs::create_dir_all(a.join("dir/sub")).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("dir/f.txt"), "new").unwrap();
        fs::write(a.join("dir/sub/g.txt"), "deep").unwrap();
        (a, b)
    }

    #[test]
    fn copy_tree_and_never_overwrite_on_skip() {
        let (a, b) = setup("skip");
        fs::create_dir_all(b.join("dir")).unwrap();
        fs::write(b.join("dir/f.txt"), "old").unwrap();
        let req = OpRequest { kind: OpKind::Copy, sources: vec![uri(&a.join("dir"))], dest: Some(uri(&b)) };
        let (s, n) = run_op(req, Resolution::Skip);
        assert_eq!(n, 1);
        assert_eq!(fs::read_to_string(b.join("dir/f.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(b.join("dir/sub/g.txt")).unwrap(), "deep");
        assert_eq!(s.failed_sources.len(), 1); // incomplete → retryable
    }

    #[test]
    fn replace_and_rename() {
        let (a, b) = setup("replace");
        fs::create_dir_all(b.join("dir")).unwrap();
        fs::write(b.join("dir/f.txt"), "old").unwrap();
        let req = OpRequest { kind: OpKind::Copy, sources: vec![uri(&a.join("dir"))], dest: Some(uri(&b)) };
        run_op(req.clone(), Resolution::Rename);
        assert_eq!(fs::read_to_string(b.join("dir/f.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(b.join("dir/f (copy).txt")).unwrap(), "new");
        run_op(req, Resolution::Replace);
        assert_eq!(fs::read_to_string(b.join("dir/f.txt")).unwrap(), "new");
    }

    #[test]
    fn move_and_skip_keeps_source() {
        let (a, b) = setup("move");
        fs::create_dir_all(b.join("dir")).unwrap();
        fs::write(b.join("dir/f.txt"), "old").unwrap();
        let req = OpRequest { kind: OpKind::Move, sources: vec![uri(&a.join("dir"))], dest: Some(uri(&b)) };
        let (s, _) = run_op(req, Resolution::Skip);
        // Skipped file must not be lost: the source folder still has it.
        assert!(a.join("dir/f.txt").exists());
        assert_eq!(fs::read_to_string(b.join("dir/f.txt")).unwrap(), "old");
        assert_eq!(s.failed_sources.len(), 1);
    }

    #[test]
    fn copy_in_place_duplicates_and_folder_into_itself_fails() {
        let (a, _b) = setup("inplace");
        fs::write(a.join("x.tar.gz"), "z").unwrap();
        let req = OpRequest { kind: OpKind::Copy, sources: vec![uri(&a.join("x.tar.gz"))], dest: Some(uri(&a)) };
        let (_, n) = run_op(req, Resolution::Cancel);
        assert_eq!(n, 0);
        assert!(a.join("x (copy).tar.gz").exists());
        let req =
            OpRequest { kind: OpKind::Copy, sources: vec![uri(&a.join("dir"))], dest: Some(uri(&a.join("dir/sub"))) };
        let (s, _) = run_op(req, Resolution::Skip);
        assert_eq!(s.errors.len(), 1);
    }

    #[test]
    fn summary_records_final_destinations_and_undoability() {
        let (a, b) = setup("summary");
        let req = OpRequest { kind: OpKind::Copy, sources: vec![uri(&a.join("dir"))], dest: Some(uri(&b)) };
        let (s, _) = run_op(req.clone(), Resolution::Skip);
        assert!(s.undoable);
        assert_eq!(s.done, [(uri(&a.join("dir")), uri(&b.join("dir")))]);
        // Second copy merges into the existing folder and conflicts: renamed copy, but a merge isn't undoable.
        let (s, _) = run_op(req, Resolution::Rename);
        assert!(!s.undoable);
    }

    #[test]
    fn delete_unlocks_read_only_folders_inside_the_tree_only() {
        use std::os::unix::fs::PermissionsExt;
        let (a, _b) = setup("delete-ro");
        fs::set_permissions(a.join("dir/sub"), fs::Permissions::from_mode(0o555)).unwrap();
        let req = OpRequest { kind: OpKind::Delete, sources: vec![uri(&a.join("dir"))], dest: None };
        let (s, _) = run_op(req, Resolution::Skip);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert!(!a.join("dir").exists());
        // A single file in a read-only folder the user did not choose stays protected.
        fs::create_dir_all(a.join("ro")).unwrap();
        fs::write(a.join("ro/keep"), "k").unwrap();
        fs::set_permissions(a.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
        let req = OpRequest { kind: OpKind::Delete, sources: vec![uri(&a.join("ro/keep"))], dest: None };
        let (s, _) = run_op(req, Resolution::Skip);
        assert_eq!(s.errors.len(), 1);
        assert!(a.join("ro/keep").exists());
        assert_eq!(fs::metadata(a.join("ro")).unwrap().permissions().mode() & 0o777, 0o555);
        fs::set_permissions(a.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn delete_tree_removes_everything() {
        let (a, _b) = setup("delete");
        let req = OpRequest { kind: OpKind::Delete, sources: vec![uri(&a.join("dir"))], dest: None };
        let (s, _) = run_op(req, Resolution::Skip);
        assert!(s.errors.is_empty());
        assert!(!a.join("dir").exists());
    }
}
