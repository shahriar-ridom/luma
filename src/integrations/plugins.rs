//! Custom actions and plugins.
//!
//! A plugin is a folder with a `plugin.toml` manifest. Plugins are
//! *declarative*: they contribute context-menu actions and metadata
//! providers that run external commands (an argument list, never a shell
//! string). Nothing runs when a plugin is loaded, and newly found plugins are
//! disabled until the user turns them on.
//!
//! ```toml
//! id = "image-tools"
//! name = "Image Tools"
//! version = "1.0"
//! description = "Small helpers for pictures"
//!
//! [[actions]]
//! id = "half"
//! name = "Resize to 50%"
//! command = ["convert", "%f", "-resize", "50%", "%d/%b-small.png"]
//! mime = ["image/*"]          # and/or: extensions = ["png"], folders = true
//!
//! [[metadata]]
//! title = "ImageMagick"
//! command = ["identify", "-verbose", "%f"]    # prints "Key: Value" lines
//! mime = ["image/*"]
//! ```
//!
//! Placeholders: `%f` first path, `%F` all paths (as separate arguments),
//! `%d` current folder, `%n` file name, `%b` name without extension,
//! `%u` URI, `%%` a literal percent sign.

use gtk::gio::prelude::FileExt;
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use crate::core::search::glob_match;

const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_ACTIONS: usize = 50;

// ---- matching ------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub mime: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matcher {
    pub files: bool,
    pub folders: bool,
    pub mime: Vec<String>,
    pub globs: Vec<String>,
}

impl Matcher {
    /// "files", "folders", "any", `*.png`, `image/*` — comma separated.
    pub fn parse(text: &str) -> Self {
        let mut m = Matcher::default();
        for t in text.split([',', ';']).map(str::trim).filter(|t| !t.is_empty()) {
            match t.to_lowercase().as_str() {
                "files" => m.files = true,
                "folders" | "dirs" => m.folders = true,
                "any" | "all" => {
                    m.files = true;
                    m.folders = true;
                }
                _ if t.contains('/') => m.mime.push(t.to_lowercase()),
                _ => m.globs.push(t.to_lowercase()),
            }
        }
        if !m.files && !m.folders {
            // Patterns alone mean files; nothing at all means everything.
            if m.mime.is_empty() && m.globs.is_empty() {
                m.files = true;
                m.folders = true;
            } else {
                m.files = true;
            }
        }
        m
    }

    fn item_ok(&self, it: &Item) -> bool {
        if it.is_dir && !self.folders || !it.is_dir && !self.files {
            return false;
        }
        if self.mime.is_empty() && self.globs.is_empty() {
            return true;
        }
        let name = it.name.to_lowercase();
        let nc: Vec<char> = name.chars().collect();
        self.globs.iter().any(|g| glob_match(&g.chars().collect::<Vec<_>>(), &nc))
            || self.mime.iter().any(|p| match p.strip_suffix("/*") {
                Some(prefix) => it.mime.starts_with(&format!("{prefix}/")),
                None => it.mime == *p,
            })
    }

    /// Every selected item must match; an empty selection never does.
    pub fn matches(&self, items: &[Item]) -> bool {
        !items.is_empty() && items.iter().all(|i| self.item_ok(i))
    }
}

// ---- command expansion ----------------------------------------------------------

pub fn expand(template: &[String], paths: &[PathBuf], dir: &Path) -> Vec<String> {
    let first = paths.first();
    let name = first.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = first.and_then(|p| p.file_stem()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let uri = |p: &Path| gtk::gio::File::for_path(p).uri().to_string();
    let mut out = Vec::new();
    for arg in template {
        if arg == "%F" {
            out.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
            continue;
        }
        let mut s = String::new();
        let mut it = arg.chars();
        while let Some(c) = it.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match it.next() {
                Some('f') => s.push_str(&first.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()),
                Some('d') => s.push_str(&dir.to_string_lossy()),
                Some('n') => s.push_str(&name),
                Some('b') => s.push_str(&base),
                Some('u') => s.push_str(&first.map(|p| uri(p)).unwrap_or_default()),
                Some('%') => s.push('%'),
                Some(o) => {
                    s.push('%');
                    s.push(o);
                }
                None => s.push('%'),
            }
        }
        out.push(s);
    }
    out
}

/// Split a user-written command line into arguments (quotes supported, no shell).
pub fn parse_command(line: &str) -> Result<Vec<String>, String> {
    let v = shell_words::split(line).map_err(|e| format!("Unbalanced quotes: {e}"))?;
    if v.is_empty() {
        return Err("The command is empty".into());
    }
    Ok(v)
}

// ---- registry ---------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ExtAction {
    /// `c:<id>` for custom actions, `p:<plugin>/<action>` for plugins.
    pub key: String,
    pub name: String,
    /// "Custom" or the plugin's name — used to group menu entries.
    pub group: String,
    pub argv: Vec<String>,
    pub matcher: Matcher,
    pub wait: bool,
}

#[derive(Clone, Debug)]
pub struct MetaProvider {
    pub title: String,
    pub argv: Vec<String>,
    pub matcher: Matcher,
}

#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    pub enabled: bool,
    pub actions: usize,
    pub metadata: usize,
}

#[derive(Default)]
pub struct Extensions {
    pub actions: Vec<ExtAction>,
    pub providers: Vec<MetaProvider>,
    pub plugins: Vec<PluginInfo>,
    /// Plugins that failed to load: (folder, reason).
    pub problems: Vec<(PathBuf, String)>,
}

#[derive(Deserialize)]
struct Manifest {
    id: String,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    actions: Vec<ManifestAction>,
    #[serde(default)]
    metadata: Vec<ManifestMeta>,
}

#[derive(Deserialize)]
struct Targets {
    #[serde(default)]
    mime: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    folders: bool,
    #[serde(default = "yes")]
    files: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct ManifestAction {
    id: String,
    name: String,
    command: Vec<String>,
    #[serde(default)]
    wait: bool,
    #[serde(flatten)]
    targets: Targets,
}

#[derive(Deserialize)]
struct ManifestMeta {
    title: String,
    command: Vec<String>,
    #[serde(flatten)]
    targets: Targets,
}

fn matcher_of(t: &Targets) -> Matcher {
    Matcher {
        files: t.files,
        folders: t.folders,
        mime: t.mime.iter().map(|m| m.to_lowercase()).collect(),
        globs: t.extensions.iter().map(|e| format!("*.{}", e.trim_start_matches('.').to_lowercase())).collect(),
    }
}

fn safe_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

pub fn user_plugin_dir() -> PathBuf {
    gtk::glib::user_data_dir().join("file-manager").join("plugins")
}

fn load_manifest(dir: &Path) -> Result<Manifest, String> {
    let file = dir.join("plugin.toml");
    let md = std::fs::metadata(&file).map_err(|_| "no plugin.toml".to_string())?;
    if md.len() > MAX_MANIFEST {
        return Err("plugin.toml is too large".into());
    }
    let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let m: Manifest = toml::from_str(&text).map_err(|e| e.message().to_string())?;
    if !safe_id(&m.id) {
        return Err(format!("invalid plugin id “{}”", m.id));
    }
    if m.actions.len() > MAX_ACTIONS {
        return Err("too many actions".into());
    }
    for a in &m.actions {
        if !safe_id(&a.id) || a.command.is_empty() || a.name.trim().is_empty() {
            return Err(format!("action “{}” needs an id, a name and a command", a.id));
        }
    }
    if m.metadata.iter().any(|x| x.command.is_empty()) {
        return Err("a metadata provider has an empty command".into());
    }
    Ok(m)
}

impl Extensions {
    /// Build the registry. `enabled(id)` says whether a plugin is switched on;
    /// `custom` are the user's own actions from the database.
    pub fn load(
        custom: &[crate::database::CustomActionRow],
        plugin_dirs: &[PathBuf],
        enabled: impl Fn(&str) -> bool,
    ) -> Self {
        let mut ext = Extensions::default();
        for c in custom {
            match parse_command(&c.command) {
                Ok(argv) => ext.actions.push(ExtAction {
                    key: format!("c:{}", c.id),
                    name: c.name.clone(),
                    group: "Custom".into(),
                    argv,
                    matcher: Matcher::parse(&c.patterns),
                    wait: c.wait,
                }),
                Err(e) => ext.problems.push((PathBuf::from(&c.name), e)),
            }
        }
        let mut seen = std::collections::HashSet::new();
        for root in plugin_dirs {
            let Ok(rd) = std::fs::read_dir(root) else { continue };
            let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for dir in dirs {
                match load_manifest(&dir) {
                    Err(e) => ext.problems.push((dir, e)),
                    Ok(m) if !seen.insert(m.id.clone()) => {
                        ext.problems.push((dir, format!("duplicate plugin id “{}”", m.id)))
                    }
                    Ok(m) => {
                        let on = enabled(&m.id);
                        ext.plugins.push(PluginInfo {
                            id: m.id.clone(),
                            name: m.name.clone(),
                            version: m.version.clone(),
                            description: m.description.clone(),
                            dir: dir.clone(),
                            enabled: on,
                            actions: m.actions.len(),
                            metadata: m.metadata.len(),
                        });
                        if !on {
                            continue;
                        }
                        for a in m.actions {
                            ext.actions.push(ExtAction {
                                key: format!("p:{}/{}", m.id, a.id),
                                name: a.name,
                                group: m.name.clone(),
                                argv: a.command,
                                matcher: matcher_of(&a.targets),
                                wait: a.wait,
                            });
                        }
                        for p in m.metadata {
                            ext.providers.push(MetaProvider {
                                title: p.title,
                                argv: p.command,
                                matcher: matcher_of(&p.targets),
                            });
                        }
                    }
                }
            }
        }
        ext
    }

    pub fn find(&self, key: &str) -> Option<&ExtAction> {
        self.actions.iter().find(|a| a.key == key)
    }

    /// Actions applicable to the whole selection, grouped in registry order.
    pub fn matching(&self, items: &[Item]) -> Vec<&ExtAction> {
        self.actions.iter().filter(|a| a.matcher.matches(items)).collect()
    }
}

// ---- running ------------------------------------------------------------------------

/// Run to completion with cancel and timeout; returns captured stdout.
fn run_capture(argv: &[String], dir: &Path, cancel: &AtomicBool, timeout: Duration) -> Result<String, String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut child = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {prog}: {e}"))?;
    let (mut o, mut e) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let ho = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut o).take(1 << 20).read_to_string(&mut s);
        s
    });
    let he = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut e).take(1 << 20).read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let (out, err) = (ho.join().unwrap_or_default(), he.join().unwrap_or_default());
                if st.success() {
                    return Ok(out);
                }
                let last = |s: &str| s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                return Err(format!(
                    "exit {}: {}",
                    st.code().unwrap_or(-1),
                    last(if err.trim().is_empty() { &out } else { &err })
                ));
            }
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) || start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if cancel.load(Ordering::Relaxed) { "Cancelled".into() } else { "Timed out".into() });
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// For actions: waits up to 10 minutes; returns the last line of output.
pub fn run_command(argv: &[String], dir: &Path, cancel: &AtomicBool) -> Result<String, String> {
    run_capture(argv, dir, cancel, Duration::from_secs(600))
        .map(|o| o.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string())
}

/// Optional command-line metadata sources that are used when installed.
pub fn builtin_providers() -> Vec<MetaProvider> {
    let have =
        |p: &str| std::env::var_os("PATH").is_some_and(|v| std::env::split_paths(&v).any(|d| d.join(p).is_file()));
    let mut v = Vec::new();
    let m = |mime: &[&str]| Matcher {
        files: true,
        folders: false,
        mime: mime.iter().map(|s| s.to_string()).collect(),
        globs: vec![],
    };
    if have("pdfinfo") {
        v.push(MetaProvider {
            title: "PDF".into(),
            argv: vec!["pdfinfo".into(), "%f".into()],
            matcher: m(&["application/pdf"]),
        });
    }
    if have("ffprobe") {
        v.push(MetaProvider {
            title: "Media".into(),
            argv: ["ffprobe", "-v", "error", "-show_entries", "format=duration,bit_rate,format_long_name:stream=codec_type,codec_name,width,height,r_frame_rate,sample_rate,channels", "-of", "default=noprint_wrappers=1", "%f"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            matcher: m(&["video/*"]),
        });
    }
    v
}

/// Gather every metadata group for one file: built-in extractors, installed
/// helpers, and enabled plugins. Blocking — call from a worker thread.
pub fn collect_metadata(
    path: &Path,
    mime: &str,
    ext: &[MetaProvider],
    cancel: &AtomicBool,
) -> Vec<crate::core::metadata::advanced::MetaGroup> {
    use crate::core::metadata::advanced::{MetaGroup, extract_builtin, parse_kv};
    let mut groups = extract_builtin(path, mime);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let item = [Item { name, is_dir: path.is_dir(), mime: mime.to_string() }];
    let dir = path.parent().unwrap_or(Path::new("."));
    for p in builtin_providers().iter().chain(ext) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !p.matcher.matches(&item) {
            continue;
        }
        let argv = expand(&p.argv, &[path.to_path_buf()], dir);
        let items = match run_capture(&argv, dir, cancel, Duration::from_secs(10)) {
            Ok(out) => parse_kv(&out),
            Err(e) => vec![crate::core::metadata::advanced::MetaItem { key: "Error".into(), value: e }],
        };
        if !items.is_empty() {
            groups.push(MetaGroup { title: p.title.clone(), items });
        }
    }
    groups
}

/// Start without waiting (GUI programs); output is discarded.
pub fn spawn_detached(argv: &[String], dir: &Path) -> Result<(), String> {
    let (prog, args) = argv.split_first().ok_or("empty command")?;
    let mut c = Command::new(prog);
    c.args(args).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| format!("could not start {prog}: {e}"))?;
    // Reap in the background so the child never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn item(name: &str, dir: bool, mime: &str) -> Item {
        Item { name: name.into(), is_dir: dir, mime: mime.into() }
    }

    #[test]
    fn matcher_semantics() {
        let img = item("Photo.PNG", false, "image/png");
        let txt = item("a.txt", false, "text/plain");
        let dir = item("docs", true, "inode/directory");
        assert!(Matcher::parse("").matches(&[img.clone(), dir.clone()]), "empty = anything");
        assert!(Matcher::parse("image/*").matches(std::slice::from_ref(&img)));
        assert!(!Matcher::parse("image/*").matches(&[img.clone(), txt.clone()]), "all must match");
        assert!(Matcher::parse("*.png, *.jpg").matches(std::slice::from_ref(&img)));
        assert!(!Matcher::parse("*.png").matches(std::slice::from_ref(&dir)), "patterns alone mean files");
        assert!(Matcher::parse("folders").matches(std::slice::from_ref(&dir)));
        assert!(!Matcher::parse("files").matches(&[dir]));
        assert!(!Matcher::parse("any").matches(&[]));
    }

    #[test]
    fn expansion_keeps_arguments_whole() {
        let paths = vec![PathBuf::from("/d/my file.txt"), PathBuf::from("/d/b.txt")];
        let t: Vec<String> = ["cp", "%F", "%d/%b-copy%%.%n", "--uri=%u"].iter().map(|s| s.to_string()).collect();
        let out = expand(&t, &paths, Path::new("/d"));
        assert_eq!(out[..3], ["cp", "/d/my file.txt", "/d/b.txt"]);
        assert_eq!(out[3], "/d/my file-copy%.my file.txt");
        assert_eq!(out[4], "--uri=file:///d/my%20file.txt");
        assert_eq!(parse_command(r#"code --goto "%f" 'x y'"#).unwrap(), ["code", "--goto", "%f", "x y"]);
        assert!(parse_command("echo 'oops").is_err());
        assert!(parse_command("  ").is_err());
    }

    #[test]
    fn plugin_loading_is_opt_in_and_validated() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/plugins");
        let _ = fs::remove_dir_all(&root);
        for (dir, body) in [
            (
                "good",
                "id='good'\nname='Good'\nversion='1'\n[[actions]]\nid='go'\nname='Go'\ncommand=['echo','%f']\nextensions=['txt']\n[[metadata]]\ntitle='Info'\ncommand=['true']\nmime=['image/*']\n",
            ),
            ("badid", "id='../evil'\nname='x'\n"),
            ("broken", "this is not toml ["),
            ("nocmd", "id='n'\nname='n'\n[[actions]]\nid='a'\nname='A'\ncommand=[]\n"),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("plugin.toml"), body).unwrap();
        }
        let off = Extensions::load(&[], std::slice::from_ref(&root), |_| false);
        assert_eq!(off.plugins.len(), 1);
        assert!(off.actions.is_empty() && off.providers.is_empty(), "disabled plugins contribute nothing");
        assert_eq!(off.problems.len(), 3);
        let on = Extensions::load(&[], &[root], |id| id == "good");
        assert_eq!(on.actions.len(), 1);
        assert_eq!(on.actions[0].key, "p:good/go");
        assert!(on.actions[0].matcher.matches(&[item("x.TXT", false, "text/plain")]));
        assert_eq!(on.providers.len(), 1);
        assert!(on.find("p:good/go").is_some());
    }

    #[test]
    fn plugin_metadata_providers_run_and_parse() {
        let f = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let p = MetaProvider {
            title: "Test".into(),
            argv: vec!["printf".into(), "Name: %n\\nSize=3\\n".into()],
            matcher: Matcher::parse("any"),
        };
        let groups = collect_metadata(&f, "text/plain", &[p], &AtomicBool::new(false));
        let g = groups.iter().find(|g| g.title == "Test").expect("provider group");
        assert_eq!((g.items[0].key.as_str(), g.items[0].value.as_str()), ("Name", "Cargo.toml"));
        assert_eq!(g.items[1].value, "3");
        // Non-matching providers are not run.
        let never =
            MetaProvider { title: "Img".into(), argv: vec!["false".into()], matcher: Matcher::parse("image/*") };
        assert!(collect_metadata(&f, "text/plain", &[never], &AtomicBool::new(false)).iter().all(|g| g.title != "Img"));
    }

    #[test]
    fn running_commands() {
        let c = AtomicBool::new(false);
        let d = Path::new(".");
        assert_eq!(run_command(&["echo".into(), "hi there".into()], d, &c).unwrap(), "hi there");
        assert!(
            run_command(&["sh".into(), "-c".into(), "echo boom >&2; exit 3".into()], d, &c)
                .unwrap_err()
                .contains("boom")
        );
        assert!(run_command(&["definitely-not-a-program".into()], d, &c).is_err());
    }
}
