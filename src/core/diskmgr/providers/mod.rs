//! Cleanup providers that work as the current user. Each one turns a scan
//! into [`Suggestion`]s, a selection into a [`Plan`], and a plan into deletes.

mod aged;
mod dupes;
mod large;
mod root;
mod trash;

use std::{
    fs,
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

use gtk::gio::prelude::FileExt;

use super::{Action, CleanupProvider, Env, Outcome, Plan, safety::PathPolicy};

pub use aged::AgedFiles;
pub use dupes::DuplicateFiles;
pub use large::LargeFiles;
pub use root::{Coredumps, Journal, Orphans, PkgCache};
pub use trash::TrashProvider;

/// Everything the dialog offers: the user-level providers, then the ones that
/// need administrator rights or only give advice. Scheduled runs never use
/// this list.
pub fn interactive_providers(heavy: bool) -> Vec<Box<dyn CleanupProvider>> {
    let mut v = user_providers(heavy);
    v.push(Box::new(PkgCache::default()));
    v.push(Box::new(Journal));
    v.push(Box::new(Coredumps));
    v.push(Box::new(Orphans));
    v
}

/// Providers that need no administrator rights. `heavy` adds the ones that
/// walk the whole home folder, which only run when asked for.
pub fn user_providers(heavy: bool) -> Vec<Box<dyn CleanupProvider>> {
    let mut v: Vec<Box<dyn CleanupProvider>> = vec![
        Box::new(TrashProvider),
        Box::new(AgedFiles::user_cache()),
        Box::new(AgedFiles::thumbnails()),
        Box::new(AgedFiles::x11_logs()),
    ];
    if heavy {
        v.push(Box::new(LargeFiles::default()));
        v.push(Box::new(DuplicateFiles::default()));
    }
    v
}

/// Carry out `plan`. Every delete is re-checked against `policy` at the
/// moment it happens, whatever the plan says. `enforce_age` is off only for
/// providers that judged age themselves.
pub(super) fn execute(
    plan: &Plan,
    env: &Env,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
    policy: &PathPolicy,
    enforce_age: bool,
) -> Outcome {
    let mut out = Outcome::default();
    for action in &plan.actions {
        if cancel.load(Ordering::Relaxed) {
            out.skipped.push("cancelled".into());
            break;
        }
        match action {
            Action::RemovePath(p) => {
                let r = if enforce_age { policy.remove_checked(p, SystemTime::now()) } else { policy.remove_placed(p) };
                match r {
                    Ok(n) => out.freed += n,
                    Err(e) => out.errors.push(format!("{}: {e}", p.display())),
                }
            }
            Action::TrashPath(p) => match policy.check_location(p) {
                Err(d) => out.errors.push(format!("{}: {d}", p.display())),
                Ok(_) => {
                    let bytes = fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0);
                    match gtk::gio::File::for_path(p).trash(gtk::gio::Cancellable::NONE) {
                        Ok(()) => out.trashed += bytes,
                        Err(e) => out.errors.push(format!("{}: {e}", p.display())),
                    }
                }
            },
            Action::Exec(cmd) => match env.runner.run(cmd, cancel) {
                Ok(o) if o.success() => out.commands_ok += 1,
                // pkexec: 126 = the password prompt was dismissed, 127 = refused.
                Ok(o) if cmd.privileged && o.code == Some(126) => {
                    out.errors.push(format!("{}: authorization was cancelled", cmd.display()))
                }
                Ok(o) if cmd.privileged && o.code == Some(127) => {
                    out.errors.push(format!("{}: authorization failed or was refused", cmd.display()))
                }
                Ok(o) => out.errors.push(format!("{}: exit {:?} {}", cmd.display(), o.code, o.stderr.trim())),
                Err(e) => out.errors.push(format!("{}: {e}", cmd.display())),
            },
        }
        progress(out.freed + out.trashed);
    }
    out
}

#[cfg(test)]
pub(super) fn test_dir(name: &str) -> std::path::PathBuf {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/diskmgr-providers").join(name);
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

/// Write `len` bytes and back-date both times by `days_ago`.
#[cfg(test)]
pub(super) fn test_file(p: &std::path::Path, len: usize, days_ago: u64) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; len]).unwrap();
    let t = SystemTime::now() - std::time::Duration::from_secs(days_ago * 86400 + 60);
    fs::File::open(p).unwrap().set_times(fs::FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diskmgr::{Scope, exec::FakeRunner};

    #[test]
    fn heavy_providers_are_opt_in() {
        let ids = |heavy| user_providers(heavy).iter().map(|p| p.id()).collect::<Vec<_>>();
        assert_eq!(ids(false), vec!["trash", "user-cache", "thumbnails", "x11-logs"]);
        assert_eq!(ids(true).len(), 6);
        assert!(ids(true).contains(&"large-files") && ids(true).contains(&"duplicates"));
    }

    #[test]
    fn root_providers_stay_out_of_the_unattended_list() {
        let user: Vec<_> = user_providers(true).iter().map(|p| p.id()).collect();
        for id in ["pkg-cache", "journal", "coredumps", "orphans"] {
            assert!(!user.contains(&id), "{id}");
        }
        let all: Vec<_> = interactive_providers(false).iter().map(|p| p.id()).collect();
        assert_eq!(&all[all.len() - 4..], ["pkg-cache", "journal", "coredumps", "orphans"]);
    }

    #[test]
    fn every_provider_copes_with_an_empty_home_and_stays_user_scope() {
        let home = test_dir("registry-empty");
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        for p in user_providers(true) {
            for s in p.scan(&env, &AtomicBool::new(false)) {
                assert_eq!(s.scope, Scope::User, "{}", p.id());
            }
        }
        assert!(runner.calls().is_empty(), "user providers run no commands");
    }
}
