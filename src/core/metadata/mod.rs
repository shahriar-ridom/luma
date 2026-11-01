//! Recursive size statistics, checksums and file metadata.

pub mod advanced;

use gtk::glib::{Checksum, ChecksumType};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirStats {
    /// Sum of apparent file sizes (hard links counted once).
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

/// Walk `root` without following symlinks. Returns what was counted so far if
/// cancelled.
pub fn dir_stats(root: &Path, cancel: &AtomicBool) -> DirStats {
    let mut st = DirStats::default();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return st;
            }
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                st.dirs += 1;
                stack.push(e.path());
            } else {
                st.files += 1;
                if md.nlink() > 1 && !seen.insert((md.dev(), md.ino())) {
                    continue;
                }
                st.bytes += md.len();
            }
        }
    }
    st
}

/// Size of each immediate child of `dir` (folders counted recursively),
/// reported one by one so a UI can fill in as results arrive.
pub fn usage_children(dir: &Path, cancel: &AtomicBool, mut emit: impl FnMut(String, bool, u64)) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(md) = e.metadata() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if md.is_dir() {
            emit(name, true, dir_stats(&e.path(), cancel).bytes);
        } else {
            emit(name, false, md.len());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algo {
    pub const ALL: [Algo; 4] = [Algo::Sha256, Algo::Sha512, Algo::Sha1, Algo::Md5];
    pub fn label(self) -> &'static str {
        match self {
            Algo::Md5 => "MD5",
            Algo::Sha1 => "SHA-1",
            Algo::Sha256 => "SHA-256",
            Algo::Sha512 => "SHA-512",
        }
    }
    fn ty(self) -> ChecksumType {
        match self {
            Algo::Md5 => ChecksumType::Md5,
            Algo::Sha1 => ChecksumType::Sha1,
            Algo::Sha256 => ChecksumType::Sha256,
            Algo::Sha512 => ChecksumType::Sha512,
        }
    }
}

/// Stream the file through the hash (never loads it into memory).
/// `Ok(None)` means cancelled.
pub fn checksum(path: &Path, algo: Algo, cancel: &AtomicBool) -> io::Result<Option<String>> {
    checksum_prefix(path, algo, u64::MAX, cancel)
}

/// Like [`checksum`] but only over the first `max_bytes`.
pub fn checksum_prefix(path: &Path, algo: Algo, max_bytes: u64, cancel: &AtomicBool) -> io::Result<Option<String>> {
    let mut f = fs::File::open(path)?.take(max_bytes);
    let mut sum = Checksum::new(algo.ty()).ok_or_else(|| io::Error::other("unsupported checksum"))?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sum.update(&buf[..n]);
    }
    Ok(sum.string().map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn usage_children_sizes_folders_recursively() {
        let d = tmp("usage");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/x"), vec![0u8; 300]).unwrap();
        fs::write(d.join("f"), vec![0u8; 50]).unwrap();
        let mut got = Vec::new();
        usage_children(&d, &AtomicBool::new(false), |n, dir, b| got.push((n, dir, b)));
        got.sort();
        assert_eq!(got, [("a".to_string(), true, 300), ("f".to_string(), false, 50)]);
    }

    #[test]
    fn sha256_known_vector() {
        let d = tmp("sum");
        fs::write(d.join("abc"), "abc").unwrap();
        let c = AtomicBool::new(false);
        assert_eq!(
            checksum(&d.join("abc"), Algo::Sha256, &c).unwrap().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(checksum(&d.join("abc"), Algo::Md5, &c).unwrap().unwrap(), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn stats_count_hardlinks_once_and_skip_symlink_targets() {
        let d = tmp("stats");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/f1"), vec![0u8; 100]).unwrap();
        fs::hard_link(d.join("a/f1"), d.join("a/b/f1link")).unwrap();
        std::os::unix::fs::symlink(&d, d.join("a/loop")).unwrap();
        let s = dir_stats(&d.join("a"), &AtomicBool::new(false));
        assert_eq!(s.bytes, 100 + fs::symlink_metadata(d.join("a/loop")).unwrap().len());
        assert_eq!(s.dirs, 1);
        assert_eq!(s.files, 3);
    }
}
