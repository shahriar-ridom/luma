//! Which optional helper programs exist. The UI hides what it cannot run.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Everything the disk manager may call. Resolved to an absolute path so what
/// runs is what was detected.
pub const KNOWN: &[&str] = &[
    "smartctl",
    "pkexec",
    "systemctl",
    "journalctl",
    "btrfs",
    "fsck",
    "xfs_repair",
    "lsblk",
    "flatpak",
    "paccache",
    "pacman",
    "apt-get",
    "dnf",
    "zypper",
    "apk",
    "xbps-remove",
    "find",
];

/// Admin tools usually live in sbin, which is often not on a user's PATH.
const EXTRA_DIRS: &[&str] = &["/usr/sbin", "/sbin", "/usr/local/sbin"];

#[derive(Clone, Debug, Default)]
pub struct ToolSet {
    found: BTreeMap<String, PathBuf>,
}

impl ToolSet {
    pub fn detect() -> Self {
        let mut dirs: Vec<PathBuf> =
            std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        dirs.extend(EXTRA_DIRS.iter().map(PathBuf::from));
        Self::detect_in(&dirs, KNOWN)
    }

    pub fn detect_in(dirs: &[PathBuf], names: &[&str]) -> Self {
        let found = names.iter().filter_map(|n| find_in(dirs, n).map(|p| ((*n).to_string(), p))).collect();
        Self { found }
    }

    pub fn has(&self, name: &str) -> bool {
        self.found.contains_key(name)
    }

    pub fn path(&self, name: &str) -> Option<&Path> {
        self.found.get(name).map(PathBuf::as_path)
    }
}

fn find_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn tmp(name: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-tools").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn finds_only_executables() {
        let d = tmp("exec");
        fs::write(d.join("smartctl"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("smartctl"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(d.join("btrfs"), "not executable").unwrap();
        fs::set_permissions(d.join("btrfs"), fs::Permissions::from_mode(0o644)).unwrap();
        let t = ToolSet::detect_in(std::slice::from_ref(&d), &["smartctl", "btrfs", "fsck"]);
        assert!(t.has("smartctl"));
        assert_eq!(t.path("smartctl"), Some(d.join("smartctl").as_path()));
        assert!(!t.has("btrfs"));
    }
}
