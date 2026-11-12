//! Permission and ownership helpers. Never escalates privileges: operations
//! run as the current user and report the OS error if refused.

use nix::unistd::{Gid, Group, Uid, User};
use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub const SETUID: u32 = 0o4000;
pub const SETGID: u32 = 0o2000;
pub const STICKY: u32 = 0o1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only the item itself.
    ItemOnly,
    /// The item and everything inside it.
    All,
    /// The item (if a folder) and folders inside it.
    FoldersOnly,
    /// Only files inside the item (and the item itself if it is a file).
    FilesOnly,
}

#[cfg(test)]
pub fn mode_of(p: &Path) -> io::Result<u32> {
    Ok(fs::symlink_metadata(p)?.mode() & 0o7777)
}

pub fn symbolic(mode: u32) -> String {
    let bit = |m: u32, c: char| if mode & m != 0 { c } else { '-' };
    let mut s = String::new();
    for (shift, special, sc, sl) in [(6, SETUID, 's', 'S'), (3, SETGID, 's', 'S'), (0, STICKY, 't', 'T')] {
        s.push(bit(0o4 << shift, 'r'));
        s.push(bit(0o2 << shift, 'w'));
        let x = mode & (0o1 << shift) != 0;
        s.push(match (mode & special != 0, x) {
            (true, true) => sc,
            (true, false) => sl,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

pub fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(p, fs::Permissions::from_mode(mode & 0o7777))
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub changed: u64,
    pub errors: Vec<(PathBuf, String)>,
}

/// Mode for a folder when `mode` is applied recursively: wherever a class may
/// read, it also gets search (`x`), like `chmod -R u=rwX`. Without this, a
/// mode such as 644 would lock everyone out of the folder it is applied to.
pub fn folder_mode(mode: u32) -> u32 {
    mode | ((mode & 0o444) >> 2)
}

/// Apply `mode` to `root` and (per `scope`) to its descendants. Symlinks are
/// never followed or changed. With a recursive scope, folders get
/// [`folder_mode`] and are changed after everything inside them, so a mode
/// that removes access never stops the walk.
pub fn apply(root: &Path, mode: u32, scope: Scope, cancel: &AtomicBool) -> ApplyReport {
    let mut rep = ApplyReport::default();
    let do_one = |p: &Path, mode: u32, rep: &mut ApplyReport| match set_mode(p, mode) {
        Ok(()) => rep.changed += 1,
        Err(e) => rep.errors.push((p.to_path_buf(), e.to_string())),
    };
    let Ok(md) = fs::symlink_metadata(root) else {
        rep.errors.push((root.to_path_buf(), "cannot read".into()));
        return rep;
    };
    if md.file_type().is_symlink() {
        return rep;
    }
    if scope == Scope::ItemOnly || !md.is_dir() {
        do_one(root, mode, &mut rep);
        return rep;
    }
    let dirs_change = matches!(scope, Scope::All | Scope::FoldersOnly);
    let files_change = matches!(scope, Scope::All | Scope::FilesOnly);
    // Folders in discovery order; changed last, deepest first.
    let mut folders = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            rep.errors.push((dir, "cannot read folder".into()));
            continue;
        };
        for e in rd.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return rep;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let p = e.path();
            if ft.is_dir() {
                folders.push(p.clone());
                stack.push(p);
            } else if files_change {
                do_one(&p, mode, &mut rep);
            }
        }
    }
    if dirs_change {
        for d in folders.iter().rev() {
            do_one(d, folder_mode(mode), &mut rep);
        }
    }
    rep
}

pub fn owner_name(uid: u32) -> String {
    User::from_uid(Uid::from_raw(uid)).ok().flatten().map(|u| u.name).unwrap_or_else(|| uid.to_string())
}

pub fn group_name(gid: u32) -> String {
    Group::from_gid(Gid::from_raw(gid)).ok().flatten().map(|g| g.name).unwrap_or_else(|| gid.to_string())
}

/// Groups the current user may assign files to without privileges.
pub fn assignable_groups() -> Vec<(u32, String)> {
    let mut gids: Vec<Gid> = nix::unistd::getgroups().unwrap_or_default();
    gids.push(nix::unistd::getegid());
    gids.sort_by_key(|g| g.as_raw());
    gids.dedup();
    gids.into_iter().map(|g| (g.as_raw(), group_name(g.as_raw()))).collect()
}

pub fn set_group(p: &Path, gid: u32) -> io::Result<()> {
    nix::unistd::chown(p, None, Some(Gid::from_raw(gid))).map_err(io::Error::from)
}

pub fn can_chmod(md: &fs::Metadata) -> bool {
    Uid::current().is_root() || md.uid() == Uid::current().as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_forms() {
        assert_eq!(symbolic(0o755), "rwxr-xr-x");
        assert_eq!(symbolic(0o4755), "rwsr-xr-x");
        assert_eq!(symbolic(0o1777), "rwxrwxrwt");
        assert_eq!(symbolic(0o2640), "rw-r-S---");
    }

    #[test]
    fn recursive_scopes_do_not_follow_symlinks() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/perm");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("d/inner")).unwrap();
        fs::write(root.join("d/f"), "x").unwrap();
        fs::write(root.join("outside"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("d/link")).unwrap();
        set_mode(&root.join("outside"), 0o600).unwrap();
        let c = AtomicBool::new(false);
        let rep = apply(&root.join("d"), 0o750, Scope::All, &c);
        assert!(rep.errors.is_empty());
        assert_eq!(mode_of(&root.join("d/f")).unwrap(), 0o750);
        assert_eq!(mode_of(&root.join("d/inner")).unwrap(), 0o750);
        assert_eq!(mode_of(&root.join("outside")).unwrap(), 0o600, "symlink target must be untouched");
        apply(&root.join("d"), 0o640, Scope::FilesOnly, &c);
        assert_eq!(mode_of(&root.join("d/f")).unwrap(), 0o640);
        assert_eq!(mode_of(&root.join("d")).unwrap(), 0o750);
    }

    #[test]
    fn recursive_mode_without_search_bit_does_not_lock_folders() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/perm-order");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("d/inner")).unwrap();
        fs::write(root.join("d/inner/f"), "x").unwrap();
        assert_eq!(folder_mode(0o644), 0o755);
        assert_eq!(folder_mode(0o600), 0o700);
        assert_eq!(folder_mode(0o000), 0o000);
        let rep = apply(&root.join("d"), 0o644, Scope::All, &AtomicBool::new(false));
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(mode_of(&root.join("d/inner/f")).unwrap(), 0o644);
        assert_eq!(mode_of(&root.join("d/inner")).unwrap(), 0o755);
        assert_eq!(mode_of(&root.join("d")).unwrap(), 0o755);
    }
}
