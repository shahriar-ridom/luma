//! Recursive file search (names, content, type, size, date). Pure `std`
//! filesystem access on a worker thread; results stream back in batches.

use gtk::gio;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

pub const MAX_HITS: usize = 5000;
const MAX_CONTENT_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KindFilter {
    #[default]
    Any,
    Folders,
    Images,
    Audio,
    Video,
    Documents,
    Text,
    Archives,
}

impl KindFilter {
    pub const ALL: [KindFilter; 8] = [
        KindFilter::Any,
        KindFilter::Folders,
        KindFilter::Images,
        KindFilter::Audio,
        KindFilter::Video,
        KindFilter::Documents,
        KindFilter::Text,
        KindFilter::Archives,
    ];
    pub fn label(self) -> &'static str {
        match self {
            KindFilter::Any => "Any type",
            KindFilter::Folders => "Folders",
            KindFilter::Images => "Images",
            KindFilter::Audio => "Audio",
            KindFilter::Video => "Video",
            KindFilter::Documents => "Documents",
            KindFilter::Text => "Text & code",
            KindFilter::Archives => "Archives",
        }
    }

    fn matches(self, mime: &str, is_dir: bool) -> bool {
        match self {
            KindFilter::Any => true,
            KindFilter::Folders => is_dir,
            _ if is_dir => false,
            KindFilter::Images => mime.starts_with("image/"),
            KindFilter::Audio => mime.starts_with("audio/"),
            KindFilter::Video => mime.starts_with("video/"),
            KindFilter::Text => {
                mime.starts_with("text/")
                    || mime.contains("json")
                    || mime.contains("xml")
                    || mime.contains("shellscript")
                    || mime.contains("javascript")
            }
            KindFilter::Documents => {
                mime == "application/pdf"
                    || mime == "application/rtf"
                    || mime == "application/msword"
                    || mime.starts_with("application/vnd.oasis")
                    || mime.starts_with("application/vnd.openxmlformats")
                    || mime.starts_with("application/vnd.ms-")
                    || mime.contains("epub")
                    || mime == "text/markdown"
            }
            KindFilter::Archives => {
                ["zip", "tar", "compress", "7z", "rar", "gzip", "xz", "zstd", "bzip"].iter().any(|k| mime.contains(k))
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    pub root: PathBuf,
    /// Substring, or a glob if it contains `*` / `?`. Case-insensitive.
    pub name: String,
    /// Text to find inside files (case-insensitive).
    pub content: String,
    /// Extensions without dots, e.g. `["txt", "tar.gz"]`.
    pub exts: Vec<String>,
    pub kind: KindFilter,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Unix seconds; only items modified at or after this.
    pub modified_after: Option<i64>,
    pub recursive: bool,
    pub hidden: bool,
    /// Trees whose file contents must not be read (e.g. cloud mounts).
    pub no_content: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
    pub snippet: Option<String>,
}

pub enum SearchEvent {
    Hits(Vec<Hit>),
    Done { scanned: u64, found: usize, truncated: bool },
}

/// Case-insensitive glob supporting `*` and `?` (both inputs lowercase).
pub fn glob_match(pat: &[char], text: &[char]) -> bool {
    let (mut p, mut t, mut star, mut mark) = (0, 0, None, 0);
    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

fn is_pseudo(p: &Path) -> bool {
    matches!(p.to_str(), Some("/proc" | "/sys" | "/dev" | "/run"))
}

fn mime_of(name: &str) -> String {
    gio::content_type_guess(Some(name), None).0.to_string()
}

struct Matcher<'a> {
    q: &'a SearchQuery,
    name_lc: Vec<char>,
    is_glob: bool,
    needle: String,
}

impl<'a> Matcher<'a> {
    fn new(q: &'a SearchQuery) -> Self {
        let name = q.name.trim().to_lowercase();
        Self {
            is_glob: name.contains('*') || name.contains('?'),
            name_lc: name.chars().collect(),
            needle: q.content.to_lowercase(),
            q,
        }
    }

    fn name_ok(&self, name: &str) -> bool {
        if self.name_lc.is_empty() {
            return true;
        }
        let n = name.to_lowercase();
        if self.is_glob {
            glob_match(&self.name_lc, &n.chars().collect::<Vec<_>>())
        } else {
            n.contains(&self.name_lc.iter().collect::<String>())
        }
    }

    fn ext_ok(&self, name: &str) -> bool {
        if self.q.exts.is_empty() {
            return true;
        }
        let n = name.to_lowercase();
        self.q.exts.iter().any(|e| n.ends_with(&format!(".{}", e.trim_start_matches('.').to_lowercase())))
    }

    /// Returns `Some(snippet)` when the content matches (or no content query).
    fn content_ok(&self, path: &Path, size: u64) -> Option<Option<String>> {
        if self.needle.is_empty() {
            return Some(None);
        }
        if size > MAX_CONTENT_BYTES || size == 0 {
            return None;
        }
        let f = fs::File::open(path).ok()?;
        let mut head = [0u8; 4096];
        let n = (&f).take(4096).read(&mut head).ok()?;
        if head[..n].contains(&0) {
            return None; // binary
        }
        let mut rd = BufReader::new(fs::File::open(path).ok()?);
        let mut buf = Vec::new();
        let mut line_no = 0;
        loop {
            buf.clear();
            if rd.read_until(b'\n', &mut buf).ok()? == 0 {
                return None;
            }
            line_no += 1;
            let line = String::from_utf8_lossy(&buf);
            if line.to_lowercase().contains(&self.needle) {
                let t = line.trim();
                let snip: String = t.chars().take(160).collect();
                return Some(Some(format!("{line_no}: {snip}")));
            }
        }
    }
}

/// Synchronous search; `emit` receives batches. Cancel via the flag.
pub fn run(q: &SearchQuery, cancel: &AtomicBool, mut emit: impl FnMut(SearchEvent)) {
    let m = Matcher::new(q);
    let mut stack = vec![q.root.clone()];
    let (mut scanned, mut found) = (0u64, 0usize);
    let mut batch: Vec<Hit> = Vec::new();
    let mut last = Instant::now();
    let mut truncated = false;
    let size_filter = q.min_size.is_some() || q.max_size.is_some();

    'walk: while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            if cancel.load(Ordering::Relaxed) {
                break 'walk;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            if !q.hidden && name.starts_with('.') {
                continue;
            }
            let Ok(ft) = ent.file_type() else { continue };
            let path = ent.path();
            scanned += 1;
            let is_dir = ft.is_dir();
            if is_dir && q.recursive && !is_pseudo(&path) {
                stack.push(path.clone());
            }
            let Ok(md) = ent.metadata() else { continue };
            let size = if is_dir { 0 } else { md.len() };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            if !m.name_ok(&name) || !m.ext_ok(&name) {
                continue;
            }
            if q.kind != KindFilter::Any && !q.kind.matches(&mime_of(&name), is_dir) {
                continue;
            }
            if size_filter && (is_dir || q.min_size.is_some_and(|m| size < m) || q.max_size.is_some_and(|m| size > m)) {
                continue;
            }
            if q.modified_after.is_some_and(|t| mtime < t) {
                continue;
            }
            let snippet = if m.needle.is_empty() {
                None
            } else if is_dir || !ft.is_file() || q.no_content.iter().any(|p| path.starts_with(p)) {
                continue;
            } else {
                match m.content_ok(&path, size) {
                    Some(s) => s,
                    None => continue,
                }
            };
            found += 1;
            batch.push(Hit { path, is_dir, size, mtime, snippet });
            if found >= MAX_HITS {
                truncated = true;
                break 'walk;
            }
            if batch.len() >= 64 || last.elapsed() > Duration::from_millis(120) {
                emit(SearchEvent::Hits(std::mem::take(&mut batch)));
                last = Instant::now();
            }
        }
    }
    if !batch.is_empty() {
        emit(SearchEvent::Hits(batch));
    }
    emit(SearchEvent::Done { scanned, found, truncated });
}

pub fn spawn(q: SearchQuery) -> (Arc<AtomicBool>, async_channel::Receiver<SearchEvent>) {
    let (tx, rx) = async_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("fm-search".into())
        .spawn(move || {
            run(&q, &c, |ev| {
                let _ = tx.send_blocking(ev);
            })
        })
        .expect("spawn search thread");
    (cancel, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub/deep")).unwrap();
        fs::write(root.join("Notes.TXT"), "hello world\nsecond line TODO fix\n").unwrap();
        fs::write(root.join("sub/readme.md"), "# title\n").unwrap();
        fs::write(root.join("sub/deep/data.bin"), [0u8, 1, 2, 3, b'T', b'O', b'D', b'O']).unwrap();
        fs::write(root.join("sub/deep/big.dat"), vec![b'x'; 5000]).unwrap();
        fs::write(root.join(".hidden.txt"), "TODO").unwrap();
        root
    }

    fn search(q: SearchQuery) -> Vec<String> {
        let mut out = Vec::new();
        run(&q, &AtomicBool::new(false), |ev| {
            if let SearchEvent::Hits(h) = ev {
                out.extend(h.into_iter().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned()));
            }
        });
        out.sort();
        out
    }

    fn base(root: &Path) -> SearchQuery {
        SearchQuery { root: root.to_path_buf(), recursive: true, ..Default::default() }
    }

    #[test]
    fn glob() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert!(glob_match(&c("*.txt"), &c("notes.txt")));
        assert!(glob_match(&c("n?tes*"), &c("notes.txt")));
        assert!(!glob_match(&c("*.md"), &c("notes.txt")));
        assert!(glob_match(&c("*"), &c("")));
    }

    #[test]
    fn name_ext_hidden_recursive() {
        let r = tree("search_name");
        assert_eq!(search(SearchQuery { name: "NOTES".into(), ..base(&r) }), ["Notes.TXT"]);
        assert_eq!(search(SearchQuery { name: "*.md".into(), ..base(&r) }), ["readme.md"]);
        assert_eq!(search(SearchQuery { exts: vec!["txt".into()], ..base(&r) }), ["Notes.TXT"]);
        assert_eq!(
            search(SearchQuery { exts: vec!["txt".into()], hidden: true, ..base(&r) }),
            [".hidden.txt", "Notes.TXT"]
        );
        assert!(
            !search(SearchQuery { name: "data".into(), recursive: false, ..base(&r) })
                .contains(&"data.bin".to_string())
        );
    }

    #[test]
    fn content_skips_binary_and_reports_line() {
        let r = tree("search_content");
        let mut found = Vec::new();
        run(&SearchQuery { content: "todo".into(), ..base(&r) }, &AtomicBool::new(false), |ev| {
            if let SearchEvent::Hits(h) = ev {
                found.extend(h);
            }
        });
        assert_eq!(found.len(), 1, "binary file and hidden file must not match");
        assert_eq!(found[0].path.file_name().unwrap(), "Notes.TXT");
        assert!(found[0].snippet.as_deref().unwrap().starts_with("2: "));
    }

    #[test]
    fn size_and_kind_filters() {
        let r = tree("search_size");
        assert_eq!(search(SearchQuery { min_size: Some(1000), ..base(&r) }), ["big.dat"]);
        assert_eq!(search(SearchQuery { kind: KindFilter::Folders, ..base(&r) }), ["deep", "sub"]);
        assert_eq!(search(SearchQuery { kind: KindFilter::Text, ..base(&r) }), ["Notes.TXT", "readme.md"]);
    }
}
