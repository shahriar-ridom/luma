//! Cleanups that need administrator rights, plus suggest-only advice.
//!
//! These never delete anything themselves. A root suggestion carries one
//! fixed command from [`RootAction`]; the interactive pkexec runner is the only
//! thing that can run it. Where the tool or pkexec is missing, or the job is
//! too risky to automate (removing packages), only a command to copy is given.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime},
};

use crate::core::{
    diskmgr::{
        Action, CleanupProvider, CmdSpec, Env, Item, Outcome, Plan, Risk, Scope, Suggestion,
        distro::PackageManager,
        privileged::{self, COREDUMP_DAYS, COREDUMP_DIR, RootAction},
        safety::PathPolicy,
    },
    metadata::dir_stats,
};

/// Journals above this size are offered for trimming; it is also the target.
const JOURNAL_KEEP_MB: u32 = 500;

fn ready(env: &Env, action: RootAction) -> Option<CmdSpec> {
    if env.tools.has("pkexec") { action.spec(&env.tools) } else { None }
}

/// The plan for a one-command suggestion, rebuilt from the environment rather
/// than trusted from the selection.
fn command_plan(selection: &[Item], spec: Option<CmdSpec>) -> Plan {
    match spec {
        Some(s) if selection.iter().any(|i| i.selected) => Plan { actions: vec![Action::Exec(s)] },
        _ => Plan::default(),
    }
}

/// Runs commands only: any delete in a plan is refused (no allowed folders).
fn run_commands(plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
    super::execute(plan, env, cancel, progress, &PathPolicy::new(vec![], Duration::ZERO), false)
}

fn single(label: String, bytes: u64) -> Vec<Item> {
    vec![Item { label, path: None, bytes, selected: false }]
}

fn root_or_hint(
    id: &str,
    title: &str,
    detail: String,
    est: Option<u64>,
    items: Vec<Item>,
    spec: Option<CmdSpec>,
    fallback: Vec<String>,
) -> Vec<Suggestion> {
    vec![match spec {
        Some(_) => Suggestion {
            id: id.into(),
            title: title.into(),
            detail,
            est_bytes: est,
            risk: Risk::Review,
            scope: Scope::Root,
            items,
            hint: None,
        },
        None => Suggestion {
            id: id.into(),
            title: title.into(),
            detail,
            est_bytes: est,
            risk: Risk::SuggestOnly,
            scope: Scope::User,
            items: vec![],
            hint: Some(fallback),
        },
    }]
}

fn sudo(prefix: &str, args: &[&str]) -> Vec<String> {
    ["sudo", prefix].iter().chain(args).map(|s| s.to_string()).collect()
}

// ---- package manager download cache ---------------------------------------

pub struct PkgCache {
    pub min_bytes: u64,
}

impl Default for PkgCache {
    fn default() -> Self {
        Self { min_bytes: 50 << 20 }
    }
}

fn cache_dir(env: &Env, pm: PackageManager) -> Option<PathBuf> {
    let rel = match pm {
        PackageManager::Pacman => "var/cache/pacman/pkg",
        PackageManager::Apt => "var/cache/apt/archives",
        PackageManager::Dnf => "var/cache/dnf",
        PackageManager::Zypper => "var/cache/zypp/packages",
        PackageManager::Apk => "var/cache/apk",
        PackageManager::Xbps => "var/cache/xbps",
        PackageManager::Portage | PackageManager::Unknown => return None,
    };
    Some(env.sys.join(rel))
}

impl CleanupProvider for PkgCache {
    fn id(&self) -> &'static str {
        "pkg-cache"
    }

    fn available(&self, env: &Env) -> bool {
        cache_dir(env, env.distro.pm).is_some_and(|d| d.is_dir())
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let pm = env.distro.pm;
        let Some(dir) = cache_dir(env, pm) else { return vec![] };
        let bytes = dir_stats(&dir, cancel).bytes;
        if bytes < self.min_bytes {
            return vec![];
        }
        let spec = ready(env, RootAction::PkgCacheClean(pm));
        let (prog, args) = privileged::pkg_command(pm).unwrap_or(("", &[]));
        // pacman's own tool is the fallback when paccache is not installed.
        let fallback = if pm == PackageManager::Pacman { sudo("pacman", &["-Sc"]) } else { sudo(prog, args) };
        let keeps = if pm == PackageManager::Pacman { " The latest 3 versions of each package are kept." } else { "" };
        root_or_hint(
            "pkg-cache",
            "Package download cache",
            format!("Downloaded {} packages that are already installed.{keeps}", pm.name()),
            Some(bytes),
            single(format!("{} cache ({})", pm.name(), dir.display()), bytes),
            spec,
            fallback,
        )
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        command_plan(selection, ready(env, RootAction::PkgCacheClean(env.distro.pm)))
    }

    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
        run_commands(plan, env, cancel, progress)
    }
}

// ---- systemd journal --------------------------------------------------------

pub struct Journal;

impl CleanupProvider for Journal {
    fn id(&self) -> &'static str {
        "journal"
    }

    fn available(&self, env: &Env) -> bool {
        env.sys.join("var/log/journal").is_dir()
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let bytes = dir_stats(&env.sys.join("var/log/journal"), cancel).bytes;
        let keep = u64::from(JOURNAL_KEEP_MB) << 20;
        if bytes <= keep {
            return vec![];
        }
        let action = RootAction::JournalVacuum { max_mb: JOURNAL_KEEP_MB };
        let spec = ready(env, action);
        let fallback = vec!["sudo".into(), "journalctl".into(), format!("--vacuum-size={JOURNAL_KEEP_MB}M")];
        root_or_hint(
            "journal",
            "System journal",
            format!("Old log records. The newest {JOURNAL_KEEP_MB} MB are kept."),
            Some(bytes - keep),
            single("systemd journal".into(), bytes - keep),
            spec,
            fallback,
        )
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        command_plan(selection, ready(env, RootAction::JournalVacuum { max_mb: JOURNAL_KEEP_MB }))
    }

    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
        run_commands(plan, env, cancel, progress)
    }
}

// ---- crash dumps -------------------------------------------------------------

pub struct Coredumps;

fn coredump_dir(env: &Env) -> PathBuf {
    env.sys.join(COREDUMP_DIR.trim_start_matches('/'))
}

impl CleanupProvider for Coredumps {
    fn id(&self) -> &'static str {
        "coredumps"
    }

    fn available(&self, env: &Env) -> bool {
        coredump_dir(env).is_dir()
    }

    fn scan(&self, env: &Env, _cancel: &AtomicBool) -> Vec<Suggestion> {
        let cutoff = Duration::from_secs(u64::from(COREDUMP_DAYS) * 86400);
        let now = SystemTime::now();
        let bytes: u64 = fs::read_dir(coredump_dir(env))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fs::symlink_metadata(e.path()).ok())
            .filter(|m| m.is_file() && m.modified().ok().and_then(|t| now.duration_since(t).ok()) > Some(cutoff))
            .map(|m| m.len())
            .sum();
        if bytes == 0 {
            return vec![];
        }
        let spec = ready(env, RootAction::CoredumpsPrune);
        let fallback = vec![
            "sudo".into(),
            "find".into(),
            COREDUMP_DIR.into(),
            "-xdev".into(),
            "-type".into(),
            "f".into(),
            "-mtime".into(),
            format!("+{COREDUMP_DAYS}"),
            "-delete".into(),
        ];
        root_or_hint(
            "coredumps",
            "Old crash dumps",
            format!("Program crash dumps older than {COREDUMP_DAYS} days."),
            Some(bytes),
            single("crash dumps".into(), bytes),
            spec,
            fallback,
        )
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        command_plan(selection, ready(env, RootAction::CoredumpsPrune))
    }

    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
        run_commands(plan, env, cancel, progress)
    }
}

// ---- orphaned packages (advice only) ----------------------------------------

pub struct Orphans;

/// `pacman -Qdtq`: one package name per line.
pub fn parse_pacman_orphans(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(char::is_whitespace))
        .map(String::from)
        .collect()
}

/// `apt-get -s autoremove`: lines such as `Remv libfoo [1.2-3]`.
pub fn parse_apt_autoremove(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("Remv "))
        .filter_map(|r| r.split_whitespace().next())
        .map(String::from)
        .collect()
}

impl CleanupProvider for Orphans {
    fn id(&self) -> &'static str {
        "orphans"
    }

    fn available(&self, env: &Env) -> bool {
        match env.distro.pm {
            PackageManager::Pacman => env.tools.has("pacman"),
            PackageManager::Apt => env.tools.has("apt-get"),
            _ => false,
        }
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let (argv, parse, remove): (&[&str], fn(&str) -> Vec<String>, Vec<String>) = match env.distro.pm {
            PackageManager::Pacman => (&["pacman", "-Qdtq"], parse_pacman_orphans, sudo("pacman", &["-Rns"])),
            PackageManager::Apt => {
                (&["apt-get", "-s", "autoremove"], parse_apt_autoremove, sudo("apt-get", &["autoremove"]))
            }
            _ => return vec![],
        };
        // `pacman -Qdtq` exits 1 when there are none, so only the output counts.
        let Ok(out) = env.runner.run(&CmdSpec::user(argv.iter().copied()), cancel) else { return vec![] };
        let names = parse(&out.stdout);
        if names.is_empty() {
            return vec![];
        }
        let mut hint = remove;
        if env.distro.pm == PackageManager::Pacman {
            hint.extend(names.iter().take(100).cloned());
        }
        let preview = names.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        let more = if names.len() > 8 { format!(" and {} more", names.len() - 8) } else { String::new() };
        vec![Suggestion {
            id: "orphans".into(),
            title: format!("{} unused packages", names.len()),
            detail: format!(
                "Installed only as dependencies that nothing needs now: {preview}{more}. Luma never removes packages; run the command yourself after checking the list."
            ),
            est_bytes: None,
            risk: Risk::SuggestOnly,
            scope: Scope::User,
            items: vec![],
            hint: Some(hint),
        }]
    }

    fn plan(&self, _selection: &[Item], _env: &Env) -> Plan {
        Plan::default()
    }

    fn run(&self, _plan: &Plan, _env: &Env, _cancel: &AtomicBool, _progress: &dyn Fn(u64)) -> Outcome {
        Outcome::default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_dir;
    use super::*;
    use crate::core::diskmgr::{
        ToolSet,
        distro::parse_os_release,
        exec::{CmdOutput, FakeRunner},
    };
    use std::os::unix::fs::PermissionsExt;

    fn tools(home: &std::path::Path, progs: &[&str]) -> ToolSet {
        let d = home.join("bin");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        for p in progs {
            fs::write(d.join(p), "#!/bin/sh\n").unwrap();
            fs::set_permissions(d.join(p), fs::Permissions::from_mode(0o755)).unwrap();
        }
        ToolSet::detect_in(&[d], crate::core::diskmgr::tools::KNOWN)
    }

    fn env<'a>(home: &std::path::Path, os: &str, progs: &[&str], r: &'a FakeRunner) -> Env<'a> {
        let mut e = Env::rooted(home.to_path_buf(), r);
        e.distro = parse_os_release(os);
        e.tools = tools(home, progs);
        e
    }

    fn fill(dir: PathBuf, len: usize) {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("blob"), vec![0u8; len]).unwrap();
    }

    #[test]
    fn parsers() {
        assert_eq!(parse_pacman_orphans("libfoo\nlibbar\n\n"), vec!["libfoo", "libbar"]);
        assert!(parse_pacman_orphans("error: no targets\n").is_empty());
        let apt = "Reading package lists...\nRemv libold1 [1.0-1]\nRemv libold2 [2.0] [x]\nInst a [1]\n";
        assert_eq!(parse_apt_autoremove(apt), vec!["libold1", "libold2"]);
    }

    #[test]
    fn package_cache_is_a_root_suggestion_with_nothing_preselected() {
        let home = test_dir("root-pkg");
        let r = FakeRunner::ok();
        let e = env(&home, "ID=arch", &["paccache", "pkexec"], &r);
        fill(e.sys.join("var/cache/pacman/pkg"), 4096);
        let p = PkgCache { min_bytes: 1000 };
        let s = &p.scan(&e, &AtomicBool::new(false))[0];
        assert_eq!((s.scope, s.risk), (Scope::Root, Risk::Review));
        assert!(s.items.iter().all(|i| !i.selected), "root work is never pre-selected");

        assert!(p.plan(&s.items, &e).actions.is_empty(), "unselected means no plan");
        let mut items = s.items.clone();
        items[0].selected = true;
        let plan = p.plan(&items, &e);
        let Action::Exec(spec) = &plan.actions[0] else { panic!("expected a command") };
        assert!(spec.privileged);
        assert!(spec.argv[0].ends_with("/paccache") && spec.argv[1] == "-r");
        assert!(r.calls().is_empty(), "planning runs nothing");
    }

    #[test]
    fn plan_ignores_what_the_selection_claims() {
        let home = test_dir("root-forge");
        let r = FakeRunner::ok();
        let e = env(&home, "ID=debian", &["apt-get", "pkexec"], &r);
        let forged = vec![Item { label: "rm -rf /".into(), path: Some("/".into()), bytes: 0, selected: true }];
        let plan = PkgCache::default().plan(&forged, &e);
        assert_eq!(plan.actions.len(), 1);
        let Action::Exec(spec) = &plan.actions[0] else { panic!() };
        assert_eq!(&spec.argv[1..], ["clean"], "the command comes from RootAction, not the item");
    }

    #[test]
    fn without_pkexec_or_the_tool_only_a_command_is_offered() {
        let home = test_dir("root-hint");
        let r = FakeRunner::ok();
        let e = env(&home, "ID=arch", &["pkexec"], &r); // no paccache
        fill(e.sys.join("var/cache/pacman/pkg"), 4096);
        let s = &PkgCache { min_bytes: 1000 }.scan(&e, &AtomicBool::new(false))[0];
        assert_eq!((s.scope, s.risk), (Scope::User, Risk::SuggestOnly));
        assert_eq!(s.hint.as_deref(), Some(&["sudo", "pacman", "-Sc"].map(String::from)[..]));
        assert!(s.items.is_empty());
        assert!(PkgCache::default().plan(&[], &e).actions.is_empty());

        let e = env(&home, "ID=arch", &["paccache"], &r); // no pkexec
        let s = &PkgCache { min_bytes: 1000 }.scan(&e, &AtomicBool::new(false))[0];
        assert_eq!(s.risk, Risk::SuggestOnly);
    }

    #[test]
    fn small_caches_and_unknown_distros_are_ignored() {
        let home = test_dir("root-small");
        let r = FakeRunner::ok();
        let e = env(&home, "ID=arch", &["paccache", "pkexec"], &r);
        fill(e.sys.join("var/cache/pacman/pkg"), 10);
        assert!(PkgCache::default().scan(&e, &AtomicBool::new(false)).is_empty());
        let e = env(&home, "ID=templeos", &["pkexec"], &r);
        assert!(!PkgCache::default().available(&e));
        assert!(PkgCache::default().scan(&e, &AtomicBool::new(false)).is_empty());
    }

    #[test]
    fn journal_offers_only_the_excess() {
        let home = test_dir("root-journal");
        let r = FakeRunner::ok();
        let e = env(&home, "ID=arch", &["journalctl", "pkexec"], &r);
        let dir = e.sys.join("var/log/journal");
        fill(dir.clone(), 1 << 20);
        assert!(Journal.scan(&e, &AtomicBool::new(false)).is_empty(), "under the limit");
        let f = fs::File::create(dir.join("big")).unwrap();
        f.set_len(600 << 20).unwrap();
        let s = &Journal.scan(&e, &AtomicBool::new(false))[0];
        assert_eq!(s.scope, Scope::Root);
        assert_eq!(s.est_bytes, Some((600 << 20) + (1 << 20) - (500 << 20)));
        let mut items = s.items.clone();
        items[0].selected = true;
        let Action::Exec(spec) = &Journal.plan(&items, &e).actions[0] else { panic!() };
        assert_eq!(spec.argv[1], "--vacuum-size=500M");
    }

    #[test]
    fn coredumps_count_only_old_regular_files() {
        let home = test_dir("root-core");
        let r = FakeRunner::ok();
        let e = env(&home, "ID=arch", &["find", "pkexec"], &r);
        let dir = coredump_dir(&e);
        fill(dir.clone(), 100); // fresh
        let old = dir.join("old.zst");
        fs::write(&old, vec![0u8; 700]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(30 * 86400);
        fs::File::open(&old).unwrap().set_times(fs::FileTimes::new().set_modified(t)).unwrap();
        let s = &Coredumps.scan(&e, &AtomicBool::new(false))[0];
        assert_eq!(s.est_bytes, Some(700));
        assert_eq!(s.scope, Scope::Root);
    }

    #[test]
    fn orphans_are_advice_only_and_never_planned() {
        let home = test_dir("root-orphans");
        let r = FakeRunner {
            calls: Default::default(),
            reply: Box::new(|c| {
                assert!(!c.privileged);
                Ok(CmdOutput { code: Some(0), stdout: "libfoo\nlibbar\n".into(), stderr: String::new() })
            }),
        };
        let e = env(&home, "ID=arch", &["pacman"], &r);
        let s = &Orphans.scan(&e, &AtomicBool::new(false))[0];
        assert_eq!((s.risk, s.scope), (Risk::SuggestOnly, Scope::User));
        assert_eq!(s.hint.as_deref(), Some(&["sudo", "pacman", "-Rns", "libfoo", "libbar"].map(String::from)[..]));
        assert!(Orphans.plan(&[], &e).actions.is_empty());
        let forged = Plan { actions: vec![Action::RemovePath("/etc".into())] };
        let out = Orphans.run(&forged, &e, &AtomicBool::new(false), &|_| {});
        assert_eq!((out.freed, out.errors.len()), (0, 0), "advice-only providers do nothing");
    }

    #[test]
    fn no_orphans_means_no_suggestion() {
        let home = test_dir("root-noorphans");
        let r = FakeRunner {
            calls: Default::default(),
            reply: Box::new(|_| Ok(CmdOutput { code: Some(1), ..Default::default() })),
        };
        let e = env(&home, "ID=arch", &["pacman"], &r);
        assert!(Orphans.scan(&e, &AtomicBool::new(false)).is_empty());
    }

    #[test]
    fn root_run_refuses_file_deletes() {
        let home = test_dir("root-nodelete");
        fs::write(home.join("keep"), "x").unwrap();
        let r = FakeRunner::ok();
        let e = env(&home, "ID=arch", &["paccache", "pkexec"], &r);
        let plan = Plan { actions: vec![Action::RemovePath(home.join("keep"))] };
        let out = PkgCache::default().run(&plan, &e, &AtomicBool::new(false), &|_| {});
        assert_eq!(out.errors.len(), 1);
        assert!(home.join("keep").exists());
    }
}
