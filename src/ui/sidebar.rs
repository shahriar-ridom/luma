use adw::prelude::*;
use gtk::{gdk, gio, glib, glib::UserDirectory};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

use crate::application::state::AppState;
use crate::integrations::{cloud, network};
use crate::services::mounts;
use crate::ui::{
    dialogs,
    interact::{self, DropFn},
    operations::OpsUi,
};

enum Target {
    Path(PathBuf),
    /// An unmounted volume: mounted on click.
    Volume(gio::Volume),
    /// A saved network address: connected on click.
    Connection(String),
    ConnectNew,
    /// A Google Drive (rclone remote): mounted on click.
    Cloud(String),
    AddCloud,
    Tag(String),
}

struct Entry {
    target: Target,
    /// Used to highlight the row for the current folder.
    path: Option<PathBuf>,
}

pub struct Sidebar {
    pub widget: gtk::ScrolledWindow,
    list: gtk::ListBox,
    entries: RefCell<Vec<Entry>>,
    state: Rc<AppState>,
    window: glib::WeakRef<gtk::Window>,
    on_nav: Rc<dyn Fn(PathBuf)>,
    on_drop: DropFn,
    ops: OpsUi,
    on_reveal: Rc<dyn Fn(PathBuf)>,
    cloud: RefCell<Vec<cloud::Remote>>,
    current: RefCell<PathBuf>,
    rebuild_pending: Cell<bool>,
    me: Weak<Sidebar>,
}

type MenuItems = Vec<(String, Rc<dyn Fn()>)>;

impl Sidebar {
    pub fn new(
        state: Rc<AppState>,
        window: &gtk::Window,
        ops: OpsUi,
        on_nav: impl Fn(PathBuf) + 'static,
        on_drop: DropFn,
        on_reveal: Rc<dyn Fn(PathBuf)>,
    ) -> Rc<Self> {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_header_func(|row, prev| {
            let title = row.widget_name();
            let prev_title = prev.map(|r| r.widget_name());
            if Some(&title) != prev_title.as_ref() {
                let l = gtk::Label::new(Some(title.as_str()));
                l.add_css_class("fm-section");
                l.set_xalign(0.0);
                l.set_margin_top(12);
                l.set_margin_start(12);
                l.set_margin_bottom(4);
                row.set_header(Some(&l));
            } else {
                row.set_header(None::<&gtk::Widget>);
            }
        });
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&list));
        sw.set_hscrollbar_policy(gtk::PolicyType::Never);
        let window = window.downgrade();
        let this = Rc::new_cyclic(|me| Self {
            widget: sw,
            list,
            entries: RefCell::default(),
            state,
            window,
            on_nav: Rc::new(on_nav),
            on_drop,
            ops,
            on_reveal,
            cloud: RefCell::default(),
            current: RefCell::default(),
            rebuild_pending: Cell::new(false),
            me: me.clone(),
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            if let Some(s) = weak.upgrade() {
                s.activate(row.index() as usize);
            }
        });
        // Live device / network changes.
        let vm = gio::VolumeMonitor::get();
        let sched = |w: Weak<Sidebar>| {
            move || {
                if let Some(s) = w.upgrade() {
                    s.schedule_rebuild();
                }
            }
        };
        let w = Rc::downgrade(&this);
        let f = sched(w.clone());
        vm.connect_mount_added(move |_, _| f());
        let f = sched(w.clone());
        vm.connect_mount_removed(move |_, _| f());
        let f = sched(w.clone());
        vm.connect_mount_changed(move |_, _| f());
        let f = sched(w.clone());
        vm.connect_volume_added(move |_, _| f());
        let f = sched(w.clone());
        vm.connect_volume_removed(move |_, _| f());
        let f = sched(w);
        vm.connect_volume_changed(move |_, _| f());
        this.rebuild();
        this.refresh_cloud();
        this
    }

    fn schedule_rebuild(&self) {
        if self.rebuild_pending.replace(true) {
            return;
        }
        let w = self.me.clone();
        glib::idle_add_local_once(move || {
            if let Some(s) = w.upgrade() {
                s.rebuild_pending.set(false);
                s.rebuild();
            }
        });
    }

    fn window(&self) -> Option<gtk::Window> {
        self.window.upgrade()
    }

    fn show_error(&self, heading: &str, body: &str) {
        if let Some(w) = self.window() {
            dialogs::error(&w, heading, body);
        }
    }

    /// Highlight the entry matching `path`, if any.
    pub fn select_path(&self, path: &Path) {
        *self.current.borrow_mut() = path.to_path_buf();
        let idx = self.entries.borrow().iter().position(|e| e.path.as_deref() == Some(path));
        match idx.and_then(|i| self.list.row_at_index(i as i32)) {
            Some(r) => self.list.select_row(Some(&r)),
            None => self.list.unselect_all(),
        }
    }

    fn activate(&self, idx: usize) {
        let entries = self.entries.borrow();
        let Some(e) = entries.get(idx) else { return };
        match &e.target {
            Target::Path(p) => (self.on_nav)(p.clone()),
            Target::Volume(v) => {
                let (v, w) = (v.clone(), self.me.clone());
                drop(entries);
                glib::spawn_future_local(async move {
                    let Some(s) = w.upgrade() else { return };
                    let Some(win) = s.window() else { return };
                    match mounts::mount_volume(&win, &v).await {
                        Ok(p) => (s.on_nav)(p),
                        Err(e) => s.show_error("Could not mount", &e),
                    }
                });
            }
            Target::Connection(uri) => {
                let uri = uri.clone();
                drop(entries);
                self.connect_uri(uri);
            }
            Target::ConnectNew => {
                drop(entries);
                self.connect_dialog();
            }
            Target::Cloud(name) => {
                let name = name.clone();
                drop(entries);
                self.open_cloud(name);
            }
            Target::AddCloud => {
                drop(entries);
                self.add_cloud();
            }
            Target::Tag(t) => {
                let t = t.clone();
                drop(entries);
                if let Some(win) = self.window() {
                    dialogs::tags::browse(&win, &t, self.state.clone(), self.on_reveal.clone());
                }
            }
        }
    }

    // ---- Google Drive (rclone) ---------------------------------------------------

    /// Re-read the configured Drive accounts without blocking the UI.
    pub fn refresh_cloud(&self) {
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let remotes = gio::spawn_blocking(cloud::list_drive_remotes).await.unwrap_or_default();
            if let Some(s) = w.upgrade() {
                *s.cloud.borrow_mut() = remotes;
                s.rebuild();
            }
        });
    }

    fn open_cloud(&self, name: String) {
        let w = self.me.clone();
        let n = name.clone();
        self.ops.run_job(
            &format!("Opening Google Drive “{name}”"),
            move |cancel| cloud::mount(&n, &cancel),
            move |r| {
                let Some(s) = w.upgrade() else { return };
                match r {
                    Ok(p) => {
                        s.rebuild();
                        (s.on_nav)(p);
                    }
                    Err(e) => s.show_error("Could not open Google Drive", &e),
                }
            },
        );
    }

    fn add_cloud(&self) {
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let Some(s) = w.upgrade() else { return };
            let Some(win) = s.window() else { return };
            if cloud::rclone().is_none() {
                dialogs::cloud::explain_missing_rclone(&win);
                return;
            }
            let taken: Vec<String> = s.cloud.borrow().iter().map(|r| r.name.clone()).collect();
            let Some((name, read_only)) = dialogs::cloud::ask_new(&win, &taken).await else { return };
            let (n, w2) = (name.clone(), s.me.clone());
            s.ops.run_job(
                "Signing in to Google (check your browser)",
                move |cancel| cloud::create_remote(&n, read_only, &cancel),
                move |r| {
                    let Some(s) = w2.upgrade() else { return };
                    match r {
                        Ok(()) => {
                            s.refresh_cloud();
                            s.open_cloud(name);
                        }
                        Err(e) if e == "Cancelled" => {}
                        Err(e) => s.show_error("Could not add Google Drive", &e),
                    }
                },
            );
        });
    }

    fn remove_cloud(w: &Weak<Sidebar>, name: String) {
        let w = w.clone();
        glib::spawn_future_local(async move {
            let Some(s) = w.upgrade() else { return };
            let Some(win) = s.window() else { return };
            if !dialogs::cloud::confirm_remove(&win, &name).await {
                return;
            }
            let (n, w2) = (name.clone(), s.me.clone());
            s.ops.run_job(
                "Removing Google Drive",
                move |_c| cloud::delete_remote(&n),
                move |r| {
                    let Some(s) = w2.upgrade() else { return };
                    match r {
                        Ok(()) => s.refresh_cloud(),
                        Err(e) => s.show_error("Could not remove Google Drive", &e),
                    }
                },
            );
        });
    }

    // ---- network ---------------------------------------------------------------

    fn connect_uri(&self, uri: String) {
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let Some(s) = w.upgrade() else { return };
            let Some(win) = s.window() else { return };
            match network::connect(&win, &uri).await {
                Ok(p) => (s.on_nav)(p),
                Err(e) => s.show_error("Could not connect", &e),
            }
        });
    }

    /// Ask for a server address, connect, and optionally remember it.
    pub fn connect_dialog(&self) {
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let Some(s) = w.upgrade() else { return };
            let Some(win) = s.window() else { return };
            let Some(c) = dialogs::connect::ask(&win).await else { return };
            match network::connect(&win, &c.uri).await {
                Ok(p) => {
                    if c.remember {
                        let name = if c.name.is_empty() { network::label_for(&c.uri) } else { c.name };
                        let _ = s.state.db.add_connection(&name, &c.uri);
                    }
                    s.rebuild();
                    (s.on_nav)(p);
                }
                Err(e) => s.show_error("Could not connect", &e),
            }
        });
    }

    // ---- building ----------------------------------------------------------------

    pub fn rebuild(&self) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        self.entries.borrow_mut().clear();

        let path = |p: PathBuf| (Target::Path(p.clone()), Some(p));
        let (t, p) = path(glib::home_dir());
        self.add("Favorites", "fm-home-symbolic", "Home", t, p, None, vec![]);
        for (dir, icon, name) in [
            (UserDirectory::Desktop, "fm-desktop-symbolic", "Desktop"),
            (UserDirectory::Documents, "fm-documents-symbolic", "Documents"),
            (UserDirectory::Downloads, "fm-downloads-symbolic", "Downloads"),
            (UserDirectory::Pictures, "fm-pictures-symbolic", "Pictures"),
            (UserDirectory::Videos, "fm-videos-symbolic", "Videos"),
            (UserDirectory::Music, "fm-music-symbolic", "Music"),
        ] {
            if let Some(p) = glib::user_special_dir(dir).filter(|p| p.is_dir()) {
                let (t, p) = path(p);
                self.add("Favorites", icon, name, t, p, None, vec![]);
            }
        }
        for b in self.state.db.bookmarks().unwrap_or_default() {
            let (t, p) = path(b.path.clone());
            let id = b.id;
            let weak = self.me.clone();
            let w2 = self.me.clone();
            let old = b.name.clone();
            let menu: MenuItems = vec![
                ("Rename…".into(), Rc::new(move || Self::rename_bookmark(&w2, id, &old))),
                (
                    "Remove Bookmark".into(),
                    Rc::new(move || {
                        if let Some(s) = weak.upgrade() {
                            let _ = s.state.db.remove_bookmark(id);
                            s.rebuild();
                        }
                    }),
                ),
            ];
            self.add("Bookmarks", "fm-bookmark-symbolic", &b.name, t, p, None, menu);
            self.make_reorderable(id);
        }

        // Devices.
        let (t, p) = path(PathBuf::from("/"));
        self.add("Devices", "fm-drive-symbolic", "Computer", t, p, None, vec![]);
        let vm = gio::VolumeMonitor::get();
        let mut net_mounts = Vec::new();
        for m in vm.mounts().into_iter().filter(|m| !m.is_shadowed()) {
            let root = m.root();
            match root.path().filter(|_| root.has_uri_scheme("file")) {
                Some(p) if p != Path::new("/") => {
                    let suffix = self.eject_widget(&m);
                    self.add(
                        "Devices",
                        "fm-removable-symbolic",
                        &m.name(),
                        Target::Path(p.clone()),
                        Some(p),
                        suffix,
                        vec![],
                    );
                }
                Some(_) => {}
                None => net_mounts.push(m),
            }
        }
        for v in vm.volumes() {
            if v.get_mount().is_none() && v.can_mount() {
                self.add("Devices", "fm-removable-symbolic", &v.name(), Target::Volume(v.clone()), None, None, vec![]);
            }
        }

        // Network: saved connections, live mounts that aren't saved, connect row.
        let saved = self.state.db.connections().unwrap_or_default();
        let mut shown_hosts = Vec::new();
        for c in &saved {
            let sh = network::scheme_host(&c.uri);
            let live = sh
                .as_ref()
                .and_then(|sh| net_mounts.iter().find(|m| network::scheme_host(&m.root().uri()).as_ref() == Some(sh)));
            let widget = live.and_then(|m| self.eject_widget(m));
            let (id, weak) = (c.id, self.me.clone());
            let menu: MenuItems = vec![(
                "Remove Saved Connection".into(),
                Rc::new(move || {
                    if let Some(s) = weak.upgrade() {
                        let _ = s.state.db.remove_connection(id);
                        s.rebuild();
                    }
                }),
            )];
            let live_path = live.and_then(|m| m.root().path());
            self.add(
                "Network",
                "fm-server-symbolic",
                &c.name,
                Target::Connection(c.uri.clone()),
                live_path,
                widget,
                menu,
            );
            shown_hosts.extend(sh);
        }
        for m in &net_mounts {
            if network::scheme_host(&m.root().uri()).is_some_and(|sh| shown_hosts.contains(&sh)) {
                continue;
            }
            let p = m.root().path();
            let t = p.clone().map(Target::Path).unwrap_or(Target::ConnectNew);
            self.add("Network", "fm-network-symbolic", &m.name(), t, p, self.eject_widget(m), vec![]);
        }
        self.add("Network", "fm-plus-symbolic", "Connect to Server…", Target::ConnectNew, None, None, vec![]);

        // Google Drive accounts.
        for r in self.cloud.borrow().iter() {
            let (name, mp) = (r.name.clone(), cloud::mount_point(&r.name));
            let mounted = cloud::is_mounted(&name);
            let suffix = mounted.then(|| {
                let b = gtk::Button::from_icon_name("fm-eject-symbolic");
                b.add_css_class("flat");
                b.set_valign(gtk::Align::Center);
                b.set_tooltip_text(Some("Disconnect"));
                let (n, w) = (name.clone(), self.me.clone());
                b.connect_clicked(move |_| {
                    let (n, w) = (n.clone(), w.clone());
                    glib::spawn_future_local(async move {
                        let n2 = n.clone();
                        let r = gio::spawn_blocking(move || cloud::unmount(&n2)).await;
                        if let Some(s) = w.upgrade() {
                            if let Ok(Err(e)) = r {
                                s.show_error("Could not disconnect", &e);
                            }
                            s.rebuild();
                        }
                    });
                });
                b.upcast::<gtk::Widget>()
            });
            let (w, n2) = (self.me.clone(), name.clone());
            let menu: MenuItems = vec![("Remove Account…".into(), Rc::new(move || Self::remove_cloud(&w, n2.clone())))];
            self.add(
                "Google Drive",
                "fm-cloud-symbolic",
                &name,
                Target::Cloud(name.clone()),
                mounted.then_some(mp),
                suffix,
                menu,
            );
        }
        self.add("Google Drive", "fm-plus-symbolic", "Add Google Drive…", Target::AddCloud, None, None, vec![]);

        // Tags.
        for (tag, count) in self.state.db.all_tags().unwrap_or_default() {
            let row_label = format!("{tag} ({count})");
            self.add("Tags", "fm-tag-symbolic", &row_label, Target::Tag(tag.clone()), None, None, vec![]);
            if let Some(row) = self.list.last_child().and_downcast::<gtk::ListBoxRow>()
                && let Some(img) =
                    row.child().and_downcast::<gtk::Box>().and_then(|b| b.first_child()).and_downcast::<gtk::Image>()
            {
                img.add_css_class(&crate::ui::tags::class_for(&tag));
            }
        }

        let cur = self.current.borrow().clone();
        self.select_path(&cur);
    }

    /// An eject / disconnect button for mounts that support it.
    fn eject_widget(&self, m: &gio::Mount) -> Option<gtk::Widget> {
        if !(m.can_unmount() || m.can_eject()) {
            return None;
        }
        let b = gtk::Button::from_icon_name("fm-eject-symbolic");
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some(if m.can_eject() { "Eject" } else { "Unmount" }));
        let (m, w) = (m.clone(), self.me.clone());
        b.connect_clicked(move |_| {
            let (m, w) = (m.clone(), w.clone());
            glib::spawn_future_local(async move {
                let Some(s) = w.upgrade() else { return };
                let Some(win) = s.window() else { return };
                if let Err(e) = mounts::unmount(&win, &m).await {
                    s.show_error("Could not unmount", &e);
                }
            });
        });
        Some(b.upcast())
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &self,
        section: &str,
        icon: &str,
        name: &str,
        target: Target,
        path: Option<PathBuf>,
        suffix: Option<gtk::Widget>,
        menu: MenuItems,
    ) {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        b.set_margin_top(4);
        b.set_margin_bottom(4);
        b.append(&gtk::Image::from_icon_name(icon));
        let l = gtk::Label::new(Some(name));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        b.append(&l);
        if let Some(s) = suffix {
            b.append(&s);
        }
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&b));
        row.set_widget_name(section);
        if let Some(p) = &path {
            row.set_tooltip_text(Some(&p.to_string_lossy()));
        }

        // Dropping files onto a folder entry copies/moves them there.
        if let Some(p) = path.clone() {
            let dt = interact::drop_target(self.on_drop.clone(), move || Some(p.clone()));
            let rw = row.downgrade();
            dt.connect_current_drop_notify(move |t| {
                if let Some(r) = rw.upgrade() {
                    if t.current_drop().is_some() {
                        r.add_css_class("drop-hover")
                    } else {
                        r.remove_css_class("drop-hover")
                    }
                }
            });
            row.add_controller(dt);
        }
        if !menu.is_empty() {
            Self::attach_menu(&row, menu);
        }
        self.list.append(&row);
        self.entries.borrow_mut().push(Entry { target, path });
    }

    fn attach_menu(row: &gtk::ListBoxRow, items: MenuItems) {
        let pop = gtk::Popover::new();
        pop.set_has_arrow(false);
        pop.set_parent(row);
        row.connect_destroy({
            let pop = pop.clone();
            move |_| pop.unparent()
        });
        let v = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for (label, f) in items {
            let b = gtk::Button::with_label(&label);
            b.add_css_class("flat");
            let p = pop.clone();
            b.connect_clicked(move |_| {
                p.popdown();
                f();
            });
            v.append(&b);
        }
        pop.set_child(Some(&v));
        let click = gtk::GestureClick::new();
        click.set_button(3);
        click.connect_pressed(move |_, _, x, y| {
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop.popup();
        });
        row.add_controller(click);
    }

    fn rename_bookmark(w: &Weak<Sidebar>, id: i64, old: &str) {
        let (w, old) = (w.clone(), old.to_string());
        glib::spawn_future_local(async move {
            let Some(s) = w.upgrade() else { return };
            let Some(win) = s.window() else { return };
            if let Some(n) = dialogs::prompt_name(&win, "Rename Bookmark", "Rename", &old).await {
                let _ = s.state.db.rename_bookmark(id, &n);
                s.rebuild();
            }
        });
    }

    /// Drag to reorder: a private string payload, accepted by bookmark rows.
    fn make_reorderable(&self, id: i64) {
        let Some(row) = self.list.last_child().and_downcast::<gtk::ListBoxRow>() else { return };
        let ds = gtk::DragSource::new();
        ds.set_actions(gdk::DragAction::MOVE);
        ds.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(&format!("fm-bookmark:{id}").to_value()))
        });
        row.add_controller(ds);
        let dt = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
        let w = self.me.clone();
        dt.connect_drop(move |_, v, _, _| {
            let Some(dragged) =
                v.get::<String>().ok().and_then(|s| s.strip_prefix("fm-bookmark:")?.parse::<i64>().ok())
            else {
                return false;
            };
            if let Some(s) = w.upgrade() {
                let _ = s.state.db.move_bookmark_before(dragged, id);
                // Rebuild after the drop finishes, not inside the drag source's signal.
                s.schedule_rebuild();
            }
            true
        });
        row.add_controller(dt);
    }

    /// Bookmark `path` (name = folder name). Returns false if already present.
    pub fn add_bookmark(&self, path: &Path) -> bool {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        let added = self.state.db.add_bookmark(&name, path).unwrap_or(false);
        if added {
            self.rebuild();
        }
        added
    }
}
