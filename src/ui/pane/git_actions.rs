//! Git status display and context-menu actions for a pane.

use super::*;
use crate::integrations::git::{self, FileState};

impl Pane {
    pub fn in_repo(&self) -> bool {
        self.repo.borrow().is_some()
    }

    fn clear_git(&self) {
        let changed = !self.git.borrow().is_empty();
        self.git.borrow_mut().clear();
        *self.repo.borrow_mut() = None;
        self.repo_label.set_visible(false);
        if changed {
            let n = self.list.store.n_items();
            if n > 0 {
                self.list.store.items_changed(0, n, n);
            }
        }
    }

    /// Query git in the background and update badges + the branch label.
    pub fn refresh_git(self: &Rc<Self>) {
        let Some(dir) = self.cur_dir() else { return };
        let generation = self.git_gen.get() + 1;
        self.git_gen.set(generation);
        if !self.env.state.prefs.git_enabled()
            || dir.starts_with(crate::integrations::cloud::base_dir())
            || git::find_root_fast(&dir).is_none()
        {
            self.clear_git();
            return;
        }
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let d = dir.clone();
            let st = gio::spawn_blocking(move || git::status(&d)).await.ok().flatten();
            let Some(p) = w.upgrade() else { return };
            if p.git_gen.get() != generation {
                return; // a newer refresh superseded this one
            }
            match st {
                Some(st) => {
                    let map = git::states_for_dir(&st, &dir);
                    p.repo_label.set_text(&st.summary());
                    p.repo_label.set_tooltip_text(Some(&format!("Repository: {}", st.root.display())));
                    p.repo_label.set_visible(true);
                    let changed = *p.git.borrow() != map;
                    *p.git.borrow_mut() = map;
                    *p.repo.borrow_mut() = Some(st);
                    if changed {
                        let n = p.list.store.n_items();
                        if n > 0 {
                            p.list.store.items_changed(0, n, n);
                        }
                    }
                }
                None => p.clear_git(),
            }
            p.emit(PaneEvent::Status);
        });
    }

    fn git_root(&self) -> Option<PathBuf> {
        self.repo.borrow().as_ref().map(|r| r.root.clone())
    }

    /// Run a git job, then refresh badges and report the outcome.
    fn git_job(
        self: &Rc<Self>,
        title: &str,
        ok_msg: &str,
        job: impl FnOnce(&std::sync::atomic::AtomicBool) -> Result<String, String> + Send + 'static,
    ) {
        let (this, ok_msg) = (self.clone(), ok_msg.to_string());
        self.env.ops.run_job(
            title,
            move |cancel| job(&cancel),
            move |r| {
                match r {
                    Ok(out) => {
                        let first = out.lines().next().unwrap_or("").trim();
                        this.env.ops.toast(&if first.is_empty() || ok_msg.is_empty() {
                            ok_msg.clone()
                        } else {
                            format!("{ok_msg}: {first}")
                        });
                    }
                    Err(e) if e == "Cancelled" => this.env.ops.toast("Cancelled"),
                    Err(e) => this.env.ops.toast(&format!("Git: {e}")),
                }
                this.refresh_git();
            },
        );
    }

    pub fn git_stage(self: &Rc<Self>) {
        let (Some(root), paths) = (self.git_root(), self.selected_paths()) else { return };
        if !paths.is_empty() {
            self.git_job("Staging", "Staged", move |_| git::stage(&root, &paths));
        }
    }

    pub fn git_unstage(self: &Rc<Self>) {
        let (Some(root), paths) = (self.git_root(), self.selected_paths()) else { return };
        if !paths.is_empty() {
            self.git_job("Unstaging", "Unstaged", move |_| git::unstage(&root, &paths));
        }
    }

    pub fn git_discard(self: &Rc<Self>) {
        let (Some(root), paths, Some(win)) = (self.git_root(), self.selected_paths(), self.window()) else { return };
        if paths.is_empty() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let body = format!(
                "Uncommitted changes to {} will be lost and cannot be recovered. Untracked files are not affected.",
                if paths.len() == 1 { "this item".to_string() } else { format!("{} items", paths.len()) }
            );
            if dialogs::confirm(&win, "Discard changes?", &body, "Discard", true).await {
                this.git_job("Discarding changes", "Changes discarded", move |_| git::discard(&root, &paths));
            }
        });
    }

    pub fn git_commit(self: &Rc<Self>) {
        let (Some(root), Some(win)) = (self.git_root(), self.window()) else { return };
        let staged = self.repo.borrow().as_ref().map_or(0, |r| r.count(|c| c.staged));
        if staged == 0 {
            self.env.ops.toast("Nothing is staged. Stage files first.");
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let hint = format!("{staged} staged change{} will be committed.", if staged == 1 { "" } else { "s" });
            if let Some(msg) = dialogs::prompt_text(&win, "Commit", "Commit", &hint).await {
                this.git_job("Committing", "Committed", move |_| git::commit(&root, &msg));
            }
        });
    }

    pub fn git_pull(self: &Rc<Self>) {
        if let Some(root) = self.git_root() {
            self.git_job("Pulling", "Pulled", move |c| git::pull(&root, c));
        }
    }

    pub fn git_push(self: &Rc<Self>) {
        if let Some(root) = self.git_root() {
            self.git_job("Pushing", "Pushed", move |c| git::push(&root, c));
        }
    }

    pub(super) fn git_menu(&self, with_selection: bool) -> Option<gio::Menu> {
        if !self.in_repo() {
            return None;
        }
        let m = gio::Menu::new();
        if with_selection {
            let s = gio::Menu::new();
            s.append(Some("Stage"), Some("win.git-stage"));
            s.append(Some("Unstage"), Some("win.git-unstage"));
            s.append(Some("Discard Changes…"), Some("win.git-discard"));
            m.append_section(None, &s);
        }
        let r = gio::Menu::new();
        r.append(Some("Commit…"), Some("win.git-commit"));
        r.append(Some("Pull"), Some("win.git-pull"));
        r.append(Some("Push"), Some("win.git-push"));
        r.append(Some("Refresh Status"), Some("win.git-refresh"));
        m.append_section(None, &r);
        Some(m)
    }
}

#[allow(unused)]
fn _s(_: FileState) {}
