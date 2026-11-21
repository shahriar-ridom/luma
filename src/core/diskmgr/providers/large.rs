//! Big files in the home folder. Review only: nothing is pre-selected and
//! removal goes to the Trash, so it can be undone.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::core::diskmgr::{
    Action, CleanupProvider, Env, Item, Outcome, Plan, Risk, Scope, Suggestion, safety::PathPolicy,
};

pub struct LargeFiles {
    pub min_size: u64,
    pub max_items: usize,
}

impl Default for LargeFiles {
    fn default() -> Self {
        Self { min_size: 512 << 20, max_items: 50 }
    }
}

fn policy(env: &Env) -> PathPolicy {
    PathPolicy::new(vec![env.home.clone()], Duration::ZERO)
}

impl CleanupProvider for LargeFiles {
    fn id(&self) -> &'static str {
        "large-files"
    }

    fn available(&self, env: &Env) -> bool {
        env.home.is_dir()
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let Ok(root_dev) = fs::metadata(&env.home).map(|m| m.dev()) else { return vec![] };
        let mut found: Vec<(PathBuf, u64)> = vec![];
        let mut stack = vec![env.home.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return vec![];
                }
                // Hidden folders hold app data and configuration, not "files to clean".
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let Ok(md) = e.metadata() else { continue };
                if md.is_dir() {
                    if md.dev() == root_dev {
                        stack.push(e.path());
                    }
                } else if md.is_file() && md.len() >= self.min_size {
                    found.push((e.path(), md.len()));
                }
            }
        }
        found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        found.truncate(self.max_items);
        if found.is_empty() {
            return vec![];
        }
        let items: Vec<Item> = found
            .into_iter()
            .map(|(p, bytes)| Item {
                label: p.strip_prefix(&env.home).unwrap_or(&p).display().to_string(),
                path: Some(p),
                bytes,
                selected: false,
            })
            .collect();
        vec![Suggestion {
            id: "large-files".into(),
            title: "Large files".into(),
            detail: "The biggest files in your home folder. Look before you choose; they go to the Trash.".into(),
            est_bytes: Some(items.iter().map(|i| i.bytes).sum()),
            risk: Risk::Review,
            scope: Scope::User,
            items,
            hint: None,
        }]
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        plan_trash(selection, &policy(env))
    }

    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
        super::execute(plan, env, cancel, progress, &policy(env), true)
    }
}

/// Selected, regular, still-present files inside the policy's roots.
pub(super) fn plan_trash(selection: &[Item], policy: &PathPolicy) -> Plan {
    let actions = selection
        .iter()
        .filter(|i| i.selected)
        .filter_map(|i| i.path.clone())
        .filter(|p| policy.check_placement(p).is_ok())
        .filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .map(Action::TrashPath)
        .collect();
    Plan { actions }
}

#[cfg(test)]
mod tests {
    use super::super::{test_dir, test_file};
    use super::*;
    use crate::core::diskmgr::exec::FakeRunner;

    #[test]
    fn finds_big_files_largest_first_and_preselects_nothing() {
        let home = test_dir("large");
        test_file(&home.join("videos/a.mkv"), 3000, 1);
        test_file(&home.join("b.iso"), 5000, 1);
        test_file(&home.join("small.txt"), 10, 1);
        test_file(&home.join(".hidden/huge.bin"), 9000, 1);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        let p = LargeFiles { min_size: 1000, max_items: 10 };
        let s = p.scan(&env, &AtomicBool::new(false));
        let labels: Vec<_> = s[0].items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["b.iso", "videos/a.mkv"]);
        assert!(s[0].items.iter().all(|i| !i.selected));
        assert_eq!(s[0].risk, Risk::Review);
        assert!(p.plan(&s[0].items, &env).actions.is_empty(), "nothing selected, nothing planned");
    }

    #[test]
    fn caps_the_list_and_plans_trash_for_selection_only() {
        let home = test_dir("large-cap");
        for (n, len) in [("a", 2000), ("b", 3000), ("c", 4000)] {
            test_file(&home.join(n), len, 1);
        }
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let p = LargeFiles { min_size: 1000, max_items: 2 };
        let mut items = p.scan(&env, &AtomicBool::new(false))[0].items.clone();
        assert_eq!(items.len(), 2);
        items[0].selected = true;
        let plan = p.plan(&items, &env);
        assert_eq!(plan.actions, vec![Action::TrashPath(home.join("c"))]);
        assert!(home.join("c").exists(), "planning never touches files");
    }

    #[test]
    fn refuses_paths_outside_home_and_directories() {
        let home = test_dir("large-forge");
        fs::create_dir_all(home.join("dir")).unwrap();
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let item = |p: PathBuf| Item { label: "x".into(), path: Some(p), bytes: 1, selected: true };
        let p = LargeFiles::default();
        let plan = p.plan(&[item("/etc/passwd".into()), item(home.join("dir")), item(home.clone())], &env);
        assert!(plan.actions.is_empty());
        let rogue = Plan { actions: vec![Action::TrashPath("/etc/passwd".into())] };
        let out = p.run(&rogue, &env, &AtomicBool::new(false), &|_| {});
        assert_eq!(out.errors.len(), 1);
    }
}
