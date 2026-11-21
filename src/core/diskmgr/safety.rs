//! Rules every deletion must pass. Cleanup never removes a path unless it is
//! inside an allowlisted root, old enough, on the same device and not reached
//! through a symlink.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::core::{filesystem::remove_tree, metadata::dir_stats};

/// Nothing modified within this window is ever touched, whatever the config says.
pub const MIN_AGE_FLOOR: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, PartialEq, Eq)]
pub enum Denied {
    NotAbsolute,
    ParentDirRef,
    OutsideRoots,
    IsRoot,
    Escapes,
    CrossDevice,
    TooNew,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "path is not absolute",
            Self::ParentDirRef => "path contains '..'",
            Self::OutsideRoots => "path is outside the allowed folders",
            Self::IsRoot => "refusing to remove an allowed folder itself",
            Self::Escapes => "path leaves the allowed folder through a symlink",
            Self::CrossDevice => "path is on a different filesystem",
            Self::TooNew => "modified too recently",
        })
    }
}

#[derive(Clone, Debug)]
pub struct PathPolicy {
    pub roots: Vec<PathBuf>,
    pub min_age: Duration,
}

impl PathPolicy {
    pub fn new(roots: Vec<PathBuf>, min_age: Duration) -> Self {
        Self { roots, min_age: min_age.max(MIN_AGE_FLOOR) }
    }

    /// Where `path` may be removed. Returns the matching (canonical) root.
    pub fn check_location(&self, path: &Path) -> Result<PathBuf, Denied> {
        if !path.is_absolute() {
            return Err(Denied::NotAbsolute);
        }
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(Denied::ParentDirRef);
        }
        let root = self.roots.iter().find(|r| path.starts_with(r)).ok_or(Denied::OutsideRoots)?;
        if path == root {
            return Err(Denied::IsRoot);
        }
        // The path itself may be a symlink (we remove the link, not the
        // target), but the folder holding it must really be inside the root.
        let canon_root = root.canonicalize().map_err(|_| Denied::Escapes)?;
        let parent = path.parent().ok_or(Denied::OutsideRoots)?;
        let canon_parent = parent.canonicalize().map_err(|_| Denied::Escapes)?;
        if !canon_parent.starts_with(&canon_root) {
            return Err(Denied::Escapes);
        }
        Ok(canon_root)
    }

    /// Old enough? The newer of modification and access time counts, so a
    /// file somebody still reads is never treated as stale.
    pub fn check_age(&self, md: &fs::Metadata, now: SystemTime) -> Result<(), Denied> {
        let newest = [md.modified().ok(), md.accessed().ok()].into_iter().flatten().max().unwrap_or(now);
        match now.duration_since(newest) {
            Ok(age) if age >= self.min_age => Ok(()),
            Ok(_) | Err(_) => Err(Denied::TooNew),
        }
    }

    /// Location and same device as its root, without the age rule. For callers
    /// that judge age another way (trashed files keep their old mtime).
    pub fn check_placement(&self, path: &Path) -> Result<(), Denied> {
        let root = self.check_location(path)?;
        let md = fs::symlink_metadata(path).map_err(|_| Denied::OutsideRoots)?;
        let root_md = fs::metadata(&root).map_err(|_| Denied::Escapes)?;
        if md.dev() != root_md.dev() {
            return Err(Denied::CrossDevice);
        }
        Ok(())
    }

    /// Full check for one path: placement and age.
    pub fn check(&self, path: &Path, now: SystemTime) -> Result<(), Denied> {
        self.check_placement(path)?;
        let md = fs::symlink_metadata(path).map_err(|_| Denied::OutsideRoots)?;
        self.check_age(&md, now)
    }

    /// Remove `path` if it passes [`Self::check`]. Returns the bytes freed.
    pub fn remove_checked(&self, path: &Path, now: SystemTime) -> io::Result<u64> {
        self.check(path, now).map_err(denied)?;
        remove_counted(path)
    }

    /// Like [`Self::remove_checked`] but the caller has already judged age.
    pub fn remove_placed(&self, path: &Path) -> io::Result<u64> {
        self.check_placement(path).map_err(denied)?;
        remove_counted(path)
    }
}

fn denied(d: Denied) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, d.to_string())
}

fn remove_counted(path: &Path) -> io::Result<u64> {
    let md = fs::symlink_metadata(path)?;
    let bytes = if md.is_dir() { dir_stats(path, &std::sync::atomic::AtomicBool::new(false)).bytes } else { md.len() };
    remove_tree(path)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, time::Duration};

    fn tmp(name: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-safety").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn age(p: &Path, secs_ago: u64) {
        let t = SystemTime::now() - Duration::from_secs(secs_ago);
        fs::File::open(p).unwrap().set_times(fs::FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
    }

    const DAY: u64 = 24 * 3600;

    #[test]
    fn age_floor_cannot_be_lowered() {
        let p = PathPolicy::new(vec![], Duration::from_secs(1));
        assert_eq!(p.min_age, MIN_AGE_FLOOR);
    }

    #[test]
    fn rejects_bad_locations() {
        let root = tmp("loc");
        let p = PathPolicy::new(vec![root.clone()], Duration::ZERO);
        assert_eq!(p.check_location(Path::new("relative/x")), Err(Denied::NotAbsolute));
        assert_eq!(p.check_location(&root.join("../x")), Err(Denied::ParentDirRef));
        assert_eq!(p.check_location(Path::new("/etc/passwd")), Err(Denied::OutsideRoots));
        assert_eq!(p.check_location(&root), Err(Denied::IsRoot));
        // Prefix match must be per component, not per string.
        let sibling = PathBuf::from(format!("{}-evil/x", root.display()));
        assert_eq!(p.check_location(&sibling), Err(Denied::OutsideRoots));
        fs::write(root.join("ok"), "x").unwrap();
        assert!(p.check_location(&root.join("ok")).is_ok());
    }

    #[test]
    fn symlinked_parent_escape_is_denied() {
        let base = tmp("escape");
        let root = base.join("cache");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("victim"), "keep").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let p = PathPolicy::new(vec![root.clone()], Duration::ZERO);
        assert_eq!(p.check_location(&root.join("link/victim")), Err(Denied::Escapes));
        assert!(p.remove_checked(&root.join("link/victim"), SystemTime::now()).is_err());
        assert!(outside.join("victim").exists());
    }

    #[test]
    fn removing_a_symlink_keeps_its_target() {
        let base = tmp("symrm");
        let root = base.join("cache");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "data").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let p = PathPolicy::new(vec![root.clone()], Duration::ZERO);
        let old = SystemTime::now() + Duration::from_secs(10 * DAY);
        p.remove_checked(&root.join("link"), old).unwrap();
        assert!(!root.join("link").exists());
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn age_filter_and_byte_accounting() {
        let root = tmp("age");
        let p = PathPolicy::new(vec![root.clone()], Duration::from_secs(7 * DAY));
        let (old, new) = (root.join("old"), root.join("new"));
        fs::write(&old, vec![0u8; 1000]).unwrap();
        fs::write(&new, vec![0u8; 1000]).unwrap();
        age(&old, 30 * DAY);
        age(&new, 3600);
        let now = SystemTime::now();
        assert_eq!(p.check(&new, now), Err(Denied::TooNew));
        assert_eq!(p.remove_checked(&old, now).unwrap(), 1000);
        assert!(!old.exists());
        assert!(p.remove_checked(&new, now).is_err());
        assert!(new.exists());
    }

    #[test]
    fn removes_directory_trees_and_counts_them() {
        let root = tmp("tree");
        let d = root.join("dir");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a"), vec![0u8; 100]).unwrap();
        fs::write(d.join("sub/b"), vec![0u8; 200]).unwrap();
        age(&d, 10 * DAY);
        let p = PathPolicy::new(vec![root], Duration::ZERO);
        assert_eq!(p.remove_checked(&d, SystemTime::now()).unwrap(), 300);
        assert!(!d.exists());
    }
}
