//! Runs disk manager work off the main thread: scanning for things to clean,
//! planning a selection, and carrying the plan out. The UI only receives
//! events; all logic lives in `core::diskmgr`.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_channel::Receiver;

use crate::core::diskmgr::{
    CmdSpec, Env, Item, Outcome, Plan, Suggestion, ToolSet,
    devices::{self, Disk, Volume},
    exec::{CmdOutput, Runner, StdRunner},
    fscheck::{self, CheckResult},
    privileged::RootAction,
    providers::interactive_providers,
    smart::{self, SmartReport},
};

/// Runs ordinary commands as the user, and administrator commands through
/// pkexec, but only ones that are exactly a [`RootAction`]. Anything else
/// marked privileged is refused, so a bug elsewhere cannot run arbitrary
/// commands as root.
pub struct PkexecRunner {
    inner: StdRunner,
    tools: ToolSet,
}

impl PkexecRunner {
    pub fn new(tools: ToolSet) -> Self {
        // The timeout also covers the time spent at the password prompt.
        Self::with_timeout(tools, std::time::Duration::from_secs(900))
    }

    pub fn with_timeout(tools: ToolSet, timeout: std::time::Duration) -> Self {
        Self { inner: StdRunner { timeout }, tools }
    }
}

impl Runner for PkexecRunner {
    fn run(&self, cmd: &CmdSpec, cancel: &AtomicBool) -> std::io::Result<CmdOutput> {
        use std::io::{Error, ErrorKind};
        if !cmd.privileged {
            return self.inner.run(cmd, cancel);
        }
        if RootAction::from_spec(cmd, &self.tools).is_none() {
            return Err(Error::new(ErrorKind::PermissionDenied, "not an allowed administrator action"));
        }
        let pkexec =
            self.tools.path("pkexec").ok_or_else(|| Error::new(ErrorKind::NotFound, "pkexec is not installed"))?;
        let mut argv = vec![pkexec.to_string_lossy().into_owned()];
        argv.extend(cmd.argv.iter().cloned());
        self.inner.run(&CmdSpec::user(argv), cancel)
    }
}

pub enum ScanEvent {
    /// Provider id, as a scan of it begins.
    Working(&'static str),
    Found(Suggestion),
    Done,
}

pub enum CleanEvent {
    /// Bytes removed or trashed so far.
    Progress(u64),
    Finished(CleanReport),
}

#[derive(Debug, Default)]
pub struct CleanReport {
    pub outcome: Outcome,
    pub free_before: Option<u64>,
    pub free_after: Option<u64>,
}

impl CleanReport {
    /// How much the free space of the home drive really changed. Differs from
    /// `outcome.freed` on filesystems with snapshots or shared blocks.
    pub fn free_delta(&self) -> Option<i64> {
        Some(self.free_after? as i64 - self.free_before? as i64)
    }
}

/// Free bytes for unprivileged users on the filesystem holding `path`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let st = nix::sys::statvfs::statvfs(path).ok()?;
    Some(st.blocks_available() * st.fragment_size())
}

pub fn scan(env: &Env, heavy: bool, cancel: &AtomicBool, mut emit: impl FnMut(ScanEvent)) {
    for p in interactive_providers(heavy) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !p.available(env) {
            continue;
        }
        emit(ScanEvent::Working(p.id()));
        for s in p.scan(env, cancel) {
            emit(ScanEvent::Found(s));
        }
    }
    emit(ScanEvent::Done);
}

/// One plan per provider that has something selected. Always a dry run.
pub fn plan(env: &Env, selection: &[(String, Vec<Item>)]) -> Vec<(String, Plan)> {
    let providers = interactive_providers(true);
    selection
        .iter()
        .filter_map(|(id, items)| {
            let p = providers.iter().find(|p| p.id() == id)?;
            let plan = p.plan(items, env);
            (!plan.actions.is_empty()).then(|| (id.clone(), plan))
        })
        .collect()
}

pub fn clean(env: &Env, plans: &[(String, Plan)], cancel: &AtomicBool, progress: &dyn Fn(u64)) -> CleanReport {
    let providers = interactive_providers(true);
    let free_before = free_bytes(&env.home);
    let mut outcome = Outcome::default();
    for (id, plan) in plans {
        let Some(p) = providers.iter().find(|p| p.id() == id) else { continue };
        let base = outcome.freed + outcome.trashed;
        outcome.merge(p.run(plan, env, cancel, &|n| progress(base + n)));
        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    CleanReport { outcome, free_before, free_after: free_bytes(&env.home) }
}

pub fn spawn_scan(heavy: bool) -> (Arc<AtomicBool>, Receiver<ScanEvent>) {
    let (tx, rx) = async_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("fm-diskmgr-scan".into())
        .spawn(move || {
            let runner = StdRunner::default();
            let env = Env::detect(&runner);
            scan(&env, heavy, &c, |ev| {
                // Nobody is listening any more: stop scanning.
                if tx.send_blocking(ev).is_err() {
                    c.store(true, Ordering::Relaxed);
                }
            });
        })
        .expect("spawn disk manager scan thread");
    (cancel, rx)
}

pub fn spawn_plan(selection: Vec<(String, Vec<Item>)>) -> Receiver<Vec<(String, Plan)>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("fm-diskmgr-plan".into())
        .spawn(move || {
            let runner = StdRunner::default();
            let env = Env::detect(&runner);
            let _ = tx.send_blocking(plan(&env, &selection));
        })
        .expect("spawn disk manager plan thread");
    rx
}

pub fn spawn_clean(plans: Vec<(String, Plan)>) -> (Arc<AtomicBool>, Receiver<CleanEvent>) {
    let (tx, rx) = async_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::Builder::new()
        .name("fm-diskmgr-clean".into())
        .spawn(move || {
            let tools = ToolSet::detect();
            let runner = PkexecRunner::new(tools);
            let env = Env::detect(&runner);
            let report = clean(&env, &plans, &c, &|n| {
                let _ = tx.send_blocking(CleanEvent::Progress(n));
            });
            let _ = tx.send_blocking(CleanEvent::Finished(report));
        })
        .expect("spawn disk manager clean thread");
    (cancel, rx)
}

// ---- Health: disks, SMART and filesystem checks ---------------------------

fn run_root(
    runner: &dyn Runner,
    tools: &ToolSet,
    action: &RootAction,
    cancel: &AtomicBool,
) -> Result<CmdOutput, String> {
    let spec = action.spec(tools).ok_or("This needs a tool that is not installed.")?;
    runner.run(&spec, cancel).map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut => "It took too long and was stopped.".to_string(),
        _ => e.to_string(),
    })
}

/// pkexec answers 126 when the password prompt is dismissed and 127 when
/// authentication fails; the tool itself never ran.
fn not_authorised(out: &CmdOutput) -> bool {
    matches!(out.code, Some(126 | 127)) && out.stdout.trim().is_empty()
}

pub fn list_disks(runner: &dyn Runner, tools: &ToolSet) -> Result<Vec<Disk>, String> {
    let cmd = devices::lsblk_cmd(tools).ok_or("lsblk is not installed.")?;
    let out = runner.run(&cmd, &AtomicBool::new(false)).map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!("Could not list the drives. {}", out.stderr.trim()));
    }
    devices::parse_lsblk(&out.stdout)
}

/// Only devices that exist right now are ever handed to an administrator command.
fn find_disk(disks: &[Disk], dev: &str) -> Result<Disk, String> {
    disks.iter().find(|d| d.path == dev).cloned().ok_or_else(|| format!("{dev} is not a drive in this computer."))
}

fn find_volume(disks: &[Disk], path: &str) -> Result<Volume, String> {
    disks
        .iter()
        .flat_map(|d| &d.volumes)
        .find(|v| v.path == path)
        .cloned()
        .ok_or_else(|| format!("{path} is not a volume in this computer."))
}

pub fn read_smart(runner: &dyn Runner, tools: &ToolSet, dev: &str) -> Result<SmartReport, String> {
    find_disk(&list_disks(runner, tools)?, dev)?;
    let action = RootAction::SmartRead { dev: dev.to_string() };
    let out = run_root(runner, tools, &action, &AtomicBool::new(false))?;
    if not_authorised(&out) {
        return Err("Administrator permission was not given.".into());
    }
    // smartctl uses its exit status as a bit mask for drive trouble, so only
    // the JSON says whether it could read the drive.
    smart::parse(&out.stdout)
}

pub fn start_self_test(runner: &dyn Runner, tools: &ToolSet, dev: &str) -> Result<(), String> {
    find_disk(&list_disks(runner, tools)?, dev)?;
    let action = RootAction::SmartSelfTest { dev: dev.to_string() };
    let out = run_root(runner, tools, &action, &AtomicBool::new(false))?;
    if not_authorised(&out) {
        return Err("Administrator permission was not given.".into());
    }
    // Exit bit 0/1 means the command or device could not be used.
    if out.code.is_some_and(|c| c & 0b11 != 0) || out.code.is_none() {
        let why = out.stdout.lines().chain(out.stderr.lines()).find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!("The drive did not start the test. {}", why.trim()));
    }
    Ok(())
}

/// Look up the volume among the current drives and check it read-only.
pub fn run_check(runner: &dyn Runner, tools: &ToolSet, volume: &str) -> Result<CheckResult, String> {
    let vol = find_volume(&list_disks(runner, tools)?, volume)?;
    let action = fscheck::check_action(&vol)?;
    let out = run_root(runner, tools, &action, &AtomicBool::new(false))?;
    if not_authorised(&out) {
        return Err("Administrator permission was not given.".into());
    }
    fscheck::parse_result(&action, out.code, &out.stdout, &out.stderr).ok_or_else(|| "Unexpected check.".to_string())
}

fn spawn_job<T: Send + 'static>(name: &str, job: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send_blocking(job());
        })
        .expect("spawn disk manager health thread");
    rx
}

pub fn spawn_disks() -> Receiver<Result<Vec<Disk>, String>> {
    spawn_job("fm-diskmgr-disks", || list_disks(&StdRunner::default(), &ToolSet::detect()))
}

pub fn spawn_smart(dev: String) -> Receiver<Result<SmartReport, String>> {
    spawn_job("fm-diskmgr-smart", move || {
        let tools = ToolSet::detect();
        read_smart(&PkexecRunner::new(tools.clone()), &tools, &dev)
    })
}

pub fn spawn_self_test(dev: String) -> Receiver<Result<(), String>> {
    spawn_job("fm-diskmgr-selftest", move || {
        let tools = ToolSet::detect();
        start_self_test(&PkexecRunner::new(tools.clone()), &tools, &dev)
    })
}

pub fn spawn_check(volume: String) -> Receiver<Result<CheckResult, String>> {
    spawn_job("fm-diskmgr-check", move || {
        let tools = ToolSet::detect();
        // A scrub of a large drive takes hours.
        let runner = PkexecRunner::with_timeout(tools.clone(), std::time::Duration::from_secs(12 * 3600));
        run_check(&runner, &tools, &volume)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diskmgr::exec::FakeRunner;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    fn old_file(p: &Path, len: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; len]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(90 * 86400);
        fs::File::open(p).unwrap().set_times(fs::FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
    }

    #[test]
    fn scan_plan_clean_end_to_end() {
        let home = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-service");
        let _ = fs::remove_dir_all(&home);
        old_file(&home.join(".cache/app/blob"), 4000);
        old_file(&home.join(".cache/thumbnails/normal/t.png"), 100);
        let home = home.canonicalize().unwrap();
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);

        let mut found = vec![];
        let mut done = false;
        scan(&env, false, &AtomicBool::new(false), |ev| match ev {
            ScanEvent::Found(s) => found.push(s),
            ScanEvent::Done => done = true,
            ScanEvent::Working(_) => {}
        });
        assert!(done);
        let ids: Vec<_> = found.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["user-cache", "thumbnails"]);

        // Deselect the thumbnails; only the cache entry is planned.
        let selection: Vec<(String, Vec<Item>)> = found
            .into_iter()
            .map(|mut s| {
                if s.id == "thumbnails" {
                    s.items.iter_mut().for_each(|i| i.selected = false);
                }
                (s.id, s.items)
            })
            .collect();
        let plans = plan(&env, &selection);
        assert_eq!(plans.len(), 1);
        assert!(home.join(".cache/app/blob").exists(), "planning must not delete");

        let progress = std::cell::Cell::new(0);
        let report = clean(&env, &plans, &AtomicBool::new(false), &|n| progress.set(n));
        assert_eq!(report.outcome.freed, 4000);
        assert_eq!(progress.get(), 4000);
        assert!(report.outcome.errors.is_empty());
        assert!(!home.join(".cache/app/blob").exists());
        assert!(home.join(".cache/thumbnails/normal/t.png").exists());
        assert!(report.free_delta().is_some());
    }

    #[test]
    fn unknown_provider_ids_are_ignored() {
        let home = std::env::temp_dir();
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        let item = Item { label: "x".into(), path: Some("/etc/passwd".into()), bytes: 1, selected: true };
        assert!(plan(&env, &[("nope".into(), vec![item.clone()])]).is_empty());
        let rogue = vec![("nope".to_string(), Plan::default())];
        assert_eq!(clean(&env, &rogue, &AtomicBool::new(false), &|_| {}).outcome.freed, 0);
    }

    #[test]
    fn pkexec_runner_refuses_unlisted_privileged_commands() {
        use std::io::ErrorKind;
        let r = PkexecRunner::new(ToolSet::detect());
        let cancel = AtomicBool::new(false);
        let rm = CmdSpec { argv: vec!["/usr/bin/rm".into(), "-rf".into(), "/".into()], privileged: true };
        assert_eq!(r.run(&rm, &cancel).unwrap_err().kind(), ErrorKind::PermissionDenied);
        let sh = CmdSpec { argv: vec!["/bin/sh".into(), "-c".into(), "id".into()], privileged: true };
        assert_eq!(r.run(&sh, &cancel).unwrap_err().kind(), ErrorKind::PermissionDenied);
        // Unprivileged commands still run as the user.
        assert!(r.run(&CmdSpec::user(["true"]), &cancel).unwrap().success());
    }

    #[test]
    fn free_bytes_reads_a_real_filesystem() {
        assert!(free_bytes(Path::new("/")).is_some());
        assert!(free_bytes(Path::new("/definitely/not/here")).is_none());
    }

    const LSBLK: &str = r#"{"blockdevices":[
      {"path":"/dev/sda","type":"disk","size":1000,"model":"Disk A","mountpoints":[null],"children":[
        {"path":"/dev/sda1","type":"part","size":900,"fstype":"ext4","mountpoints":["/mnt/a"]},
        {"path":"/dev/sda2","type":"part","size":90,"fstype":"ext4","mountpoints":[null]}]},
      {"path":"/dev/sdb","type":"disk","size":1000,"mountpoints":[null],"children":[
        {"path":"/dev/sdb1","type":"part","size":900,"fstype":"btrfs","mountpoints":["/mnt/b"]}]}]}"#;

    fn health_tools(name: &str) -> ToolSet {
        use std::os::unix::fs::PermissionsExt;
        let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-health").join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        for p in ["lsblk", "smartctl", "fsck", "btrfs", "xfs_repair"] {
            fs::write(d.join(p), "#!/bin/sh\n").unwrap();
            fs::set_permissions(d.join(p), fs::Permissions::from_mode(0o755)).unwrap();
        }
        ToolSet::detect_in(&[d], &["lsblk", "smartctl", "fsck", "btrfs", "xfs_repair"])
    }

    /// Answers lsblk with `LSBLK` and anything else with `(code, stdout)`.
    fn fake(code: i32, stdout: &'static str) -> FakeRunner {
        FakeRunner {
            calls: Default::default(),
            reply: Box::new(move |c| {
                let list = c.argv[0].ends_with("lsblk");
                Ok(CmdOutput {
                    code: Some(if list { 0 } else { code }),
                    stdout: if list { LSBLK.into() } else { stdout.into() },
                    stderr: String::new(),
                })
            }),
        }
    }

    fn ran(r: &FakeRunner, prog: &str) -> usize {
        r.calls().iter().filter(|c| c.argv[0].ends_with(prog)).count()
    }

    #[test]
    fn smart_reads_through_the_admin_command_even_when_the_exit_status_is_set() {
        let t = health_tools("smart");
        let r = fake(4, r#"{"smart_status":{"passed":true},"model_name":"Disk A"}"#);
        let rep = read_smart(&r, &t, "/dev/sda").unwrap();
        assert_eq!(rep.model, "Disk A");
        let smart = r.calls().into_iter().find(|c| c.argv[0].ends_with("smartctl")).unwrap();
        assert!(smart.privileged);
        assert_eq!(&smart.argv[1..], ["--json=c", "-a", "/dev/sda"]);
    }

    #[test]
    fn devices_that_are_not_listed_are_refused_before_any_command_runs() {
        let t = health_tools("unlisted");
        let r = fake(0, "{}");
        for dev in ["/dev/sdz", "/dev/sda1", "/dev/sda; id"] {
            assert!(read_smart(&r, &t, dev).is_err(), "{dev}");
            assert!(start_self_test(&r, &t, dev).is_err(), "{dev}");
        }
        assert!(run_check(&r, &t, "/dev/sdz9").is_err());
        assert_eq!(ran(&r, "smartctl") + ran(&r, "fsck") + ran(&r, "btrfs"), 0);
    }

    #[test]
    fn dismissed_password_prompt_is_not_reported_as_a_bad_drive() {
        let t = health_tools("denied");
        let r = fake(126, "");
        assert_eq!(read_smart(&r, &t, "/dev/sda").unwrap_err(), "Administrator permission was not given.");
        assert!(start_self_test(&r, &t, "/dev/sda").unwrap_err().contains("permission"));
        assert!(run_check(&r, &t, "/dev/sda2").unwrap_err().contains("permission"));
    }

    #[test]
    fn self_test_failure_is_reported() {
        let t = health_tools("selftest");
        assert!(start_self_test(&fake(0, ""), &t, "/dev/sda").is_ok());
        assert!(
            start_self_test(&fake(4, "Test started"), &t, "/dev/sda").is_ok(),
            "bit 2 is about the drive, not the command"
        );
        let err = start_self_test(&fake(2, "Smartctl open device: /dev/sda failed"), &t, "/dev/sda").unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }

    #[test]
    fn checks_never_run_on_a_mounted_ext4_volume() {
        let t = health_tools("mounted");
        let r = fake(0, "");
        assert!(run_check(&r, &t, "/dev/sda1").unwrap_err().contains("Unmount"));
        assert_eq!(ran(&r, "fsck"), 0);
        // The unmounted one is checked with the read-only flag.
        let res = run_check(&r, &t, "/dev/sda2").unwrap();
        assert_eq!(res.verdict, fscheck::Verdict::Clean);
        let fsck = r.calls().into_iter().find(|c| c.argv[0].ends_with("fsck")).unwrap();
        assert_eq!(&fsck.argv[1..], ["-n", "/dev/sda2"]);
        // A mounted btrfs volume is scrubbed in place.
        run_check(&fake(0, "Error summary:    no errors found\n"), &t, "/dev/sdb1").unwrap();
    }
}
