//! Pane actions beyond basic file operations: properties, archives,
//! terminal, copy path, reveal.

use super::*;
use crate::integrations::terminal;
use crate::services::archive::{self, ArchiveError};
use crate::ui::dialogs::archive as archive_dialogs;

fn unique_file(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} {n}.{ext}"))).find(|p| !p.exists()).unwrap()
}

impl Pane {
    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(d) = self.cur_dir() else { return vec![] };
        self.list.selected_infos().into_iter().map(|i| d.join(i.name())).collect()
    }

    /// Folders the Favorites actions apply to: the selected folders, or the
    /// folder being shown when no folder is selected.
    pub fn favorite_targets(&self) -> Vec<PathBuf> {
        let Some(d) = self.cur_dir() else { return vec![] };
        let dirs: Vec<PathBuf> =
            self.list.selected_infos().iter().filter(|i| filesystem::is_dir(i)).map(|i| d.join(i.name())).collect();
        if dirs.is_empty() { vec![d] } else { dirs }
    }

    /// (can add, can remove) for the current favorite targets.
    fn favorite_state(&self, targets: &[PathBuf]) -> (bool, bool) {
        let user: Vec<PathBuf> =
            self.env.state.db.bookmarks().unwrap_or_default().into_iter().map(|b| b.path).collect();
        let can_add = targets.iter().any(|t| !user.contains(t) && !crate::ui::sidebar::builtin_favorite(t));
        let can_remove = targets.iter().any(|t| user.contains(t));
        (can_add, can_remove)
    }

    fn append_favorite_items(&self, menu: &gio::Menu, targets: &[PathBuf]) {
        let (add, remove) = self.favorite_state(targets);
        if add {
            menu.append(Some("Add to Favorites"), Some("win.bookmark-add"));
        }
        if remove {
            menu.append(Some("Remove from Favorites"), Some("win.favorite-remove"));
        }
    }

    /// True when something is selected and every selected item is an archive.
    pub fn archive_selection(&self) -> bool {
        let infos = self.list.selected_infos();
        !infos.is_empty()
            && infos.iter().all(|i| !filesystem::is_dir(i) && archive::is_archive(&i.name().to_string_lossy()))
    }

    pub fn search_root(&self) -> PathBuf {
        self.cur_dir().unwrap_or_else(glib::home_dir)
    }

    /// Enter / double-click: enter folders, extract common archives, open the rest.
    pub fn open_path(self: &Rc<Self>, path: PathBuf, is_dir: bool) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if is_dir {
            self.navigate(path);
        } else if archive::is_extractable_on_open(&name) {
            self.extract_paths(vec![path], true);
        } else {
            self.launch(&path);
        }
    }

    pub fn batch_rename(self: &Rc<Self>) {
        let (Some(win), Some(dir)) = (self.window(), self.cur_dir()) else { return };
        let names: Vec<String> =
            self.list.selected_infos().iter().map(|i| i.name().to_string_lossy().into_owned()).collect();
        if names.len() < 2 {
            return;
        }
        let existing: std::collections::HashSet<String> = (0..self.list.store.n_items())
            .filter_map(|i| self.list.store.item(i).and_downcast::<gio::FileInfo>())
            .map(|i| i.name().to_string_lossy().into_owned())
            .collect();
        let this = self.clone();
        crate::ui::dialogs::batch_rename::show(
            &win,
            names,
            existing,
            Rc::new(move |plan| {
                let pairs: Vec<(PathBuf, PathBuf)> =
                    plan.iter().map(|p| (dir.join(&p.old), dir.join(&p.new))).collect();
                let (p2, t) = (pairs.clone(), this.clone());
                this.env.ops.run_job(
                    "Renaming files",
                    move |_c| crate::core::operations::rename::rename_many(&p2),
                    move |r| match r {
                        Ok(()) => {
                            let st = &t.env.state;
                            for (a, b) in &pairs {
                                let _ = st.db.move_tags(a, b);
                            }
                            st.undo.push(
                                "Batch Rename",
                                UndoAction::Rename(pairs.iter().map(|(a, b)| (b.clone(), a.clone())).collect()),
                            );
                            t.env.ops.toast(&format!("Renamed {} items", pairs.len()));
                            t.reload_tags();
                        }
                        Err(e) => t.env.ops.toast(&format!("Nothing was renamed: {e}")),
                    },
                );
            }),
        );
    }

    pub fn edit_tags(self: &Rc<Self>) {
        let (paths, Some(win)) = (self.selected_paths(), self.window()) else { return };
        if paths.is_empty() {
            return;
        }
        let this = self.clone();
        crate::ui::dialogs::tags::edit(
            &win,
            paths,
            self.env.state.clone(),
            Rc::new(move || {
                this.reload_tags();
                this.emit(PaneEvent::Tags);
            }),
        );
    }

    /// True for exactly two selected regular files.
    pub fn can_compare(&self) -> bool {
        let infos = self.list.selected_infos();
        infos.len() == 2 && infos.iter().all(|i| !filesystem::is_dir(i))
    }

    pub fn compare(&self) {
        let (p, Some(win)) = (self.selected_paths(), self.window()) else { return };
        if self.can_compare() {
            crate::ui::dialogs::compare::show(&win, p[0].clone(), p[1].clone());
        }
    }

    pub fn find_duplicates(self: &Rc<Self>) {
        let (Some(win), Some(dir)) = (self.window(), self.cur_dir()) else { return };
        let this = self.clone();
        crate::ui::dialogs::duplicates::show(
            &win,
            dir,
            Rc::new(move |paths| {
                this.env.ops.submit(OpRequest {
                    kind: OpKind::Trash,
                    sources: paths.iter().map(|p| gio::File::for_path(p).uri().to_string()).collect(),
                    dest: None,
                });
            }),
        );
    }

    /// Show `model` at (x, y) in `from`'s coordinates.
    pub fn popup_menu(&self, model: &gio::Menu, from: &gtk::Widget, x: f64, y: f64) {
        let Some(pt) = from.compute_point(&self.stack, &gtk::graphene::Point::new(x as f32, y as f32)) else { return };
        let state = self.env.state.clone();
        let accel_of: Rc<dyn Fn(&str) -> Option<String>> =
            Rc::new(move |id| Some(state.prefs.accel(id)).filter(|a| !a.is_empty()));
        crate::ui::menu::popup(
            self.stack.upcast_ref(),
            model,
            gtk::gdk::Rectangle::new(pt.x() as i32, pt.y() as i32, 1, 1),
            accel_of,
        );
    }

    /// Context menu for empty space (rebuilt on every right-click).
    pub fn bg_menu(&self) -> gio::Menu {
        let menu = gio::Menu::new();
        let news = gio::Menu::new();
        news.append(Some("New Folder"), Some("win.new-folder"));
        news.append(Some("New File"), Some("win.new-file"));
        menu.append_section(None, &news);
        let clip = gio::Menu::new();
        clip.append(Some("Paste"), Some("win.paste"));
        clip.append(Some("Select All"), Some("win.select-all"));
        menu.append_section(None, &clip);
        if let Some(d) = self.cur_dir() {
            let fav = gio::Menu::new();
            self.append_favorite_items(&fav, &[d]);
            if fav.n_items() > 0 {
                menu.append_section(None, &fav);
            }
        }
        let more = gio::Menu::new();
        more.append(Some("Open Terminal Here"), Some("win.open-terminal"));
        more.append(Some("Find Duplicates…"), Some("win.find-duplicates"));
        if let Some(g) = self.git_menu(false) {
            more.append_submenu(Some("Git"), &g);
        }
        if let Some(a) = self.ext_menu() {
            more.append_submenu(Some("Actions"), &a);
        }
        let tail = gio::Menu::new();
        tail.append_submenu(Some("More"), &more);
        tail.append(Some("Properties"), Some("win.properties"));
        menu.append_section(None, &tail);
        menu
    }

    /// Context menu for the current selection (rebuilt on every right-click).
    pub fn item_menu(&self) -> gio::Menu {
        // Kept short on purpose: a tall popover near the bottom of the window
        // would have to scroll. Rare actions live in submenus.
        let infos = self.list.selected_infos();
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append(Some("Open"), Some("win.open"));
        if infos.iter().any(|i| !filesystem::is_dir(i)) {
            open.append(Some("Open With…"), Some("win.open-with"));
        }
        menu.append_section(None, &open);
        let clip = gio::Menu::new();
        clip.append(Some("Cut"), Some("win.cut"));
        clip.append(Some("Copy"), Some("win.copy"));
        clip.append(Some("Paste"), Some("win.paste"));
        menu.append_section(None, &clip);
        let edit = gio::Menu::new();
        edit.append(Some(if infos.len() > 1 { "Batch Rename…" } else { "Rename…" }), Some("win.rename"));
        edit.append(Some("Move to Trash"), Some("win.trash"));
        menu.append_section(None, &edit);

        let pack = gio::Menu::new();
        if self.archive_selection() {
            let ex = gio::Menu::new();
            ex.append(Some("Extract Here"), Some("win.extract-here"));
            ex.append(Some("Extract To…"), Some("win.extract-to"));
            if infos.len() == 1 {
                ex.append(Some("Browse Contents…"), Some("win.browse-archive"));
            }
            pack.append_submenu(Some("Archive"), &ex);
        }
        pack.append(Some("Compress…"), Some("win.compress"));
        pack.append(Some("Tags…"), Some("win.tags"));
        let dirs: Vec<PathBuf> = self
            .cur_dir()
            .map(|d| infos.iter().filter(|i| filesystem::is_dir(i)).map(|i| d.join(i.name())).collect())
            .unwrap_or_default();
        if !dirs.is_empty() {
            self.append_favorite_items(&pack, &dirs);
        }
        menu.append_section(None, &pack);

        let more = gio::Menu::new();
        if infos.len() == 1 && filesystem::is_dir(&infos[0]) {
            more.append(Some("Open Terminal Here"), Some("win.open-terminal"));
        }
        if self.can_compare() {
            more.append(Some("Compare Files…"), Some("win.compare"));
        }
        more.append(Some("Copy Path"), Some("win.copy-path"));
        more.append(Some("Delete Permanently…"), Some("win.delete"));
        if let Some(g) = self.git_menu(true) {
            more.append_submenu(Some("Git"), &g);
        }
        if let Some(a) = self.ext_menu() {
            more.append_submenu(Some("Actions"), &a);
        }
        let tail = gio::Menu::new();
        tail.append_submenu(Some("More"), &more);
        tail.append(Some("Properties"), Some("win.properties"));
        menu.append_section(None, &tail);
        menu
    }

    pub fn properties(&self) {
        let Some(win) = self.window() else { return };
        let mut t = self.selected_paths();
        if t.is_empty() {
            t.extend(self.cur_dir());
        }
        crate::ui::dialogs::properties::show(&win, t, self.env.state.clone());
    }

    pub fn copy_path(&self) {
        let mut t = self.selected_paths();
        if t.is_empty() {
            t.extend(self.cur_dir());
        }
        let text = t.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n");
        self.stack.clipboard().set_text(&text);
        self.env.ops.toast(if t.len() == 1 { "Path copied" } else { "Paths copied" });
    }

    pub fn open_terminal(&self) {
        let infos = self.list.selected_infos();
        let dir = match (infos.as_slice(), self.cur_dir()) {
            ([one], Some(cur)) if filesystem::is_dir(one) => cur.join(one.name()),
            (_, Some(cur)) => cur,
            _ => return,
        };
        let pref = self.env.state.prefs.terminal.borrow().clone();
        if let Err(e) = terminal::open_in(&dir, &pref) {
            self.env.ops.toast(&e);
        }
    }

    /// Show `path` in its folder with the item selected.
    pub fn reveal(self: &Rc<Self>, path: PathBuf) {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return };
        let name = name.to_string_lossy().into_owned();
        if self.cur_dir().as_deref() == Some(parent) {
            self.select_visible(&name);
        } else {
            *self.pending_select.borrow_mut() = Some(name);
            self.navigate(parent.to_path_buf());
        }
    }

    /// Select `name` as soon as the folder finishes loading.
    pub fn select_when_loaded(&self, name: String) {
        *self.pending_select.borrow_mut() = Some(name);
    }

    pub fn select_visible(&self, name: &str) {
        if let Some(pos) = self.list.select_name(name) {
            self.grid.scroll_to(pos);
        }
    }

    fn reveal_soon(self: &Rc<Self>, path: PathBuf) {
        // The directory monitor adds new entries after a short debounce.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(450), move || {
            if let Some(p) = weak.upgrade() {
                p.reveal(path);
            }
        });
    }

    // ---- archives ------------------------------------------------------------

    pub fn compress(self: &Rc<Self>) {
        let (paths, Some(dir), Some(win)) = (self.selected_paths(), self.cur_dir(), self.window()) else { return };
        if paths.is_empty() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let default = if paths.len() == 1 {
                let n = paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if filesystem::is_dir_async(&paths[0]).await {
                    n
                } else {
                    n.rsplit_once('.').map(|(s, _)| s.to_string()).filter(|s| !s.is_empty()).unwrap_or(n)
                }
            } else {
                dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into())
            };
            let Some(c) = archive_dialogs::compress(&win, &default, paths.len()).await else { return };
            let (d, n, e) = (dir.clone(), c.name.clone(), c.format.ext());
            let Ok(out) = gio::spawn_blocking(move || unique_file(&d, &n, e)).await else { return };
            let names: Vec<String> =
                paths.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
            let (base, out2) = (dir.clone(), out.clone());
            let title =
                format!("Creating {}", out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let t2 = this.clone();
            this.env.ops.run_job(
                &title,
                move |cancel| archive::create(c.format, c.level, &base, &names, &out2, &cancel),
                move |r| match r {
                    Ok(()) => {
                        t2.env.ops.toast("Archive created");
                        t2.reveal_soon(out);
                    }
                    Err(ArchiveError::Cancelled) => t2.env.ops.toast("Cancelled"),
                    Err(e) => t2.env.ops.toast(&format!("Could not create archive: {e}")),
                },
            );
        });
    }

    pub fn extract(self: &Rc<Self>, here: bool) {
        let archives: Vec<PathBuf> = self.selected_paths();
        if archives.is_empty() || !self.archive_selection() {
            return;
        }
        self.extract_paths(archives, here);
    }

    pub(super) fn extract_paths(self: &Rc<Self>, archives: Vec<PathBuf>, here: bool) {
        let (Some(cur), Some(win)) = (self.cur_dir(), self.window()) else { return };
        let this = self.clone();
        glib::spawn_future_local(async move {
            let dest = if here {
                cur
            } else {
                let dlg = gtk::FileDialog::new();
                dlg.set_title("Extract to…");
                dlg.set_initial_folder(Some(&gio::File::for_path(&cur)));
                match dlg.select_folder_future(Some(&win)).await.ok().and_then(|f| f.path()) {
                    Some(p) => p,
                    None => return,
                }
            };
            for a in archives {
                this.extract_one(a, dest.clone(), None);
            }
        });
    }

    fn extract_one(self: &Rc<Self>, archive_path: PathBuf, dest: PathBuf, pass: Option<String>) {
        let name = archive_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (a, d, p) = (archive_path.clone(), dest.clone(), pass.clone());
        let this = self.clone();
        self.env.ops.run_job(
            &format!("Extracting {name}"),
            move |cancel| archive::extract_smart(&a, &d, p.as_deref(), &cancel),
            move |r| match r {
                Ok(shown) => {
                    this.env.ops.toast(&format!("Extracted {name}"));
                    if this.cur_dir().as_deref() == shown.parent() {
                        this.reveal_soon(shown);
                    }
                }
                Err(ArchiveError::NeedsPassphrase) => {
                    let Some(win) = this.window() else { return };
                    let retry = pass.is_some();
                    let t = this.clone();
                    glib::spawn_future_local(async move {
                        if let Some(p) = archive_dialogs::prompt_passphrase(&win, &name, retry).await {
                            t.extract_one(archive_path, dest, Some(p));
                        }
                    });
                }
                Err(ArchiveError::Cancelled) => this.env.ops.toast("Cancelled"),
                Err(e) => this.env.ops.toast(&format!("Could not extract {name}: {e}")),
            },
        );
    }

    pub fn browse_archive(self: &Rc<Self>) {
        let paths = self.selected_paths();
        if paths.len() == 1 && self.archive_selection() {
            self.browse_path(paths[0].clone(), None);
        }
    }

    fn browse_path(self: &Rc<Self>, path: PathBuf, pass: Option<String>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (p, ps) = (path.clone(), pass.clone());
        let this = self.clone();
        self.env.ops.run_job(
            &format!("Reading {name}"),
            move |_cancel| archive::list(&p, ps.as_deref()),
            move |r| {
                let Some(win) = this.window() else { return };
                match r {
                    Ok(entries) => {
                        let (t, p) = (this.clone(), path.clone());
                        archive_dialogs::browse(
                            &win,
                            &name,
                            entries,
                            Rc::new(move |here| t.extract_paths(vec![p.clone()], here)),
                        );
                    }
                    Err(ArchiveError::NeedsPassphrase) => {
                        let retry = pass.is_some();
                        glib::spawn_future_local(async move {
                            if let Some(p) = archive_dialogs::prompt_passphrase(&win, &name, retry).await {
                                this.browse_path(path, Some(p));
                            }
                        });
                    }
                    Err(e) => this.env.ops.toast(&format!("Could not read {name}: {e}")),
                }
            },
        );
    }
}

#[cfg(debug_assertions)]
impl Pane {
    /// Development aids for screenshots: `FM_SELECT=name` selects an item and
    /// `FM_MENU=item|bg` opens a context menu once the first folder has loaded.
    pub(super) fn debug_hooks(self: &Rc<Self>) {
        thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        if DONE.replace(true) {
            return;
        }
        if let Ok(n) = std::env::var("FM_SELECT") {
            self.select_visible(&n);
        }
        if let Ok(kind) = std::env::var("FM_MENU") {
            let w = self.me.clone();
            glib::timeout_add_local_once(
                Duration::from_millis(std::env::var("FM_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(600)),
                move || {
                    let Some(p) = w.upgrade() else { return };
                    let model = if kind == "item" { p.item_menu() } else { p.bg_menu() };
                    let y = std::env::var("FM_Y").ok().and_then(|v| v.parse().ok()).unwrap_or(180.0);
                    p.popup_menu(&model, p.stack.upcast_ref(), 300.0, y);
                },
            );
        }
    }
}
