//! Git integration through the `git` command line (no libgit2 dependency).
//!
//! Status uses `status --porcelain=v2 --branch -z` with optional locks
//! disabled so it never interferes with the user's own git commands. Network
//! operations never prompt: they fail fast with git's message instead.

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Ordered by importance when a folder aggregates its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileState {
    Ignored,
    Untracked,
    Added,
    Renamed,
    Deleted,
    Modified,
    Conflicted,
}

impl FileState {
    pub fn letter(self) -> &'static str {
        match self {
            FileState::Ignored => "I",
            FileState::Untracked => "U",
            FileState::Added => "A",
            FileState::Renamed => "R",
            FileState::Deleted => "D",
            FileState::Modified => "M",
            FileState::Conflicted => "!",
        }
    }
    pub fn css(self) -> &'static str {
        match self {
            FileState::Ignored => "fm-git-ignored",
            FileState::Untracked => "fm-git-untracked",
            FileState::Added | FileState::Renamed => "fm-git-added",
            FileState::Deleted | FileState::Conflicted => "fm-git-bad",
            FileState::Modified => "fm-git-modified",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            FileState::Ignored => "Ignored",
            FileState::Untracked => "Untracked",
            FileState::Added => "Added",
            FileState::Renamed => "Renamed",
            FileState::Deleted => "Deleted",
            FileState::Modified => "Modified",
            FileState::Conflicted => "Conflict",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Path relative to the repository root, without a trailing slash.
    pub path: String,
    pub state: FileState,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RepoStatus {
    pub root: PathBuf,
    pub branch: String,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: Vec<Change>,
}

impl RepoStatus {
    pub fn count(&self, f: impl Fn(&Change) -> bool) -> usize {
        self.changes.iter().filter(|c| f(c)).count()
    }

    /// "⎇ main ↑1 ↓2 · 3 changed · 1 untracked"
    pub fn summary(&self) -> String {
        let mut s = format!("⎇ {}", self.branch);
        if self.ahead > 0 {
            s.push_str(&format!(" ↑{}", self.ahead));
        }
        if self.behind > 0 {
            s.push_str(&format!(" ↓{}", self.behind));
        }
        let changed = self.count(|c| !matches!(c.state, FileState::Untracked | FileState::Ignored));
        let untracked = self.count(|c| c.state == FileState::Untracked);
        let conflicts = self.count(|c| c.state == FileState::Conflicted);
        if conflicts > 0 {
            s.push_str(&format!(" · {conflicts} conflicts"));
        }
        if changed > 0 {
            s.push_str(&format!(" · {changed} changed"));
        }
        if untracked > 0 {
            s.push_str(&format!(" · {untracked} untracked"));
        }
        if changed + untracked == 0 {
            s.push_str(" · clean");
        }
        s
    }
}

fn git(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(root);
    c.env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0");
    c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    c.stdin(Stdio::null());
    c
}

/// Cheap check that avoids spawning git outside repositories.
pub fn find_root_fast(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().take(40).find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

fn state_of(c: char) -> Option<FileState> {
    Some(match c {
        'M' | 'T' => FileState::Modified,
        'A' | 'C' => FileState::Added,
        'D' => FileState::Deleted,
        'R' => FileState::Renamed,
        _ => return None,
    })
}

pub fn parse_status(root: &Path, out: &[u8]) -> RepoStatus {
    let mut st = RepoStatus { root: root.to_path_buf(), ..Default::default() };
    let text = String::from_utf8_lossy(out);
    let mut toks = text.split('\0');
    while let Some(t) = toks.next() {
        if let Some(h) = t.strip_prefix("# branch.head ") {
            st.detached = h == "(detached)";
            st.branch = if st.detached { "detached HEAD".into() } else { h.to_string() };
        } else if let Some(u) = t.strip_prefix("# branch.upstream ") {
            st.upstream = Some(u.to_string());
        } else if let Some(ab) = t.strip_prefix("# branch.ab ") {
            let mut p = ab.split_whitespace();
            st.ahead = p.next().and_then(|a| a.trim_start_matches('+').parse().ok()).unwrap_or(0);
            st.behind = p.next().and_then(|b| b.trim_start_matches('-').parse().ok()).unwrap_or(0);
        } else if t.starts_with("1 ") || t.starts_with("2 ") || t.starts_with("u ") {
            let kind = t.as_bytes()[0];
            let fields: Vec<&str> = t
                .splitn(
                    match kind {
                        b'1' => 9,
                        b'2' => 10,
                        _ => 11,
                    },
                    ' ',
                )
                .collect();
            let (Some(xy), Some(path)) = (fields.get(1), fields.last()) else { continue };
            if kind == b'2' {
                toks.next(); // original path of a rename/copy
            }
            let mut ch = xy.chars();
            let (x, y) = (ch.next().unwrap_or('.'), ch.next().unwrap_or('.'));
            let state = if kind == b'u' {
                FileState::Conflicted
            } else {
                match [state_of(x), state_of(y)].into_iter().flatten().max() {
                    Some(s) => s,
                    None => continue,
                }
            };
            st.changes.push(Change {
                path: path.trim_end_matches('/').to_string(),
                state,
                staged: x != '.' && kind != b'u',
                unstaged: y != '.' || kind == b'u',
            });
        } else if let Some(p) = t.strip_prefix("? ") {
            st.changes.push(Change {
                path: p.trim_end_matches('/').to_string(),
                state: FileState::Untracked,
                staged: false,
                unstaged: true,
            });
        } else if let Some(p) = t.strip_prefix("! ") {
            st.changes.push(Change {
                path: p.trim_end_matches('/').to_string(),
                state: FileState::Ignored,
                staged: false,
                unstaged: false,
            });
        }
    }
    st
}

/// State of each direct child of `dir`, folders aggregating what is inside.
pub fn states_for_dir(st: &RepoStatus, dir: &Path) -> HashMap<String, FileState> {
    let rel = dir.strip_prefix(&st.root).unwrap_or(Path::new("")).to_string_lossy().into_owned();
    let rel = rel.trim_matches('/');
    let mut out: HashMap<String, FileState> = HashMap::new();
    let mut inherited: Option<FileState> = None;
    for c in &st.changes {
        // `dir` sits inside an untracked/ignored folder: everything inherits.
        if !rel.is_empty()
            && (rel == c.path || rel.starts_with(&format!("{}/", c.path)))
            && matches!(c.state, FileState::Untracked | FileState::Ignored)
        {
            inherited = Some(inherited.map_or(c.state, |s| s.max(c.state)));
            continue;
        }
        let rest = if rel.is_empty() { Some(c.path.as_str()) } else { c.path.strip_prefix(&format!("{rel}/")) };
        let Some(rest) = rest else { continue };
        let name = rest.split('/').next().unwrap_or(rest);
        if name.is_empty() {
            continue;
        }
        out.entry(name.to_string()).and_modify(|s| *s = (*s).max(c.state)).or_insert(c.state);
    }
    if let Some(s) = inherited {
        return HashMap::from([("*".to_string(), s)]); // sentinel: everything in this folder
    }
    out
}

/// Run `git status` in `dir`; `None` if `dir` isn't in a repository.
pub fn status(dir: &Path) -> Option<RepoStatus> {
    let root = find_root_fast(dir)?;
    let out = git(&root)
        .args(["status", "--porcelain=v2", "--branch", "-z", "--ignored=traditional", "--untracked-files=normal"])
        .output()
        .ok()?;
    out.status.success().then(|| parse_status(&root, &out.stdout))
}

fn run(mut cmd: Command, cancel: &AtomicBool, timeout: Duration) -> Result<String, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not run git: {e}"))?;
    let (mut o, mut e) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let ho = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = o.read_to_string(&mut s);
        s
    });
    let he = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = e.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let (out, err) = (ho.join().unwrap_or_default(), he.join().unwrap_or_default());
                return if st.success() {
                    Ok(out.trim().to_string())
                } else {
                    let msg = if err.trim().is_empty() { out } else { err };
                    Err(msg.lines().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().join("\n"))
                };
            }
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) || start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if cancel.load(Ordering::Relaxed) {
                        "Cancelled".into()
                    } else {
                        "git took too long".into()
                    });
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub fn stage(root: &Path, paths: &[PathBuf]) -> Result<String, String> {
    let mut c = git(root);
    c.args(["add", "--"]).args(paths);
    run(c, &AtomicBool::new(false), Duration::from_secs(60))
}

pub fn unstage(root: &Path, paths: &[PathBuf]) -> Result<String, String> {
    let mut c = git(root);
    c.args(["restore", "--staged", "--"]).args(paths);
    match run(c, &AtomicBool::new(false), Duration::from_secs(60)) {
        Ok(s) => Ok(s),
        Err(_) => {
            // No commits yet (no HEAD to restore from): drop from the index instead.
            let mut c = git(root);
            c.args(["rm", "--cached", "-r", "-q", "--"]).args(paths);
            run(c, &AtomicBool::new(false), Duration::from_secs(60))
        }
    }
}

/// Throw away uncommitted edits to tracked files. Untracked files are never touched.
pub fn discard(root: &Path, paths: &[PathBuf]) -> Result<String, String> {
    let mut c = git(root);
    c.args(["restore", "--worktree", "--"]).args(paths);
    run(c, &AtomicBool::new(false), Duration::from_secs(60))
}

pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("A commit message is required".into());
    }
    let mut c = git(root);
    c.args(["commit", "-m", message]);
    run(c, &AtomicBool::new(false), Duration::from_secs(120))
}

pub fn pull(root: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let mut c = git(root);
    c.args(["pull", "--ff-only"]);
    run(c, cancel, Duration::from_secs(300))
}

pub fn push(root: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let mut c = git(root);
    c.arg("push");
    run(c, cancel, Duration::from_secs(300))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sh(root: &Path, args: &[&str]) {
        let o = Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn repo(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        sh(&root, &["init", "-q", "-b", "main"]);
        sh(&root, &["config", "user.name", "T"]);
        sh(&root, &["config", "user.email", "t@example.com"]);
        sh(&root, &["config", "commit.gpgsign", "false"]);
        root
    }

    #[test]
    fn parses_porcelain_v2() {
        let out = b"# branch.oid abc\0# branch.head feature\0# branch.upstream origin/feature\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 a b src/lib.rs\0\
1 A. N... 000000 100644 100644 a b new file.txt\0\
2 R. N... 100644 100644 100644 a b R100 renamed.txt\0old.txt\0\
u UU N... 1 2 3 4 a b c conflict.txt\0\
? untracked.txt\0! target/\0";
        let st = parse_status(Path::new("/r"), out);
        assert_eq!((st.branch.as_str(), st.ahead, st.behind), ("feature", 2, 1));
        assert_eq!(st.upstream.as_deref(), Some("origin/feature"));
        let by = |p: &str| st.changes.iter().find(|c| c.path == p).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(by("src/lib.rs").state, FileState::Modified);
        assert!(by("src/lib.rs").unstaged && !by("src/lib.rs").staged);
        assert!(by("new file.txt").staged);
        assert_eq!(by("renamed.txt").state, FileState::Renamed);
        assert!(st.changes.iter().all(|c| c.path != "old.txt"));
        assert_eq!(by("conflict.txt").state, FileState::Conflicted);
        assert_eq!(by("target").state, FileState::Ignored);
        assert!(st.summary().contains("↑2") && st.summary().contains("↓1"));
    }

    #[test]
    fn folder_aggregation_and_inheritance() {
        let out = b"# branch.head main\0\
1 .M N... 100644 100644 100644 a b src/deep/x.rs\0\
? src/new.rs\0\
? build/\0";
        let st = parse_status(Path::new("/r"), out);
        let top = states_for_dir(&st, Path::new("/r"));
        assert_eq!(top["src"], FileState::Modified);
        assert_eq!(top["build"], FileState::Untracked);
        let src = states_for_dir(&st, Path::new("/r/src"));
        assert_eq!(src["deep"], FileState::Modified);
        assert_eq!(src["new.rs"], FileState::Untracked);
        let inside = states_for_dir(&st, Path::new("/r/build/out"));
        assert_eq!(inside["*"], FileState::Untracked);
    }

    #[test]
    fn real_repo_stage_commit_discard() {
        let r = repo("git_real");
        fs::write(r.join("a.txt"), "one\n").unwrap();
        fs::create_dir_all(r.join("sub")).unwrap();
        fs::write(r.join("sub/b.txt"), "b\n").unwrap();
        assert_eq!(find_root_fast(&r.join("sub")), Some(r.clone()));
        let st = status(&r).unwrap();
        assert!(st.changes.iter().all(|c| c.state == FileState::Untracked));

        stage(&r, &[r.join("a.txt")]).unwrap();
        let st = status(&r).unwrap();
        assert!(st.changes.iter().find(|c| c.path == "a.txt").unwrap().staged);
        unstage(&r, &[r.join("a.txt")]).unwrap(); // works before the first commit too
        assert!(!status(&r).unwrap().changes.iter().find(|c| c.path == "a.txt").unwrap().staged);

        stage(&r, &[r.join("a.txt"), r.join("sub")]).unwrap();
        commit(&r, "first").unwrap();
        assert!(commit(&r, "  ").is_err());
        assert!(status(&r).unwrap().changes.is_empty());
        assert_eq!(status(&r).unwrap().summary(), "⎇ main · clean");

        fs::write(r.join("a.txt"), "changed\n").unwrap();
        fs::write(r.join("keep-untracked.txt"), "u\n").unwrap();
        let st = status(&r).unwrap();
        assert_eq!(states_for_dir(&st, &r)["a.txt"], FileState::Modified);
        discard(&r, &[r.join("a.txt")]).unwrap();
        assert_eq!(fs::read_to_string(r.join("a.txt")).unwrap(), "one\n");
        assert!(r.join("keep-untracked.txt").exists(), "discard must never delete untracked files");
    }
}
