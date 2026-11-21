//! The home Trash. Age is the time of deletion (from the `.trashinfo`, else
//! the entry's ctime), never the file's mtime: a file trashed today keeps the
//! old mtime it had before.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gtk::glib;

use crate::core::{
    diskmgr::{
        Action, CleanupProvider, Env, Item, Outcome, Plan, Risk, Scope, Suggestion,
        safety::{MIN_AGE_FLOOR, PathPolicy},
    },
    metadata::dir_stats,
};

pub struct TrashProvider;

fn trash_dir(env: &Env) -> PathBuf {
    env.data_dir.join("Trash")
}

fn info_for(trash: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut p = trash.join("info").join(name);
    p.as_mut_os_string().push(".trashinfo");
    p
}

fn min_age(env: &Env) -> Duration {
    Duration::from_secs(u64::from(env.thresholds.trash_days) * 86400).max(MIN_AGE_FLOOR)
}

/// `YYYY-MM-DDThh:mm:ss` -> its parts. Pure so it can be tested anywhere.
fn parse_date_parts(s: &str) -> Option<(i32, i32, i32, i32, i32, i32)> {
    let (d, t) = s.trim().split_once('T')?;
    let mut dp = d.split('-').map(|x| x.parse::<i32>().ok());
    let mut tp = t.split(':').map(|x| x.parse::<i32>().ok());
    Some((dp.next()??, dp.next()??, dp.next()??, tp.next()??, tp.next()??, tp.next()??))
}

/// When was the entry deleted? Local time, as the spec writes it.
fn deleted_at(info: &Path, entry: &fs::Metadata) -> SystemTime {
    let from_info = fs::read_to_string(info)
        .ok()
        .and_then(|text| text.lines().find_map(|l| l.strip_prefix("DeletionDate=").map(str::to_string)))
        .and_then(|d| parse_date_parts(&d))
        .and_then(|(y, mo, d, h, mi, s)| glib::DateTime::from_local(y, mo, d, h, mi, f64::from(s)).ok())
        .and_then(|dt| u64::try_from(dt.to_unix()).ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
    // Renaming into the Trash updates ctime, so it is a sound fallback for
    // entries without (or with a broken) info file.
    from_info.unwrap_or_else(|| UNIX_EPOCH + Duration::from_secs(entry.ctime().max(0) as u64))
}

fn old_enough(env: &Env, entry: &Path, now: SystemTime) -> Option<u64> {
    let trash = trash_dir(env);
    let md = fs::symlink_metadata(entry).ok()?;
    let when = deleted_at(&info_for(&trash, entry.file_name()?), &md);
    if now.duration_since(when).ok()? < min_age(env) {
        return None;
    }
    Some(if md.is_dir() { dir_stats(entry, &AtomicBool::new(false)).bytes } else { md.len() })
}

fn policy(env: &Env) -> PathPolicy {
    let t = trash_dir(env);
    PathPolicy::new(vec![t.join("files"), t.join("info")], Duration::ZERO)
}

impl CleanupProvider for TrashProvider {
    fn id(&self) -> &'static str {
        "trash"
    }

    fn available(&self, env: &Env) -> bool {
        trash_dir(env).join("files").is_dir()
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let Ok(rd) = fs::read_dir(trash_dir(env).join("files")) else { return vec![] };
        let now = SystemTime::now();
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        let mut items = vec![];
        for p in entries {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if let Some(bytes) = old_enough(env, &p, now) {
                items.push(Item {
                    label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    path: Some(p),
                    bytes,
                    selected: true,
                });
            }
        }
        if items.is_empty() {
            return vec![];
        }
        vec![Suggestion {
            id: "trash".into(),
            title: "Old items in the Trash".into(),
            detail: format!("Deleted more than {} days ago. This cannot be undone.", min_age(env).as_secs() / 86400),
            est_bytes: Some(items.iter().map(|i| i.bytes).sum()),
            risk: Risk::Review,
            scope: Scope::User,
            items,
            hint: None,
        }]
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        let (pol, trash, now) = (policy(env), trash_dir(env), SystemTime::now());
        let mut actions = vec![];
        for p in selection.iter().filter(|i| i.selected).filter_map(|i| i.path.as_deref()) {
            // Re-judged here: only a real, still-old entry of files/ qualifies.
            if pol.check_placement(p).is_err() || p.parent() != Some(trash.join("files").as_path()) {
                continue;
            }
            if old_enough(env, p, now).is_none() {
                continue;
            }
            if let Some(name) = p.file_name() {
                actions.push(Action::RemovePath(p.to_path_buf()));
                let info = info_for(&trash, name);
                if fs::symlink_metadata(&info).is_ok() {
                    actions.push(Action::RemovePath(info));
                }
            }
        }
        Plan { actions }
    }

    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
        let out = super::execute(plan, env, cancel, progress, &policy(env), false);
        // A cache of folder sizes; entries for removed folders are stale now.
        let _ = fs::remove_file(trash_dir(env).join("directorysizes"));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{test_dir, test_file};
    use super::*;
    use crate::core::diskmgr::exec::FakeRunner;

    fn trash_item(home: &Path, name: &str, len: usize, date: &str) {
        let t = home.join(".local/share/Trash");
        // The file keeps an old mtime on purpose, as real trashed files do.
        test_file(&t.join("files").join(name), len, 500);
        fs::create_dir_all(t.join("info")).unwrap();
        fs::write(info_for(&t, name.as_ref()), format!("[Trash Info]\nPath=/x/{name}\nDeletionDate={date}\n")).unwrap();
    }

    fn now_str() -> String {
        glib::DateTime::now_local().unwrap().format("%Y-%m-%dT%H:%M:%S").unwrap().to_string()
    }

    #[test]
    fn date_parser() {
        assert_eq!(parse_date_parts("2024-03-01T09:05:07"), Some((2024, 3, 1, 9, 5, 7)));
        assert_eq!(parse_date_parts("garbage"), None);
        assert_eq!(parse_date_parts("2024-03-01"), None);
        assert_eq!(parse_date_parts("2024-xx-01T00:00:00"), None);
    }

    #[test]
    fn age_is_the_deletion_date_not_the_mtime() {
        let home = test_dir("trash-age");
        trash_item(&home, "old.bin", 1000, "2001-01-01T00:00:00");
        // Old mtime, but trashed just now: must be kept.
        trash_item(&home, "just-now.bin", 2000, &now_str());
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let s = TrashProvider.scan(&env, &AtomicBool::new(false));
        let labels: Vec<_> = s[0].items.iter().map(|i| (i.label.as_str(), i.bytes)).collect();
        assert_eq!(labels, vec![("old.bin", 1000)]);

        let plan = TrashProvider.plan(&s[0].items, &env);
        assert_eq!(plan.actions.len(), 2, "the file and its .trashinfo");
        let out = TrashProvider.run(&plan, &env, &AtomicBool::new(false), &|_| {});
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.freed >= 1000, "file plus its small info file");
        let t = home.join(".local/share/Trash");
        assert!(!t.join("files/old.bin").exists());
        assert!(!info_for(&t, "old.bin".as_ref()).exists());
        assert!(t.join("files/just-now.bin").exists());
        assert!(info_for(&t, "just-now.bin".as_ref()).exists());
    }

    #[test]
    fn missing_info_falls_back_to_ctime_which_is_recent() {
        let home = test_dir("trash-orphan");
        let t = home.join(".local/share/Trash");
        test_file(&t.join("files/stray"), 10, 500); // ctime = now
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        assert!(TrashProvider.scan(&env, &AtomicBool::new(false)).is_empty());
    }

    #[test]
    fn plan_rejects_entries_outside_files_dir() {
        let home = test_dir("trash-forged");
        trash_item(&home, "old.bin", 10, "2001-01-01T00:00:00");
        test_file(&home.join("doc.txt"), 10, 900);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let t = home.join(".local/share/Trash");
        let forged = |p: PathBuf| Item { label: "f".into(), path: Some(p), bytes: 1, selected: true };
        let plan = TrashProvider.plan(
            &[forged(home.join("doc.txt")), forged(t.join("info")), forged(t.join("files/sub/../../files"))],
            &env,
        );
        assert!(plan.actions.is_empty());
        assert!(home.join("doc.txt").exists());
    }

    #[test]
    fn recent_threshold_respects_floor() {
        let home = test_dir("trash-floor");
        trash_item(&home, "today.bin", 10, &now_str());
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home, &runner);
        env.thresholds.trash_days = 0;
        assert!(TrashProvider.scan(&env, &AtomicBool::new(false)).is_empty());
    }
}
