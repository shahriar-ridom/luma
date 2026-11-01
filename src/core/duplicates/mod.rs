//! Duplicate file detection: size → first-4KB hash → full hash. Hard links to
//! the same inode are one file, never a duplicate pair.

use std::{
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use crate::core::metadata::{Algo, checksum, checksum_prefix};

#[derive(Clone, Debug)]
pub struct DupGroup {
    pub size: u64,
    pub files: Vec<PathBuf>,
}

pub enum DupEvent {
    /// Stage name and (done, total) within it.
    Progress(&'static str, u64, u64),
    Group(DupGroup),
    Done {
        groups: usize,
        wasted: u64,
    },
}

pub fn run(root: &Path, hidden: bool, min_size: u64, cancel: &AtomicBool, mut emit: impl FnMut(DupEvent)) {
    // 1. Group by size.
    let mut by_size: HashMap<u64, Vec<(PathBuf, (u64, u64))>> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0u64;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if !hidden && e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else if md.is_file() && md.len() >= min_size.max(1) {
                scanned += 1;
                by_size.entry(md.len()).or_default().push((e.path(), (md.dev(), md.ino())));
                if scanned.is_multiple_of(500) {
                    emit(DupEvent::Progress("Scanning", scanned, 0));
                }
            }
        }
    }
    let mut cands: Vec<(u64, Vec<PathBuf>)> = by_size
        .into_iter()
        .filter_map(|(size, mut v)| {
            v.sort_by(|a, b| a.0.cmp(&b.0));
            let mut seen = std::collections::HashSet::new();
            v.retain(|(_, id)| seen.insert(*id)); // hard links count once
            (v.len() > 1).then(|| (size, v.into_iter().map(|(p, _)| p).collect()))
        })
        .collect();
    cands.sort_by_key(|(s, _)| std::cmp::Reverse(*s));

    let total: u64 = cands.iter().map(|(_, v)| v.len() as u64).sum();
    let (mut done, mut groups, mut wasted) = (0u64, 0usize, 0u64);
    for (size, files) in cands {
        // 2. Cheap prefix hash, then 3. full hash for survivors.
        let mut by_prefix: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for f in files {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if let Ok(Some(h)) = checksum_prefix(&f, Algo::Sha1, 4096, cancel) {
                by_prefix.entry(h).or_default().push(f);
            }
            done += 1;
            emit(DupEvent::Progress("Comparing", done, total));
        }
        for (_, same_prefix) in by_prefix.into_iter().filter(|(_, v)| v.len() > 1) {
            let mut by_full: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for f in same_prefix {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Ok(Some(h)) = checksum(&f, Algo::Sha256, cancel) {
                    by_full.entry(h).or_default().push(f);
                }
            }
            for (_, mut g) in by_full.into_iter().filter(|(_, v)| v.len() > 1) {
                g.sort();
                groups += 1;
                wasted += size * (g.len() as u64 - 1);
                emit(DupEvent::Group(DupGroup { size, files: g }));
            }
        }
    }
    emit(DupEvent::Done { groups, wasted });
}

pub fn spawn(
    root: PathBuf,
    hidden: bool,
    min_size: u64,
) -> (std::sync::Arc<AtomicBool>, async_channel::Receiver<DupEvent>) {
    let (tx, rx) = async_channel::unbounded();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("fm-duplicates".into())
        .spawn(move || {
            run(&root, hidden, min_size, &c, |ev| {
                let _ = tx.send_blocking(ev);
            })
        })
        .expect("spawn duplicates thread");
    (cancel, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_true_duplicates_only() {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/dups");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.txt"), "same content").unwrap();
        fs::write(d.join("sub/a-copy.txt"), "same content").unwrap();
        fs::write(d.join("diff.txt"), "same_content").unwrap(); // same size, different bytes
        fs::write(d.join("empty1"), "").unwrap();
        fs::write(d.join("empty2"), "").unwrap();
        fs::hard_link(d.join("a.txt"), d.join("hardlink.txt")).unwrap();
        let mut head = vec![7u8; 4096];
        let (mut x, mut y) = (head.clone(), head.clone());
        x.extend_from_slice(b"X");
        y.extend_from_slice(b"Y"); // identical first 4KB, differ afterwards
        head.clear();
        fs::write(d.join("big1"), &x).unwrap();
        fs::write(d.join("big2"), &y).unwrap();

        let mut groups = Vec::new();
        let mut wasted = 0;
        run(&d, false, 1, &AtomicBool::new(false), |ev| match ev {
            DupEvent::Group(g) => groups.push(g),
            DupEvent::Done { wasted: w, .. } => wasted = w,
            _ => {}
        });
        assert_eq!(groups.len(), 1, "{groups:?}");
        let names: Vec<_> =
            groups[0].files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "hard link must not count as a third copy: {names:?}");
        assert!(names.contains(&"a-copy.txt".to_string()));
        assert_eq!(wasted, 12);
    }
}
