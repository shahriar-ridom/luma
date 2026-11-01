//! Compare two files: byte-identical check, line diff for text, first
//! differing offset for binaries.

use similar::{ChangeTag, TextDiff};
use std::{
    fs,
    io::{self, Read},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_TEXT: u64 = 2 * 1024 * 1024;
const MAX_DIFF_LINES: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Context,
    Added,
    Removed,
    /// Hunk header such as `@@ -3,4 +3,5 @@`.
    Hunk,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: Kind,
    pub text: String,
}

#[derive(Debug)]
pub enum Outcome {
    Identical,
    Text { lines: Vec<DiffLine>, added: usize, removed: usize, truncated: bool },
    Binary { first_difference: Option<u64> },
}

fn is_text(bytes: &[u8]) -> bool {
    !bytes.iter().take(8192).any(|b| *b == 0) && std::str::from_utf8(bytes).is_ok()
}

fn first_difference(a: &Path, b: &Path, cancel: &AtomicBool) -> io::Result<Option<u64>> {
    let (mut fa, mut fb) = (fs::File::open(a)?, fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    let mut off = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Some(off));
        }
        let (na, nb) = (read_full(&mut fa, &mut ba)?, read_full(&mut fb, &mut bb)?);
        let n = na.min(nb);
        if let Some(i) = (0..n).find(|&i| ba[i] != bb[i]) {
            return Ok(Some(off + i as u64));
        }
        if na != nb {
            return Ok(Some(off + n as u64));
        }
        if na == 0 {
            return Ok(None);
        }
        off += na as u64;
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

pub fn compare(a: &Path, b: &Path, cancel: &AtomicBool) -> io::Result<Outcome> {
    let (ma, mb) = (fs::metadata(a)?, fs::metadata(b)?);
    let first = first_difference(a, b, cancel)?;
    if first.is_none() {
        return Ok(Outcome::Identical);
    }
    if ma.len() <= MAX_TEXT && mb.len() <= MAX_TEXT {
        let (ta, tb) = (fs::read(a)?, fs::read(b)?);
        if is_text(&ta) && is_text(&tb) {
            let (sa, sb) = (String::from_utf8_lossy(&ta), String::from_utf8_lossy(&tb));
            let diff = TextDiff::from_lines(sa.as_ref(), sb.as_ref());
            let (mut lines, mut added, mut removed, mut truncated) = (Vec::new(), 0, 0, false);
            for group in diff.grouped_ops(3) {
                let (first_op, last_op) = (group.first().unwrap(), group.last().unwrap());
                let (o, n) = (
                    first_op.old_range().start..last_op.old_range().end,
                    first_op.new_range().start..last_op.new_range().end,
                );
                lines.push(DiffLine {
                    kind: Kind::Hunk,
                    text: format!("@@ -{},{} +{},{} @@", o.start + 1, o.len(), n.start + 1, n.len()),
                });
                for op in &group {
                    for ch in diff.iter_changes(op) {
                        let kind = match ch.tag() {
                            ChangeTag::Equal => Kind::Context,
                            ChangeTag::Insert => {
                                added += 1;
                                Kind::Added
                            }
                            ChangeTag::Delete => {
                                removed += 1;
                                Kind::Removed
                            }
                        };
                        if lines.len() < MAX_DIFF_LINES {
                            lines.push(DiffLine { kind, text: ch.value().trim_end_matches('\n').to_string() });
                        } else {
                            truncated = true;
                        }
                    }
                }
            }
            return Ok(Outcome::Text { lines, added, removed, truncated });
        }
    }
    Ok(Outcome::Binary { first_difference: first })
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
    fn identical_text_binary() {
        let d = tmp("compare");
        let c = AtomicBool::new(false);
        fs::write(d.join("a"), "one\ntwo\nthree\nfour\n").unwrap();
        fs::write(d.join("same"), "one\ntwo\nthree\nfour\n").unwrap();
        fs::write(d.join("b"), "one\n2\nthree\nfour\nfive\n").unwrap();
        assert!(matches!(compare(&d.join("a"), &d.join("same"), &c).unwrap(), Outcome::Identical));
        match compare(&d.join("a"), &d.join("b"), &c).unwrap() {
            Outcome::Text { lines, added, removed, .. } => {
                assert_eq!((added, removed), (2, 1));
                assert!(lines.iter().any(|l| l.kind == Kind::Removed && l.text == "two"));
                assert!(lines.iter().any(|l| l.kind == Kind::Added && l.text == "five"));
                assert_eq!(lines[0].kind, Kind::Hunk);
            }
            o => panic!("{o:?}"),
        }
        fs::write(d.join("x.bin"), [1u8, 2, 3, 0, 5]).unwrap();
        fs::write(d.join("y.bin"), [1u8, 2, 3, 0, 9]).unwrap();
        match compare(&d.join("x.bin"), &d.join("y.bin"), &c).unwrap() {
            Outcome::Binary { first_difference } => assert_eq!(first_difference, Some(4)),
            o => panic!("{o:?}"),
        }
    }
}
