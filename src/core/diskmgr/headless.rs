//! Unattended cleanup: what `luma --cleanup-run` does, with no window.
//!
//! Only user-level providers run, only the ones the settings list, and never
//! the slow whole-home scans. Nothing here can ask for administrator rights.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use serde::{Deserialize, Serialize};

use super::{
    Action, Env, Risk, Scope,
    exec::StdRunner,
    providers::user_providers,
    schedule::{CleanupConfig, SCHEDULABLE},
};
use crate::core::filesystem::human_size;

/// Runs kept in the log file.
const MAX_RECORDS: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunEntry {
    pub id: String,
    /// Bytes removed, or that would be removed in a dry run.
    pub bytes: u64,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub entries: Vec<RunEntry>,
    pub notes: Vec<String>,
}

impl Report {
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    pub fn errors(&self) -> Vec<&str> {
        self.entries.iter().flat_map(|e| e.errors.iter().map(String::as_str)).collect()
    }

    pub fn summary(&self) -> String {
        let label = |id: &str| SCHEDULABLE.iter().find(|(s, _)| *s == id).map_or(id, |(_, l)| *l).to_string();
        let parts: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.bytes > 0)
            .map(|e| format!("{} {}", label(&e.id), size(e.bytes)))
            .collect();
        let what = if parts.is_empty() { "Nothing to clean".to_string() } else { parts.join(", ") };
        match (self.dry_run, self.errors().len()) {
            (true, _) => format!("Would remove: {what}"),
            (false, 0) => what,
            (false, n) => format!("{what} ({n} problem{})", if n == 1 { "" } else { "s" }),
        }
    }
}

fn size(b: u64) -> String {
    human_size(b.min(i64::MAX as u64) as i64)
}

pub fn run_unattended(env: &mut Env, cfg: &CleanupConfig, dry_run: bool, cancel: &AtomicBool) -> Report {
    env.thresholds = cfg.thresholds();
    let mut report = Report { dry_run, ..Report::default() };
    // `false`: the slow scans never run unattended.
    for p in user_providers(false) {
        if !cfg.wants(p.id()) {
            continue;
        }
        if !p.available(env) {
            report.notes.push(format!("{}: nothing to look at here", p.id()));
            continue;
        }
        let mut entry = RunEntry { id: p.id().to_string(), ..RunEntry::default() };
        for s in p.scan(env, cancel) {
            if s.scope != Scope::User || s.risk == Risk::SuggestOnly {
                continue;
            }
            let items: Vec<_> = s.items.into_iter().filter(|i| i.selected).collect();
            let plan = p.plan(&items, env);
            // A command has no place in an unattended run.
            if plan.actions.iter().any(|a| matches!(a, Action::Exec(_))) {
                report.notes.push(format!("{}: skipped, it wanted to run a command", p.id()));
                continue;
            }
            if dry_run {
                entry.bytes += items.iter().map(|i| i.bytes).sum::<u64>();
            } else {
                let o = p.run(&plan, env, cancel, &|_| {});
                entry.bytes += o.freed;
                entry.errors.extend(o.errors);
            }
        }
        report.entries.push(entry);
    }
    report
}

// ---- Files ---------------------------------------------------------------

fn xdg(var: &str, rel: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rel))
}

pub fn config_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("luma/cleanup.toml")
}

pub fn runs_path() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("luma/cleanup-runs.jsonl")
}

/// A missing file means the defaults. A file that exists but is invalid is an error.
pub fn load_config(path: &Path) -> Result<CleanupConfig, String> {
    match fs::read_to_string(path) {
        Ok(text) => CleanupConfig::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CleanupConfig::default()),
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

/// Write `text` so a crash or a full disk cannot leave a half-written file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Milliseconds since the epoch.
    pub ts: i64,
    pub freed: u64,
    pub summary: String,
}

/// Lines that cannot be read are skipped: the log is only a convenience.
pub fn read_runs(path: &Path) -> Vec<RunRecord> {
    fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

pub fn append_run(path: &Path, rec: &RunRecord) -> std::io::Result<()> {
    let mut all = read_runs(path);
    all.push(rec.clone());
    let keep = all.split_at(all.len().saturating_sub(MAX_RECORDS)).1;
    let mut text = String::new();
    for r in keep {
        text.push_str(&serde_json::to_string(r).map_err(std::io::Error::other)?);
        text.push('\n');
    }
    write_atomic(path, &text)
}

// ---- Command line --------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub dry_run: bool,
    pub config: Option<PathBuf>,
}

/// `Ok(None)`: not a cleanup run, start the window as usual.
pub fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    if !args.iter().any(|a| a == "--cleanup-run") {
        return Ok(None);
    }
    let mut out = Args::default();
    // The first argument is the program itself.
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cleanup-run" => {}
            "--dry-run" => out.dry_run = true,
            "--config" => {
                out.config = Some(it.next().map(PathBuf::from).ok_or("--config needs a file name.")?);
            }
            other => return Err(format!("Unknown option {other}.")),
        }
    }
    Ok(Some(out))
}

const USAGE: &str = "Usage: luma --cleanup-run [--dry-run] [--config FILE]";

/// Run a cleanup without a window. Returns the exit status: 0 done, 1 done
/// with problems, 2 bad command line or settings.
pub fn main(args: Args) -> i32 {
    let path = args.config.clone().unwrap_or_else(config_path);
    let cfg = match load_config(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("luma: {e}\n{USAGE}");
            return 2;
        }
    };
    if !cfg.enabled && !args.dry_run {
        println!("Scheduled cleanup is turned off in {}.", path.display());
        return 0;
    }
    let runner = StdRunner::default();
    let mut env = Env::detect(&runner);
    let report = run_unattended(&mut env, &cfg, args.dry_run, &AtomicBool::new(false));
    println!("{}", report.summary());
    for n in &report.notes {
        println!("note: {n}");
    }
    for e in report.errors() {
        eprintln!("luma: {e}");
    }
    if !args.dry_run {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let rec = RunRecord { ts, freed: report.bytes(), summary: report.summary() };
        if let Err(e) = append_run(&runs_path(), &rec) {
            eprintln!("luma: could not record this run: {e}");
        }
    }
    i32::from(!report.errors().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diskmgr::exec::FakeRunner;
    use std::time::{Duration, SystemTime};

    fn dir(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-headless").join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn file(p: &Path, len: usize, age_days: u64) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; len]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_days * 86400);
        fs::File::open(p).unwrap().set_times(fs::FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
    }

    fn cfg(providers: &[&str]) -> CleanupConfig {
        CleanupConfig {
            enabled: true,
            providers: providers.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn removes_only_what_the_settings_list() {
        let home = dir("run");
        file(&home.join(".cache/app/old"), 5000, 90);
        file(&home.join(".cache/app/new"), 700, 0);
        file(&home.join(".cache/thumbnails/normal/t.png"), 300, 400);
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home.clone(), &runner);

        let r = run_unattended(&mut env, &cfg(&["user-cache"]), false, &AtomicBool::new(false));
        assert_eq!(r.bytes(), 5000);
        assert!(r.errors().is_empty());
        assert!(!home.join(".cache/app/old").exists());
        assert!(home.join(".cache/app/new").exists(), "recent files stay");
        assert!(home.join(".cache/thumbnails/normal/t.png").exists(), "thumbnails were not listed");
        assert!(runner.calls().is_empty(), "no command was run");
    }

    #[test]
    fn a_dry_run_changes_nothing_but_reports_the_same_size() {
        let home = dir("dry");
        file(&home.join(".cache/app/old"), 5000, 90);
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home.clone(), &runner);
        let r = run_unattended(&mut env, &cfg(&["user-cache"]), true, &AtomicBool::new(false));
        assert_eq!(r.bytes(), 5000);
        assert!(home.join(".cache/app/old").exists());
        assert!(r.summary().starts_with("Would remove: Application caches"), "{}", r.summary());
    }

    #[test]
    fn the_config_thresholds_apply() {
        let home = dir("days");
        file(&home.join(".cache/app/mid"), 1000, 20);
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home.clone(), &runner);
        let mut c = cfg(&["user-cache"]);
        c.cache_days = 30;
        assert_eq!(run_unattended(&mut env, &c, true, &AtomicBool::new(false)).bytes(), 0);
        c.cache_days = 10;
        assert_eq!(run_unattended(&mut env, &c, true, &AtomicBool::new(false)).bytes(), 1000);
    }

    #[test]
    fn trash_is_untouched_unless_asked_for() {
        let home = dir("trash");
        let t = home.join(".local/share/Trash");
        file(&t.join("files/old.bin"), 2000, 100);
        fs::create_dir_all(t.join("info")).unwrap();
        fs::write(
            t.join("info/old.bin.trashinfo"),
            "[Trash Info]\nPath=/x/old.bin\nDeletionDate=2020-01-01T00:00:00\n",
        )
        .unwrap();
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home.clone(), &runner);
        run_unattended(&mut env, &cfg(&["user-cache"]), false, &AtomicBool::new(false));
        assert!(t.join("files/old.bin").exists());
        let r = run_unattended(&mut env, &cfg(&["trash"]), false, &AtomicBool::new(false));
        assert!(r.bytes() >= 2000, "the file and its info entry are counted");
        assert!(!t.join("files/old.bin").exists());
        assert!(!t.join("info/old.bin.trashinfo").exists());
    }

    #[test]
    fn never_runs_anything_outside_the_user_providers() {
        let home = dir("scope");
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home, &runner);
        // Even if the settings were built in code, administrator providers do not run.
        let mut c = cfg(&[]);
        c.providers =
            ["pkg-cache", "journal", "coredumps", "orphans", "large-files", "duplicates"].map(String::from).to_vec();
        let r = run_unattended(&mut env, &c, false, &AtomicBool::new(false));
        assert!(r.entries.is_empty());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn summary_reads_naturally() {
        let r = Report {
            dry_run: false,
            entries: vec![
                RunEntry { id: "user-cache".into(), bytes: 2048, errors: vec![] },
                RunEntry { id: "thumbnails".into(), bytes: 0, errors: vec![] },
            ],
            notes: vec![],
        };
        assert_eq!(r.summary(), format!("Application caches {}", size(2048)));
        assert_eq!(Report::default().summary(), "Nothing to clean");
        let bad = Report {
            entries: vec![RunEntry { id: "trash".into(), bytes: 0, errors: vec!["x".into()] }],
            ..Default::default()
        };
        assert_eq!(bad.summary(), "Nothing to clean (1 problem)");
    }

    #[test]
    fn run_log_keeps_the_newest_and_skips_garbage() {
        let d = dir("log");
        let p = d.join("state/cleanup-runs.jsonl");
        assert!(read_runs(&p).is_empty());
        for i in 0..(MAX_RECORDS as i64 + 5) {
            append_run(&p, &RunRecord { ts: i, freed: i as u64, summary: format!("run {i}") }).unwrap();
        }
        let all = read_runs(&p);
        assert_eq!(all.len(), MAX_RECORDS);
        assert_eq!(all.first().unwrap().ts, 5);
        assert_eq!(all.last().unwrap().summary, format!("run {}", MAX_RECORDS + 4));
        let mut text = fs::read_to_string(&p).unwrap();
        text.push_str("{not json\n\n");
        fs::write(&p, text).unwrap();
        assert_eq!(read_runs(&p).len(), MAX_RECORDS);
    }

    #[test]
    fn missing_config_is_the_default_but_a_broken_one_is_an_error() {
        let d = dir("conf");
        assert_eq!(load_config(&d.join("none.toml")).unwrap(), CleanupConfig::default());
        fs::write(d.join("bad.toml"), "providers = [\"journal\"]").unwrap();
        assert!(load_config(&d.join("bad.toml")).is_err());
        fs::write(d.join("ok.toml"), "enabled = true\ninterval = \"daily\"\n").unwrap();
        let c = load_config(&d.join("ok.toml")).unwrap();
        assert!(c.enabled && c.interval == super::super::schedule::Interval::Daily);
    }

    #[test]
    fn command_line() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["luma"]).unwrap(), None);
        assert_eq!(a(&["luma", "/some/folder"]).unwrap(), None);
        assert_eq!(a(&["luma", "--cleanup-run"]).unwrap(), Some(Args::default()));
        assert_eq!(
            a(&["luma", "--cleanup-run", "--dry-run", "--config", "/x.toml"]).unwrap(),
            Some(Args { dry_run: true, config: Some("/x.toml".into()) })
        );
        assert!(a(&["luma", "--cleanup-run", "--config"]).is_err());
        assert!(a(&["luma", "--cleanup-run", "--everything"]).is_err());
    }

    #[test]
    fn a_turned_off_schedule_does_nothing() {
        let d = dir("off");
        fs::write(d.join("c.toml"), "enabled = false\n").unwrap();
        assert_eq!(main(Args { dry_run: false, config: Some(d.join("c.toml")) }), 0);
        assert_eq!(main(Args { dry_run: false, config: Some(d.join("missing-dir/x.toml")) }), 0);
        fs::write(d.join("bad.toml"), "nope").unwrap();
        assert_eq!(main(Args { dry_run: false, config: Some(d.join("bad.toml")) }), 2);
    }
}
