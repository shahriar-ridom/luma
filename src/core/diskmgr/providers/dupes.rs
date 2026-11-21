//! Duplicate files in the home folder, found with the existing duplicate
//! scanner (same size, then same content). The first path of each group is
//! kept; the extra copies are offered, unselected, and go to the Trash.

use std::{sync::atomic::AtomicBool, time::Duration};

use crate::core::{
    diskmgr::{CleanupProvider, Env, Item, Outcome, Plan, Risk, Scope, Suggestion, safety::PathPolicy},
    duplicates::{self, DupEvent},
    filesystem::WalkLimits,
};

pub struct DuplicateFiles {
    pub min_size: u64,
    pub max_groups: usize,
}

impl Default for DuplicateFiles {
    fn default() -> Self {
        Self { min_size: 1 << 20, max_groups: 100 }
    }
}

fn policy(env: &Env) -> PathPolicy {
    PathPolicy::new(vec![env.home.clone()], Duration::ZERO)
}

impl CleanupProvider for DuplicateFiles {
    fn id(&self) -> &'static str {
        "duplicates"
    }

    fn available(&self, env: &Env) -> bool {
        env.home.is_dir()
    }

    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion> {
        let mut groups = vec![];
        let limits = WalkLimits { skip: vec![], same_fs: true };
        duplicates::run(&env.home, false, self.min_size, &limits, cancel, |ev| {
            if let DupEvent::Group(g) = ev {
                groups.push(g);
            }
        });
        // Biggest waste first; ties by path so the order is stable.
        groups.sort_by(|a, b| {
            let waste = |g: &duplicates::DupGroup| g.size * (g.files.len() as u64 - 1);
            waste(b).cmp(&waste(a)).then_with(|| a.files.cmp(&b.files))
        });
        groups.truncate(self.max_groups);
        let mut items = vec![];
        for g in &groups {
            let Some((keep, extras)) = g.files.split_first() else { continue };
            let keep_name = keep.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            for f in extras {
                items.push(Item {
                    label: format!("{} (copy of {keep_name})", f.strip_prefix(&env.home).unwrap_or(f).display()),
                    path: Some(f.clone()),
                    bytes: g.size,
                    selected: false,
                });
            }
        }
        if items.is_empty() {
            return vec![];
        }
        vec![Suggestion {
            id: "duplicates".into(),
            title: "Duplicate files".into(),
            detail: "Identical copies in your home folder. One copy of each is kept; extras go to the Trash.".into(),
            est_bytes: Some(items.iter().map(|i| i.bytes).sum()),
            risk: Risk::Review,
            scope: Scope::User,
            items,
            hint: None,
        }]
    }

    fn plan(&self, selection: &[Item], env: &Env) -> Plan {
        super::large::plan_trash(selection, &policy(env))
    }

    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome {
        super::execute(plan, env, cancel, progress, &policy(env), true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{test_dir, test_file};
    use super::*;
    use crate::core::diskmgr::{Action, exec::FakeRunner};

    #[test]
    fn keeps_one_copy_and_offers_the_rest_unselected() {
        let home = test_dir("dupes");
        test_file(&home.join("a/photo.jpg"), 2000, 1);
        test_file(&home.join("b/photo-copy.jpg"), 2000, 1);
        test_file(&home.join("c/unique.bin"), 2000, 1);
        // Same size, different content must not be reported.
        std::fs::write(home.join("c/unique.bin"), vec![9u8; 2000]).unwrap();
        let runner = FakeRunner::ok();
        let env = Env::rooted(home.clone(), &runner);
        let p = DuplicateFiles { min_size: 1000, max_groups: 10 };
        let s = p.scan(&env, &AtomicBool::new(false));
        assert_eq!(s[0].items.len(), 1);
        let it = &s[0].items[0];
        assert!(it.label.starts_with("b/photo-copy.jpg"), "{}", it.label);
        assert!(!it.selected);
        assert_eq!(s[0].est_bytes, Some(2000));

        let mut items = s[0].items.clone();
        items[0].selected = true;
        assert_eq!(p.plan(&items, &env).actions, vec![Action::TrashPath(home.join("b/photo-copy.jpg"))]);
    }

    #[test]
    fn nothing_found_means_no_suggestion() {
        let home = test_dir("dupes-none");
        test_file(&home.join("one"), 2000, 1);
        let runner = FakeRunner::ok();
        let env = Env::rooted(home, &runner);
        assert!(DuplicateFiles { min_size: 1000, max_groups: 10 }.scan(&env, &AtomicBool::new(false)).is_empty());
    }
}
