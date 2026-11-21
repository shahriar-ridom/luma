//! Old files under a fixed folder: the user cache, thumbnails, stale logs.
//! One offering per top-level entry, but only files that are old enough count,
//! so an app that is still using its cache keeps the recent part of it.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use crate::core::diskmgr::{
    Action, CleanupProvider, Env, Item, Plan, Risk, Scope, Suggestion, Thresholds, safety::PathPolicy,
};

pub struct AgedFiles {
    pub id: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    root: fn(&Env) -> PathBuf,
    /// Top-level names never offered (live databases, other providers' trees).
    deny: &'static [&'static str],
    days: fn(&Thresholds) -> u32,
    name_ok: fn(&str) -> bool,
}

impl AgedFiles {
    pub fn user_cache() -> Self {
        Self {
            id: "user-cache",
            title: "Application caches",
            detail: "Old files in ~/.cache. Apps rebuild what they still need.",
            root: |e| e.cache_dir.clone(),
            // Luma's own data, thumbnails (own provider) and shader caches
            // that running games and browsers keep open.
            deny: &["luma", "thumbnails", "mesa_shader_cache", "mesa_shader_cache_db"],
            days: |t| t.cache_days,
            name_ok: |_| true,
        }
    }

    pub fn thumbnails() -> Self {
        Self {
            id: "thumbnails",
            title: "Old thumbnails",
            detail: "Thumbnails not used for a long while; they are recreated when needed.",
            root: |e| e.cache_dir.join("thumbnails"),
            deny: &[],
            days: |t| t.thumbnail_days,
            name_ok: |_| true,
        }
    }

    pub fn x11_logs() -> Self {
        Self {
            id: "x11-logs",
            title: "Old X server logs",
            detail: "Rotated Xorg logs (*.old) in ~/.local/share/xorg.",
            root: |e| e.data_dir.join("xorg"),
            deny: &[],
            days: |t| t.cache_days,
            name_ok: |n| n.ends_with(".old"),
        }
    }

    fn policy(&self, env: &Env) -> PathPolicy {
        PathPolicy::new(vec![(self.root)(env)], Duration::from_secs(u64::from((self.days)(&env.thresholds)) * 86400))
    }

    /// Files under `top` that may go, with their sizes.
    fn eligible(&self, top: &Path, policy: &PathPolicy, cancel: &AtomicBool) -> Vec<(PathBuf, u64)> {
        if policy.check_location(top).is_err() {
            return vec![];
        }
        let Some(root_dev) = policy.roots.first().and_then(|r| fs::metadata(r).ok()).map(|m| m.dev()) else {
            return vec![];
        };
        let now = SystemTime::now();
        let mut out = vec![];
        let mut stack = vec![top.to_path_buf()];
        while let Some(p) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            // Never follows links: a link is judged and removed as itself.
            let Ok(md) = fs::symlink_metadata(&p) else { continue };
            if md.dev() != root_dev {
                continue;
            }
            if md.is_dir() {
                if let Ok(rd) = fs::read_dir(&p) {
                    stack.extend(rd.flatten().map(|e| e.path()));
                }
            } else if p.file_name().is_some_and(|n| (self.name_ok)(&n.to_string_lossy()))
                && policy.check_age(&md, now).is_ok()
            {
                out.push((p, md.len()));
            }
        }
        out
    }
}

impl CleanupProvider for AgedFiles {
    fn id(&self) -> &'static str {
        self.id
    }

    fn available(&self, env: &Env) -> bool {
        (self.root)(env).is_dir()
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let policy = self.policy(env);
        let Ok(rd) = fs::read_dir((self.root)(env)) else { return vec![] };
        let mut entries: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| !self.deny.contains(&e.file_name().to_string_lossy().as_ref()))
            .map(|e| e.path())
            .collect();
        entries.sort();
        let items: Vec<Item> = entries
            .into_iter()
            .filter_map(|p| {
                let bytes: u64 = self.eligible(&p, &policy, cancel).iter().map(|f| f.1).sum();
                (bytes > 0).then(|| Item {
                    label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    path: Some(p),
                    bytes,
                    selected: true,
                })
            })
            .collect();
        if items.is_empty() {
            return vec![];
        }
        vec![Suggestion {
            id: self.id.into(),
            title: self.title.into(),
            detail: self.detail.into(),
            est_bytes: Some(items.iter().map(|i| i.bytes).sum()),
            risk: Risk::Safe,
            scope: Scope::User,
            items,
            hint: None,
        }]
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        let policy = self.policy(env);
        let never = AtomicBool::new(false);
        let actions = selection
            .iter()
            .filter(|i| i.selected)
            .filter_map(|i| i.path.as_deref())
            .flat_map(|p| self.eligible(p, &policy, &never))
            .map(|(p, _)| Action::RemovePath(p))
            .collect();
        Plan { actions }
    }

    fn run(
        &self,
        plan: &Plan,
        env: &Env,
        cancel: &AtomicBool,
        progress: &dyn Fn(u64),
    ) -> crate::core::diskmgr::Outcome {
        super::execute(plan, env, cancel, progress, &self.policy(env), true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{test_dir, test_file};
    use super::*;
    use crate::core::diskmgr::exec::FakeRunner;
    use std::os::unix::fs::symlink;

    fn files_under(p: &Path) -> Vec<String> {
        let mut v = vec![];
        let mut stack = vec![p.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(d).unwrap().flatten() {
                let md = fs::symlink_metadata(e.path()).unwrap();
                if md.is_dir() {
                    stack.push(e.path());
                } else {
                    v.push(e.path().strip_prefix(p).unwrap().display().to_string());
                }
            }
        }
        v.sort();
        v
    }

    #[test]
    fn offers_only_old_files_and_skips_denied_entries() {
        let home = test_dir("aged-scan");
        let c = home.join(".cache");
        test_file(&c.join("firefox/old.bin"), 1000, 60);
        test_file(&c.join("firefox/new.bin"), 500, 1);
        test_file(&c.join("luma/db.sqlite"), 9000, 400);
        test_file(&c.join("thumbnails/normal/t.png"), 700, 400);
        test_file(&c.join("fresh/x"), 100, 2);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        let s = AgedFiles::user_cache().scan(&env, &AtomicBool::new(false));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].scope, Scope::User);
        let labels: Vec<_> = s[0].items.iter().map(|i| (i.label.as_str(), i.bytes)).collect();
        assert_eq!(labels, vec![("firefox", 1000)]);
        assert_eq!(s[0].est_bytes, Some(1000));
    }

    #[test]
    fn plan_is_a_dry_run_and_run_deletes_exactly_the_plan() {
        let home = test_dir("aged-run");
        let c = home.join(".cache");
        test_file(&c.join("app/old1"), 100, 40);
        test_file(&c.join("app/sub/old2"), 200, 40);
        test_file(&c.join("app/new"), 300, 0);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        let p = AgedFiles::user_cache();
        let sug = p.scan(&env, &AtomicBool::new(false));
        let plan = p.plan(&sug[0].items, &env);
        assert_eq!(plan.actions.len(), 2);
        assert_eq!(files_under(&c), vec!["app/new", "app/old1", "app/sub/old2"], "planning must not delete");
        let out = p.run(&plan, &env, &AtomicBool::new(false), &|_| {});
        assert_eq!(out.freed, 300);
        assert!(out.errors.is_empty());
        assert_eq!(files_under(&c), vec!["app/new"]);
    }

    #[test]
    fn deselected_items_and_forged_paths_are_ignored() {
        let home = test_dir("aged-forged");
        let c = home.join(".cache");
        test_file(&c.join("keep/old"), 100, 90);
        test_file(&home.join("documents/old.txt"), 100, 90);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let p = AgedFiles::user_cache();
        let mut items = p.scan(&env, &AtomicBool::new(false))[0].items.clone();
        items[0].selected = false;
        items.push(Item { label: "x".into(), path: Some(home.join("documents")), bytes: 1, selected: true });
        items.push(Item { label: "y".into(), path: Some(c.join("../documents")), bytes: 1, selected: true });
        assert!(p.plan(&items, &env).actions.is_empty());
        // A plan built by hand with a path outside the root fails at run time.
        let rogue = Plan { actions: vec![Action::RemovePath(home.join("documents/old.txt"))] };
        let out = p.run(&rogue, &env, &AtomicBool::new(false), &|_| {});
        assert_eq!(out.freed, 0);
        assert_eq!(out.errors.len(), 1);
        assert!(home.join("documents/old.txt").exists());
    }

    #[test]
    fn symlinks_are_removed_as_links_never_followed() {
        let home = test_dir("aged-links");
        let c = home.join(".cache");
        test_file(&home.join("precious/data"), 100, 90);
        fs::create_dir_all(c.join("app")).unwrap();
        symlink(home.join("precious"), c.join("app/link")).unwrap();
        test_file(&c.join("app/old"), 50, 90);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let p = AgedFiles::user_cache();
        let sug = p.scan(&env, &AtomicBool::new(false));
        let plan = p.plan(&sug[0].items, &env);
        p.run(&plan, &env, &AtomicBool::new(false), &|_| {});
        assert!(home.join("precious/data").exists());
    }

    #[test]
    fn thresholds_and_name_filter() {
        let home = test_dir("aged-xorg");
        let x = home.join(".local/share/xorg");
        test_file(&x.join("Xorg.0.log.old"), 100, 45);
        test_file(&x.join("Xorg.0.log"), 100, 45);
        let runner = FakeRunner::ok();
        let mut env = Env::rooted(home, &runner);
        let p = AgedFiles::x11_logs();
        let s = p.scan(&env, &AtomicBool::new(false));
        assert_eq!(s[0].items.len(), 1);
        assert_eq!(s[0].items[0].label, "Xorg.0.log.old");
        env.thresholds.cache_days = 90;
        assert!(p.scan(&env, &AtomicBool::new(false)).is_empty());
        env.thresholds.cache_days = 0;
        assert_eq!(p.scan(&env, &AtomicBool::new(false)).len(), 1, "0 days still means at least the 24h floor");
    }

    #[test]
    fn missing_root_is_unavailable_not_an_error() {
        let home = test_dir("aged-missing");
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        let p = AgedFiles::thumbnails();
        assert!(!p.available(&env));
        assert!(p.scan(&env, &AtomicBool::new(false)).is_empty());
    }
}
