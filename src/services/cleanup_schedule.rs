//! Installing and removing the systemd user timer for scheduled cleanup, and
//! reading back what the unattended runs recorded.
//!
//! Only units that start with Luma's marker are ever overwritten or deleted:
//! a unit with the same name written by someone else is left alone.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use crate::core::diskmgr::{
    CmdSpec, ToolSet,
    exec::Runner,
    headless::{self, RunRecord},
    schedule::{self, CleanupConfig, Interval, SERVICE_NAME, TIMER_NAME, TimerInfo},
};

#[derive(Clone, Debug)]
pub struct Paths {
    pub unit_dir: PathBuf,
    pub config: PathBuf,
    pub runs: PathBuf,
}

impl Paths {
    pub fn detect() -> Self {
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
        Self { unit_dir: cfg.join("systemd/user"), config: headless::config_path(), runs: headless::runs_path() }
    }

    fn service(&self) -> PathBuf {
        self.unit_dir.join(SERVICE_NAME)
    }

    fn timer(&self) -> PathBuf {
        self.unit_dir.join(TIMER_NAME)
    }
}

/// A systemd user session is running and `systemctl` can reach it. Without
/// one (Alpine, Void, containers) the Schedule page is not shown.
pub fn available(tools: &ToolSet) -> bool {
    tools.has("systemctl")
        && std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| Path::new(&d).join("systemd").exists())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitState {
    NotInstalled,
    Installed {
        /// The timer file's schedule, if it could be read.
        interval: Option<Interval>,
        /// The service runs a different Luma than this one (moved or reinstalled).
        stale_exe: Option<String>,
    },
    /// A unit with our name exists but was not made by Luma.
    Foreign(&'static str),
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok()
}

pub fn status(paths: &Paths, exe: &Path) -> UnitState {
    let (svc, tim) = (read(&paths.service()), read(&paths.timer()));
    for (name, text) in [(SERVICE_NAME, &svc), (TIMER_NAME, &tim)] {
        if text.as_deref().is_some_and(|t| !schedule::is_managed(t)) {
            return UnitState::Foreign(name);
        }
    }
    match (svc, tim) {
        (None, None) => UnitState::NotInstalled,
        (svc, tim) => UnitState::Installed {
            interval: tim.as_deref().and_then(schedule::timer_interval),
            stale_exe: svc
                .as_deref()
                .and_then(schedule::service_exe)
                .filter(|e| Path::new(e) != exe)
                .or_else(|| svc.is_none().then(|| "missing".to_string())),
        },
    }
}

fn systemctl(runner: &dyn Runner, tools: &ToolSet, args: &[&str]) -> Result<String, String> {
    let bin = tools.path("systemctl").ok_or("systemctl is not installed.")?;
    let mut argv = vec![bin.to_string_lossy().into_owned(), "--user".into()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let out = runner.run(&CmdSpec::user(argv), &AtomicBool::new(false)).map_err(|e| e.to_string())?;
    if out.success() {
        Ok(out.stdout)
    } else {
        let why = out.stderr.trim();
        Err(format!("systemctl {} failed. {}", args.join(" "), why))
    }
}

pub fn timer_info(runner: &dyn Runner, tools: &ToolSet) -> TimerInfo {
    let prop = format!("--property={}", schedule::SHOW_PROPERTIES);
    systemctl(runner, tools, &["show", TIMER_NAME, &prop]).map(|o| schedule::parse_show(&o)).unwrap_or_default()
}

pub fn load(paths: &Paths) -> Result<CleanupConfig, String> {
    headless::load_config(&paths.config)
}

pub fn save(paths: &Paths, cfg: &CleanupConfig) -> Result<(), String> {
    cfg.validate()?;
    headless::write_atomic(&paths.config, &cfg.to_toml()).map_err(|e| format!("Could not save the settings: {e}"))
}

/// The unit files as they would be written, for the preview.
pub fn unit_texts(cfg: &CleanupConfig, exe: &Path) -> (String, String) {
    (schedule::service_text(exe), schedule::timer_text(cfg.interval))
}

/// Save the settings, write both units and start the timer.
pub fn install(
    paths: &Paths,
    runner: &dyn Runner,
    tools: &ToolSet,
    cfg: &CleanupConfig,
    exe: &Path,
) -> Result<(), String> {
    cfg.validate()?;
    if let UnitState::Foreign(name) = status(paths, exe) {
        return Err(format!("A unit named {name} already exists and was not made by Luma, so it was left alone."));
    }
    save(paths, cfg)?;
    let (svc, tim) = unit_texts(cfg, exe);
    let write = |p: PathBuf, t: &str| {
        headless::write_atomic(&p, t).map_err(|e| format!("Could not write {}: {e}", p.display()))
    };
    write(paths.service(), &svc)?;
    write(paths.timer(), &tim)?;
    let started = systemctl(runner, tools, &["daemon-reload"])
        .and_then(|_| systemctl(runner, tools, &["enable", "--now", TIMER_NAME]));
    if let Err(e) = started {
        // Do not leave units behind that nothing is running.
        let _ = remove_files(paths);
        let _ = systemctl(runner, tools, &["daemon-reload"]);
        return Err(e);
    }
    Ok(())
}

/// Delete our own unit files; foreign ones stay.
fn remove_files(paths: &Paths) -> Result<(), String> {
    for p in [paths.timer(), paths.service()] {
        match read(&p) {
            Some(t) if schedule::is_managed(&t) => {
                fs::remove_file(&p).map_err(|e| format!("Could not remove {}: {e}", p.display()))?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn remove(paths: &Paths, runner: &dyn Runner, tools: &ToolSet) -> Result<(), String> {
    if let UnitState::Foreign(name) = status(paths, Path::new("")) {
        return Err(format!("The unit {name} was not made by Luma, so it was left alone."));
    }
    // Not loaded is fine: there is nothing to stop.
    let _ = systemctl(runner, tools, &["disable", "--now", TIMER_NAME]);
    remove_files(paths)?;
    systemctl(runner, tools, &["daemon-reload"]).map(|_| ())
}

/// What unattended runs recorded, for [`crate::database::Database::import_scheduled_runs`].
pub fn recorded_runs(paths: &Paths) -> Vec<(i64, i64, String)> {
    headless::read_runs(&paths.runs)
        .into_iter()
        .map(|RunRecord { ts, freed, summary }| (ts, freed.min(i64::MAX as u64) as i64, summary))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diskmgr::exec::{CmdOutput, FakeRunner};
    use std::os::unix::fs::PermissionsExt;

    fn setup(name: &str) -> (Paths, ToolSet) {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-sched").join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::write(d.join("bin/systemctl"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("bin/systemctl"), fs::Permissions::from_mode(0o755)).unwrap();
        let tools = ToolSet::detect_in(&[d.join("bin")], &["systemctl"]);
        let paths =
            Paths { unit_dir: d.join("units"), config: d.join("cfg/cleanup.toml"), runs: d.join("state/runs.jsonl") };
        (paths, tools)
    }

    fn cfg() -> CleanupConfig {
        CleanupConfig { enabled: true, interval: Interval::Daily, ..Default::default() }
    }

    fn verbs(r: &FakeRunner) -> Vec<String> {
        r.calls().iter().map(|c| c.argv[2..].join(" ")).collect()
    }

    #[test]
    fn install_writes_units_and_starts_the_timer() {
        let (p, t) = setup("install");
        let r = FakeRunner::ok();
        let exe = Path::new("/usr/bin/luma");
        assert_eq!(status(&p, exe), UnitState::NotInstalled);
        install(&p, &r, &t, &cfg(), exe).unwrap();
        assert_eq!(verbs(&r), ["daemon-reload", "enable --now luma-cleanup.timer"]);
        assert!(r.calls().iter().all(|c| !c.privileged && c.argv[1] == "--user"));
        assert_eq!(status(&p, exe), UnitState::Installed { interval: Some(Interval::Daily), stale_exe: None });
        assert_eq!(load(&p).unwrap(), cfg(), "the settings were saved too");
        assert!(
            fs::read_to_string(p.unit_dir.join(SERVICE_NAME))
                .unwrap()
                .contains("ExecStart=/usr/bin/luma --cleanup-run")
        );
    }

    #[test]
    fn a_moved_install_is_noticed_and_fixed_by_installing_again() {
        let (p, t) = setup("stale");
        let r = FakeRunner::ok();
        install(&p, &r, &t, &cfg(), Path::new("/old/luma")).unwrap();
        let now = Path::new("/usr/bin/luma");
        assert_eq!(
            status(&p, now),
            UnitState::Installed { interval: Some(Interval::Daily), stale_exe: Some("/old/luma".into()) }
        );
        install(&p, &r, &t, &CleanupConfig { interval: Interval::Weekly, ..cfg() }, now).unwrap();
        assert_eq!(status(&p, now), UnitState::Installed { interval: Some(Interval::Weekly), stale_exe: None });
    }

    #[test]
    fn units_made_by_someone_else_are_never_touched() {
        let (p, t) = setup("foreign");
        fs::create_dir_all(&p.unit_dir).unwrap();
        let mine = "[Unit]\nDescription=not luma's\n";
        fs::write(p.unit_dir.join(TIMER_NAME), mine).unwrap();
        let r = FakeRunner::ok();
        let exe = Path::new("/usr/bin/luma");
        assert_eq!(status(&p, exe), UnitState::Foreign(TIMER_NAME));
        assert!(install(&p, &r, &t, &cfg(), exe).unwrap_err().contains("not made by Luma"));
        assert!(remove(&p, &r, &t).is_err());
        assert_eq!(fs::read_to_string(p.unit_dir.join(TIMER_NAME)).unwrap(), mine);
        assert!(!p.unit_dir.join(SERVICE_NAME).exists());
        assert!(r.calls().is_empty(), "nothing was run");
        assert!(!p.config.exists(), "and nothing was saved");
    }

    #[test]
    fn a_failed_start_leaves_no_units_behind() {
        let (p, t) = setup("fail");
        let r = FakeRunner {
            calls: Default::default(),
            reply: Box::new(|c| {
                let bad = c.argv.iter().any(|a| a == "enable");
                Ok(CmdOutput {
                    code: Some(i32::from(bad)),
                    stderr: if bad { "Failed to connect to bus".into() } else { String::new() },
                    ..Default::default()
                })
            }),
        };
        let err = install(&p, &r, &t, &cfg(), Path::new("/usr/bin/luma")).unwrap_err();
        assert!(err.contains("Failed to connect to bus"), "{err}");
        assert_eq!(status(&p, Path::new("/usr/bin/luma")), UnitState::NotInstalled);
    }

    #[test]
    fn remove_stops_the_timer_and_deletes_our_units() {
        let (p, t) = setup("remove");
        let r = FakeRunner::ok();
        let exe = Path::new("/usr/bin/luma");
        install(&p, &r, &t, &cfg(), exe).unwrap();
        let r2 = FakeRunner::ok();
        remove(&p, &r2, &t).unwrap();
        assert_eq!(verbs(&r2), ["disable --now luma-cleanup.timer", "daemon-reload"]);
        assert_eq!(status(&p, exe), UnitState::NotInstalled);
        // Removing when nothing is installed is fine too.
        remove(&p, &FakeRunner::ok(), &t).unwrap();
    }

    #[test]
    fn invalid_settings_are_refused_before_anything_is_written() {
        let (p, t) = setup("invalid");
        let r = FakeRunner::ok();
        let bad = CleanupConfig { providers: vec!["journal".into()], ..cfg() };
        assert!(install(&p, &r, &t, &bad, Path::new("/usr/bin/luma")).is_err());
        assert!(save(&p, &bad).is_err());
        assert!(!p.unit_dir.exists() && !p.config.exists());
    }

    #[test]
    fn reads_what_unattended_runs_recorded() {
        let (p, _) = setup("runs");
        assert!(recorded_runs(&p).is_empty());
        headless::append_run(&p.runs, &RunRecord { ts: 5, freed: 99, summary: "x".into() }).unwrap();
        assert_eq!(recorded_runs(&p), [(5, 99, "x".to_string())]);
    }
}
