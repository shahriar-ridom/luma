//! One browsing pane: location bar, the three views, status bar, history and
//! file operations. A tab holds one pane, or two when split.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::Duration,
};

use crate::application::state::AppState;
use crate::core::filesystem::{self, LoadToken};
use crate::core::navigation::History;
use crate::core::operations::trash::{TRASH_URI, is_trash};
use crate::core::operations::undo::UndoAction;
use crate::core::operations::{OpKind, OpRequest};
use crate::integrations::git::{FileState, RepoStatus};
use crate::services::thumbnail::Thumbnailer;
use crate::ui::{
    breadcrumb::Breadcrumb,
    clipboard,
    column_view::MillerColumns,
    dialogs,
    file_grid::{FileGrid, MAX_ICON, MIN_ICON},
    file_list::{FileList, SortKey},
    gallery_view::GalleryView,
    interact::{self, ItemCtx},
    operations::OpsUi,
    preview::Target,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneEvent {
    Navigated,
    Selection,
    Focused,
    Status,
    /// Tags were edited (the sidebar's tag list needs a refresh).
    Tags,
}

/// Window-level services a pane needs. `notify` is bound by the window after
/// construction.
pub struct PaneEnv {
    pub ops: OpsUi,
    pub window: glib::WeakRef<gtk::Window>,
    pub state: Rc<AppState>,
    pub notify: RefCell<Option<Rc<dyn Fn(&Rc<Pane>, PaneEvent)>>>,
}

mod actions;
mod ext_actions;
mod git_actions;

/// How new folder contents arrive: pushed in from the right when going deeper,
/// from the left when going back, a quick fade for reloads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NavAnim {
    Forward,
    Back,
    Fade,
}

pub struct Pane {
    env: Rc<PaneEnv>,
    pub root: gtk::Box,
    history: RefCell<History>,
    token: LoadToken,
    list: FileList,
    grid: FileGrid,
    /// Kept alive here; its signal handlers only hold weak references.
    _gallery: Rc<GalleryView>,
    columns: Rc<MillerColumns>,
    trash_view: Rc<crate::ui::trash_view::TrashView>,
    stack: gtk::Stack,
    breadcrumb: Breadcrumb,
    path_stack: gtk::Stack,
    path_entry: gtk::Entry,
    zoom: gtk::Scale,
    status: gtk::Label,
    free: gtk::Label,
    view: RefCell<String>,
    sort: Cell<(SortKey, bool)>,
    show_hidden: Cell<bool>,
    query: RefCell<String>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    pending: RefCell<HashSet<String>>,
    flush_scheduled: Cell<bool>,
    pending_select: RefCell<Option<String>>,
    tags: Rc<RefCell<std::collections::HashMap<String, Vec<String>>>>,
    git: Rc<RefCell<std::collections::HashMap<String, FileState>>>,
    repo: RefCell<Option<RepoStatus>>,
    repo_label: gtk::Label,
    git_gen: Cell<u64>,
    nav_anim: Cell<NavAnim>,
    me: Weak<Pane>,
}

/// Turn what was typed in the location bar into a path: `~` is the home
/// folder, `file://` URIs are accepted, and relative paths are taken from `cwd`.
fn resolve_location(text: &str, cwd: Option<&Path>) -> PathBuf {
    let t = text.trim();
    if t.starts_with("file://")
        && let Some(p) = gio::File::for_uri(t).path()
    {
        return p;
    }
    let home = glib::home_dir;
    let p = match t.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(t),
    };
    match cwd {
        Some(c) if p.is_relative() => c.join(p),
        _ => p,
    }
}

/// Content type of a file, read off the main thread.
async fn content_type_of(path: &Path) -> String {
    let f = gio::File::for_path(path);
    f.query_info_future("standard::content-type", gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
        .await
        .ok()
        .and_then(|i| i.content_type())
        .map(|c| c.to_string())
        .unwrap_or_else(|| gio::content_type_guess(Some(path), None).0.to_string())
}

/// True when every file lives on the same filesystem as `dest` (a move is then a rename).
/// Stats files, so call it off the main thread.
fn same_device(files: &[PathBuf], dest: &Path) -> bool {
    let Ok(d) = std::fs::metadata(dest) else { return false };
    files.iter().all(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.dev() == d.dev()))
}

impl Pane {
    pub fn new(env: Rc<PaneEnv>) -> Rc<Self> {
        Rc::new_cyclic(|weak: &Weak<Pane>| Self::build(env, weak.clone()))
    }

    fn build(env: Rc<PaneEnv>, weak: Weak<Pane>) -> Self {
        // ---- views -----------------------------------------------------------
        let w = weak.clone();
        let open: Rc<dyn Fn(&gio::FileInfo)> = Rc::new(move |info| {
            let Some(p) = w.upgrade() else { return };
            let Some(cur) = p.cur_dir() else { return };
            let path = cur.join(info.name());
            p.open_path(path, filesystem::is_dir(info));
        });
        let w = weak.clone();
        let ctx = ItemCtx {
            selection: Rc::default(),
            dir: Rc::default(),
            on_context: {
                let w = weak.clone();
                Rc::new(move |wd, x, y| {
                    if let Some(pane) = w.upgrade() {
                        pane.popup_menu(&pane.item_menu(), wd, x, y);
                    }
                })
            },
            on_drop: Rc::new(move |files, dest, copy| {
                if let Some(p) = w.upgrade() {
                    p.handle_drop(files, dest, copy);
                }
            }),
            item_claimed: Rc::default(),
            tags: Rc::default(),
            git: Rc::default(),
        };
        let o = open.clone();
        let list = FileList::new(&ctx, move |i| o(i));
        let o = open.clone();
        let thumbs = Thumbnailer::default();
        let grid = FileGrid::new(&list.selection, &ctx, thumbs.clone(), move |i| o(i));
        let gallery = GalleryView::new(&list.selection, &ctx, thumbs);
        let w = weak.clone();
        let columns = MillerColumns::new(move |p| {
            if let Some(pane) = w.upgrade() {
                pane.open_path(p, false);
            }
        });

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);
        // Only the visible view counts toward the minimum size (the gallery is tall).
        stack.set_vhomogeneous(false);
        stack.set_hhomogeneous(false);
        stack.add_named(&grid.widget, Some("grid"));
        stack.add_named(&list.widget, Some("list"));
        stack.add_named(&columns.widget, Some("column"));
        stack.add_named(&gallery.widget, Some("gallery"));
        let trash_view = crate::ui::trash_view::TrashView::new(env.ops.clone(), env.window.clone());
        stack.add_named(&trash_view.widget, Some("trash"));
        stack.set_visible_child_name("list");
        stack.set_vexpand(true);

        // Background menu + drop into this folder.
        let bg = gtk::GestureClick::new();
        bg.set_button(3);
        let claimed = ctx.item_claimed.clone();
        let bg_weak = weak.clone();
        bg.connect_pressed(move |g, _, x, y| {
            if claimed.replace(false) {
                return;
            }
            if let (Some(pane), Some(wd)) = (bg_weak.upgrade(), g.widget()) {
                pane.popup_menu(&pane.bg_menu(), &wd, x, y);
            }
        });
        stack.add_controller(bg);

        // A click on empty space (not on an item) clears the selection, like
        // other file managers. Ctrl/Shift keep it so rubberband can extend it.
        let blank = gtk::GestureClick::new();
        blank.set_button(0);
        blank.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = weak.clone();
        blank.connect_pressed(move |g, _, x, y| {
            let Some(p) = w.upgrade() else { return };
            if !matches!(p.view.borrow().as_str(), "list" | "grid") {
                return;
            }
            let mods = g.current_event_state();
            if mods.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK) {
                return;
            }
            let hit = p.stack.pick(x, y, gtk::PickFlags::DEFAULT);
            if hit.is_some_and(|h| matches!(h.css_name().as_str(), "listview" | "gridview")) {
                p.list.selection.unselect_all();
            }
        });
        stack.add_controller(blank);
        let w = weak.clone();
        stack.add_controller(interact::drop_target(ctx.on_drop.clone(), move || w.upgrade().and_then(|p| p.cur_dir())));

        // ---- location bar --------------------------------------------------
        let path_entry = gtk::Entry::new();
        path_entry.set_hexpand(true);
        let w = weak.clone();
        let breadcrumb = Breadcrumb::new(move |p| {
            if let Some(pane) = w.upgrade() {
                pane.navigate(p);
            }
        });
        let path_stack = gtk::Stack::new();
        path_stack.add_named(&breadcrumb.widget, Some("crumbs"));
        path_stack.add_named(&path_entry, Some("entry"));
        path_stack.set_hexpand(true);
        let path_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        path_bar.set_margin_start(8);
        path_bar.set_margin_end(8);
        path_bar.set_margin_top(6);
        path_bar.set_margin_bottom(6);
        path_bar.append(&path_stack);
        path_bar.add_css_class("pane-pathbar");

        let w = weak.clone();
        path_entry.connect_activate(move |e| {
            let Some(p) = w.upgrade() else { return };
            let path = resolve_location(e.text().as_str(), p.cur_dir().as_deref());
            let e = e.clone();
            glib::spawn_future_local(async move {
                if filesystem::is_dir_async(&path).await {
                    p.navigate(path);
                } else {
                    e.add_css_class("error");
                    glib::timeout_add_local_once(Duration::from_millis(800), move || e.remove_css_class("error"));
                }
            });
        });
        // Escape leaves the entry without navigating.
        let esc = gtk::EventControllerKey::new();
        let w = weak.clone();
        esc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(p) = w.upgrade() {
                    p.leave_location();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        path_entry.add_controller(esc);

        // ---- status row ----------------------------------------------------
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_margin_start(12);
        status.set_margin_end(12);
        status.set_margin_top(6);
        status.set_margin_bottom(6);
        status.add_css_class("dim-label");
        status.add_css_class("fm-status");
        // Status texts shrink with the pane (split view) instead of forcing
        // the window wider than the screen.
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.set_width_chars(6);
        let zoom = gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_ICON as f64, MAX_ICON as f64, 8.0);
        zoom.set_value(grid.icon_size() as f64);
        zoom.set_draw_value(false);
        zoom.set_width_request(140);
        zoom.set_margin_end(12);
        zoom.set_visible(false);
        let w = weak.clone();
        zoom.connect_value_changed(move |z| {
            if let Some(p) = w.upgrade() {
                p.grid.set_icon_size(z.value() as i32);
            }
        });
        let free = gtk::Label::new(None);
        free.add_css_class("dim-label");
        free.add_css_class("fm-status");
        free.set_ellipsize(gtk::pango::EllipsizeMode::End);
        free.set_width_chars(4);
        free.set_margin_end(12);
        let repo_label = gtk::Label::new(None);
        repo_label.add_css_class("fm-status");
        repo_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        repo_label.set_width_chars(4);
        repo_label.set_margin_end(14);
        repo_label.set_visible(false);
        // In the Trash, the status line is the Trash's summary.
        let (st, w) = (status.clone(), weak.clone());
        trash_view.set_on_summary(move |t| {
            st.set_text(t);
            if let Some(p) = w.upgrade() {
                p.emit(PaneEvent::Status);
            }
        });
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_row.append(&status);
        status_row.append(&repo_label);
        status_row.append(&free);
        status_row.append(&zoom);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.append(&path_bar);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&stack);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&status_row);

        // Selection and focus tracking.
        let w = weak.clone();
        list.selection.connect_selection_changed(move |_, _, _| {
            if let Some(p) = w.upgrade() {
                p.update_status();
                p.emit(PaneEvent::Selection);
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let w = weak.clone();
        focus.connect_contains_focus_notify(move |c| {
            if c.contains_focus()
                && let Some(p) = w.upgrade()
            {
                p.emit(PaneEvent::Focused);
            }
        });
        root.add_controller(focus);
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = weak.clone();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(p) = w.upgrade() {
                p.emit(PaneEvent::Focused);
            }
        });
        root.add_controller(click);

        Self {
            env,
            root,
            history: RefCell::default(),
            token: LoadToken::default(),
            list,
            grid,
            _gallery: gallery,
            trash_view,
            columns,
            stack,
            breadcrumb,
            path_stack,
            path_entry,
            zoom,
            status,
            free,
            view: RefCell::new("list".into()),
            sort: Cell::new((SortKey::Name, true)),
            show_hidden: Cell::new(false),
            query: RefCell::default(),
            monitor: RefCell::default(),
            pending: RefCell::default(),
            flush_scheduled: Cell::new(false),
            pending_select: RefCell::default(),
            tags: ctx.tags.clone(),
            git: ctx.git.clone(),
            repo: RefCell::default(),
            repo_label,
            git_gen: Cell::new(0),
            nav_anim: Cell::new(NavAnim::Fade),
            me: weak,
        }
    }

    fn emit(&self, ev: PaneEvent) {
        let cb = self.env.notify.borrow().clone();
        if let (Some(cb), Some(me)) = (cb, self.me.upgrade()) {
            cb(&me, ev);
        }
    }

    // ---- state accessors ---------------------------------------------------

    /// The folder being shown. `None` in the Trash, which isn't a folder,
    /// so file actions (new folder, paste, …) have nowhere to go there.
    pub fn cur_dir(&self) -> Option<PathBuf> {
        self.location().filter(|p| !is_trash(p))
    }
    /// Where the pane is, including the Trash.
    pub fn location(&self) -> Option<PathBuf> {
        self.history.borrow().current().map(Path::to_path_buf)
    }
    pub fn in_trash(&self) -> bool {
        self.location().is_some_and(|p| is_trash(&p))
    }
    /// Re-read the Trash if it's showing (after deleting or restoring).
    pub fn reload_trash(&self) {
        if self.in_trash() {
            self.trash_view.reload();
        }
    }
    /// (items, bytes) in the Trash, as last listed.
    pub fn trash_summary(&self) -> (usize, u64) {
        self.trash_view.summary()
    }
    pub fn can_back(&self) -> bool {
        self.history.borrow().can_back()
    }
    pub fn can_forward(&self) -> bool {
        self.history.borrow().can_forward()
    }
    pub fn can_up(&self) -> bool {
        self.cur_dir().is_some_and(|d| d.parent().is_some())
    }
    pub fn view(&self) -> String {
        self.view.borrow().clone()
    }
    pub fn sort(&self) -> (SortKey, bool) {
        self.sort.get()
    }
    pub fn hidden(&self) -> bool {
        self.show_hidden.get()
    }
    pub fn query(&self) -> String {
        self.query.borrow().clone()
    }
    pub fn selected_count(&self) -> u32 {
        self.list.selected_count()
    }
    pub fn selection_infos(&self) -> Vec<gio::FileInfo> {
        self.list.selected_infos()
    }
    pub fn visible_count(&self) -> u32 {
        self.list.visible_count()
    }

    pub fn set_view(&self, name: &str) {
        *self.view.borrow_mut() = name.to_string();
        // The Trash keeps its own view; the choice applies when leaving it.
        if self.in_trash() {
            return;
        }
        self.stack.set_visible_child_name(name);
        self.zoom.set_visible(name == "grid");
        self.ensure_gallery_selection();
    }

    /// The gallery always shows something: select the first item if none is.
    fn ensure_gallery_selection(&self) {
        if *self.view.borrow() == "gallery" && self.list.selected_count() == 0 && self.list.visible_count() > 0 {
            self.list.selection.select_item(0, true);
        }
    }

    /// Move the single selection by `delta` rows (used by Quick Look).
    pub fn step_selection(&self, delta: i32) {
        let n = self.list.selection.n_items() as i32;
        if n == 0 {
            return;
        }
        let bits = self.list.selection.selection();
        let cur = if bits.is_empty() { if delta > 0 { -1 } else { n } } else { bits.minimum() as i32 };
        self.list.selection.select_item((cur + delta).clamp(0, n - 1) as u32, true);
    }

    /// First selected entry with its full path, for the preview panel.
    pub fn preview_target(&self) -> Option<Target> {
        let dir = self.cur_dir()?;
        self.list.selected_infos().into_iter().next().map(|i| (dir.join(i.name()), i))
    }
    pub fn set_sort(&self, key: SortKey, asc: bool) {
        self.sort.set((key, asc));
        self.list.set_sort(key, asc);
    }
    pub fn set_hidden(&self, v: bool) {
        self.show_hidden.set(v);
        self.list.set_show_hidden(v);
        self.columns.set_show_hidden(v);
        self.update_status();
    }
    pub fn set_query(&self, q: &str) {
        *self.query.borrow_mut() = q.to_string();
        self.list.set_query(q);
        self.update_status();
    }
    pub fn focus_location(&self) {
        self.path_stack.set_visible_child_name("entry");
        self.path_entry.grab_focus();
    }
    /// Back to the breadcrumbs, dropping whatever was typed, focus to the files.
    fn leave_location(&self) {
        if let Some(d) = self.cur_dir() {
            self.path_entry.set_text(&d.to_string_lossy());
        }
        self.path_stack.set_visible_child_name("crumbs");
        if let Some(v) = self.stack.visible_child() {
            v.child_focus(gtk::DirectionType::TabForward);
        }
    }
    pub fn select_all(&self) {
        self.list.selection.select_all();
    }
    pub fn set_active_style(&self, active: bool) {
        if active { self.root.add_css_class("pane-active") } else { self.root.remove_css_class("pane-active") }
    }

    // ---- navigation --------------------------------------------------------

    pub fn navigate(self: &Rc<Self>, path: PathBuf) {
        self.navigate_with(path, NavAnim::Forward);
    }

    fn navigate_with(self: &Rc<Self>, path: PathBuf, anim: NavAnim) {
        self.nav_anim.set(anim);
        self.history.borrow_mut().visit(path.clone());
        if is_trash(&path) {
            self.load(path);
            return;
        }
        if let Err(e) = self.env.state.db.record_location(&path) {
            tracing::debug!("recent location: {e:#}");
        }
        self.load(path);
    }

    pub fn refresh(self: &Rc<Self>) {
        self.nav_anim.set(NavAnim::Fade);
        if let Some(c) = self.location() {
            self.load(c);
        }
    }
    pub fn go_back(self: &Rc<Self>) {
        self.nav_anim.set(NavAnim::Back);
        let p = self.history.borrow_mut().go_back().map(Path::to_path_buf);
        if let Some(p) = p {
            self.load(p)
        }
    }
    pub fn go_forward(self: &Rc<Self>) {
        self.nav_anim.set(NavAnim::Forward);
        let p = self.history.borrow_mut().go_forward().map(Path::to_path_buf);
        if let Some(p) = p {
            self.load(p)
        }
    }
    pub fn go_up(self: &Rc<Self>) {
        if let Some(p) = self.cur_dir().and_then(|c| c.parent().map(Path::to_path_buf)) {
            self.navigate_with(p, NavAnim::Back)
        }
    }

    /// Play the arrival animation on the view stack (CSS keyframes).
    fn play_nav_animation(&self) {
        let class = match self.nav_anim.get() {
            NavAnim::Forward => "fm-nav-fwd",
            NavAnim::Back => "fm-nav-back",
            NavAnim::Fade => "fm-nav-fade",
        };
        for c in ["fm-nav-fwd", "fm-nav-back", "fm-nav-fade"] {
            self.stack.remove_css_class(c);
        }
        let stack = self.stack.clone();
        // Re-adding on the next frame lets GTK restart the animation.
        glib::idle_add_local_once(move || {
            stack.add_css_class(class);
            let s = stack.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || s.remove_css_class(class));
        });
        self.nav_anim.set(NavAnim::Fade);
    }

    fn load_trash(self: &Rc<Self>) {
        self.play_nav_animation();
        let path = PathBuf::from(TRASH_URI);
        self.path_entry.set_text("");
        self.breadcrumb.set_path(&path);
        self.path_stack.set_visible_child_name("crumbs");
        if let Some(m) = self.monitor.take() {
            m.cancel();
        }
        self.token.next(); // drop any folder load still running
        self.list.store.remove_all();
        self.stack.set_visible_child_name("trash");
        self.zoom.set_visible(false);
        self.free.set_text("");
        self.repo_label.set_visible(false);
        self.status.set_text("Loading…");
        self.emit(PaneEvent::Navigated);
        self.trash_view.reload();
    }

    fn load(self: &Rc<Self>, path: PathBuf) {
        if is_trash(&path) {
            return self.load_trash();
        }
        // Coming back from the Trash: show the chosen view again.
        let view = self.view.borrow().clone();
        if self.stack.visible_child_name().as_deref() != Some(view.as_str()) {
            self.stack.set_visible_child_name(&view);
            self.zoom.set_visible(view == "grid");
        }
        self.play_nav_animation();
        self.path_entry.set_text(&path.to_string_lossy());
        self.breadcrumb.set_path(&path);
        self.path_stack.set_visible_child_name("crumbs");
        *self.list.dir.borrow_mut() = path.clone();
        self.columns.set_root(path.clone());
        self.watch(&path);
        self.list.store.remove_all();
        self.reload_tags();
        self.refresh_git();
        self.status.set_text("Loading…");
        self.emit(PaneEvent::Navigated);
        let (w, p) = (self.me.clone(), path.clone());
        glib::spawn_future_local(async move {
            let free = crate::services::storage::free_space(&p).await;
            if let Some(pane) = w.upgrade()
                && pane.cur_dir().as_deref() == Some(p.as_path())
            {
                pane.free.set_text(
                    &free.map(|b| format!("Free space: {}", filesystem::human_size(b as i64))).unwrap_or_default(),
                );
            }
        });
        let id = self.token.next();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let dir = gio::File::for_path(&path);
            let t = this.token.clone();
            let r = filesystem::load_dir(&dir, &t, id, |batch| {
                let items: Vec<glib::Object> = batch.into_iter().map(Into::into).collect();
                this.list.store.extend_from_slice(&items);
            })
            .await;
            if !this.token.is_current(id) {
                return;
            }
            match r {
                Ok(()) => {
                    this.update_status();
                    this.ensure_gallery_selection();
                    this.refresh_git();
                    #[cfg(debug_assertions)]
                    this.debug_hooks();
                    let pending = this.pending_select.take();
                    if let Some(n) = pending {
                        this.select_visible(&n);
                    }
                }
                Err(e) => this.status.set_text(&format!("Cannot open folder: {}", e.message())),
            }
            this.emit(PaneEvent::Status);
        });
    }

    fn update_status(&self) {
        let total = self.list.visible_count();
        let sel = self.list.selected_count();
        let text = if sel > 0 {
            let bytes = self.list.selected_bytes();
            let extra = if bytes > 0 { format!(" ({})", filesystem::human_size(bytes)) } else { String::new() };
            format!("{sel} of {total} selected{extra}")
        } else {
            format!("{total} items")
        };
        self.status.set_text(&text);
    }

    // ---- live updates ------------------------------------------------------

    fn watch(self: &Rc<Self>, path: &Path) {
        if let Some(m) = self.monitor.take() {
            m.cancel();
        }
        self.pending.borrow_mut().clear();
        let Ok(m) = gio::File::for_path(path).monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        m.connect_changed(move |_, f, other, _| {
            let Some(this) = weak.upgrade() else { return };
            for f in std::iter::once(f).chain(other) {
                if let Some(n) = f.basename() {
                    this.pending.borrow_mut().insert(n.to_string_lossy().into_owned());
                }
            }
            this.schedule_flush();
        });
        *self.monitor.borrow_mut() = Some(m);
    }

    fn schedule_flush(self: &Rc<Self>) {
        if self.flush_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            let Some(this) = weak.upgrade() else { return };
            this.flush_scheduled.set(false);
            let names: Vec<String> = this.pending.borrow_mut().drain().collect();
            let Some(dir) = this.cur_dir() else { return };
            if names.len() > 200 {
                this.load(dir);
                return;
            }
            glib::spawn_future_local(async move {
                for n in names {
                    let f = gio::File::for_path(dir.join(&n));
                    let r = f
                        .query_info_future(
                            filesystem::LIST_ATTRS,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await;
                    if this.cur_dir().as_deref() != Some(dir.as_path()) {
                        return;
                    }
                    match r {
                        Ok(info) => this.list.upsert(info),
                        Err(e) if e.matches(gio::IOErrorEnum::NotFound) => this.list.remove_name(&n),
                        Err(_) => {}
                    }
                }
                this.update_status();
                this.reload_tags();
                this.refresh_git();
                if this.stack.visible_child_name().as_deref() == Some("column") {
                    this.columns.set_root(dir);
                }
            });
        });
    }

    // ---- file actions ------------------------------------------------------

    fn window(&self) -> Option<gtk::Window> {
        self.env.window.upgrade()
    }

    pub fn launch(&self, path: &Path) {
        if let Err(e) = self.env.state.db.record_file(path) {
            tracing::debug!("recent file: {e:#}");
        }
        let Some(me) = self.me.upgrade() else { return };
        let path = path.to_path_buf();
        glib::spawn_future_local(async move {
            let ct = content_type_of(&path).await;
            match gio::AppInfo::default_for_type(&ct, false) {
                // A default exists: use it without asking.
                Some(app) => {
                    if let Err(e) = me.launch_with(&app, &path) {
                        me.env.ops.toast(&format!("Could not open with {}: {}", app.display_name(), e.message()));
                        me.choose_app(path, ct, false).await;
                    }
                }
                // First time for this type: ask once, offering to remember the choice.
                None => me.choose_app(path, ct, true).await,
            }
        });
    }

    fn launch_with(&self, app: &gio::AppInfo, path: &Path) -> Result<(), glib::Error> {
        let ctx = self.window().map(|w| WidgetExt::display(&w).app_launch_context());
        app.launch(&[gio::File::for_path(path)], ctx.as_ref())
    }

    /// Show the app chooser for `path`; remember the app as default if asked.
    pub(crate) async fn choose_app(self: &Rc<Self>, path: PathBuf, ct: String, default_on: bool) {
        let Some(win) = self.window() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(choice) = dialogs::open_with::choose(&win, &name, &ct, default_on).await else { return };
        if choice.make_default
            && let Err(e) = choice.app.set_as_default_for_type(&ct)
        {
            self.env.ops.toast(&format!("Could not set the default app: {}", e.message()));
        }
        if let Err(e) = self.launch_with(&choice.app, &path) {
            self.env.ops.toast(&format!("Could not open with {}: {}", choice.app.display_name(), e.message()));
        }
    }

    /// Re-read tags for this folder; views are rebound only if something changed.
    pub fn reload_tags(&self) {
        let Some(d) = self.cur_dir() else { return };
        let fresh = self.env.state.db.tags_in_dir(&d).unwrap_or_default();
        if *self.tags.borrow() != fresh {
            *self.tags.borrow_mut() = fresh;
            let n = self.list.store.n_items();
            if n > 0 {
                self.list.store.items_changed(0, n, n);
            }
        }
    }

    fn selected_files(&self) -> Vec<gio::File> {
        let Some(d) = self.cur_dir() else { return vec![] };
        self.list.selected_infos().into_iter().map(|i| gio::File::for_path(d.join(i.name()))).collect()
    }

    pub fn open_selected(self: &Rc<Self>) {
        let Some(d) = self.cur_dir() else { return };
        let infos = self.list.selected_infos();
        if let [one] = infos.as_slice()
            && filesystem::is_dir(one)
        {
            return self.navigate(d.join(one.name()));
        }
        for i in infos.iter().filter(|i| !filesystem::is_dir(i)) {
            self.open_path(d.join(i.name()), false);
        }
    }

    pub fn copy_cut(&self, cut: bool) {
        let files = self.selected_files();
        if !files.is_empty() {
            clipboard::set_files(&self.stack.clipboard(), &files, cut);
        }
    }

    pub fn paste(self: &Rc<Self>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let cb = this.stack.clipboard();
            let Some((files, cut)) = clipboard::get_files(&cb).await else {
                this.env.ops.toast("Nothing to paste");
                return;
            };
            let Some(dir) = this.cur_dir() else { return };
            this.submit_transfer(files, &dir, if cut { OpKind::Move } else { OpKind::Copy });
            if cut {
                clipboard::clear(&cb);
            }
        });
    }

    fn submit_transfer(&self, files: Vec<gio::File>, dest: &Path, kind: OpKind) {
        if files.is_empty() {
            return;
        }
        self.env.ops.submit(OpRequest {
            kind,
            sources: files.iter().map(|f| f.uri().to_string()).collect(),
            dest: Some(gio::File::for_path(dest).uri().to_string()),
        });
    }

    /// Ctrl forces copy; otherwise move within a filesystem, copy across.
    pub fn drop_files(self: &Rc<Self>, files: Vec<gio::File>, dest: PathBuf, copy: bool) {
        self.handle_drop(files, dest, copy)
    }

    fn handle_drop(self: &Rc<Self>, files: Vec<gio::File>, dest: PathBuf, copy: bool) {
        // Dropped on the Trash: move them to the trash.
        if is_trash(&dest) {
            if !files.is_empty() {
                self.env.ops.submit(OpRequest {
                    kind: OpKind::Trash,
                    sources: files.iter().map(|f| f.uri().to_string()).collect(),
                    dest: None,
                });
            }
            return;
        }
        let dest_file = gio::File::for_path(&dest);
        let files: Vec<_> = files.into_iter().filter(|f| !f.equal(&dest_file)).collect();
        if copy {
            self.submit_transfer(files, &dest, OpKind::Copy);
            return;
        }
        // Moving within one filesystem is a rename; across filesystems it copies.
        // The check stats files, so it runs off the main thread.
        let paths: Option<Vec<PathBuf>> = files.iter().map(|f| f.path()).collect();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let d = dest.clone();
            let same = match paths {
                Some(p) => gio::spawn_blocking(move || same_device(&p, &d)).await.unwrap_or(false),
                None => false,
            };
            this.submit_transfer(files, &dest, if same { OpKind::Move } else { OpKind::Copy });
        });
    }

    pub fn trash(&self) {
        let files = self.selected_files();
        if !files.is_empty() {
            self.env.ops.submit(OpRequest {
                kind: OpKind::Trash,
                sources: files.iter().map(|f| f.uri().to_string()).collect(),
                dest: None,
            });
        }
    }

    pub fn delete(self: &Rc<Self>) {
        let files = self.selected_files();
        let (Some(win), Some(first)) = (self.window(), files.first()) else { return };
        let name = first.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let this = self.clone();
        let n = files.len();
        glib::spawn_future_local(async move {
            if dialogs::confirm_delete(&win, n, &name).await {
                this.env.ops.submit(OpRequest {
                    kind: OpKind::Delete,
                    sources: files.iter().map(|f| f.uri().to_string()).collect(),
                    dest: None,
                });
            }
        });
    }

    pub fn rename(self: &Rc<Self>) {
        let mut infos = self.list.selected_infos();
        if infos.len() > 1 {
            return self.batch_rename();
        }
        if infos.len() != 1 {
            return;
        }
        let info = infos.remove(0);
        let (Some(win), Some(dir)) = (self.window(), self.cur_dir()) else { return };
        let old = info.display_name().to_string();
        let file = gio::File::for_path(dir.join(info.name()));
        let this = self.clone();
        glib::spawn_future_local(async move {
            let Some(new) = dialogs::prompt_name(&win, "Rename", "Rename", &old).await else { return };
            if new == old {
                return;
            }
            // GIO refuses to rename onto an existing name, so nothing is overwritten.
            match file.set_display_name_future(&new, glib::Priority::DEFAULT).await {
                Ok(renamed) => {
                    if let (Some(from), Some(to)) = (file.path(), renamed.path()) {
                        let st = &this.env.state;
                        let _ = st.db.move_tags(&from, &to);
                        st.undo.push("Rename", UndoAction::Rename(vec![(to, from)]));
                    }
                    this.reload_tags();
                }
                Err(e) => this.env.ops.toast(&format!("Could not rename: {}", e.message())),
            }
        });
    }

    pub fn create(self: &Rc<Self>, folder: bool) {
        let (Some(win), Some(dir)) = (self.window(), self.cur_dir()) else { return };
        let this = self.clone();
        glib::spawn_future_local(async move {
            let (head, default) =
                if folder { ("New Folder", "Untitled Folder") } else { ("New File", "Untitled File") };
            let Some(name) = dialogs::prompt_name(&win, head, "Create", default).await else { return };
            let f = gio::File::for_path(dir.join(&name));
            let r = if folder {
                f.make_directory_future(glib::Priority::DEFAULT).await
            } else {
                match f.create_future(gio::FileCreateFlags::NONE, glib::Priority::DEFAULT).await {
                    Ok(stream) => stream.close_future(glib::Priority::DEFAULT).await,
                    Err(e) => Err(e),
                }
            };
            match r {
                Ok(()) => {
                    let label = if folder { "New Folder" } else { "New File" };
                    this.env.state.undo.push(label, UndoAction::TrashCreated(vec![dir.join(&name)]));
                }
                Err(e) => this.env.ops.toast(&format!("Could not create “{name}”: {}", e.message())),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_location;
    use std::path::{Path, PathBuf};

    #[test]
    fn location_bar_paths() {
        let home = gtk::glib::home_dir();
        let cwd = Path::new("/srv/data");
        assert_eq!(resolve_location("~", Some(cwd)), home);
        assert_eq!(resolve_location(" ~/Documents ", Some(cwd)), home.join("Documents"));
        assert_eq!(resolve_location("/etc", Some(cwd)), PathBuf::from("/etc"));
        assert_eq!(resolve_location("sub/dir", Some(cwd)), cwd.join("sub/dir"));
        assert_eq!(resolve_location("file:///tmp/a%20b", Some(cwd)), PathBuf::from("/tmp/a b"));
        // "~user" is not expanded; it is just a relative name.
        assert_eq!(resolve_location("~bob", Some(cwd)), cwd.join("~bob"));
    }
}
