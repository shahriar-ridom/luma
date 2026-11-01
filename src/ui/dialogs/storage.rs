//! Storage overview: drives with capacity, and a folder usage breakdown.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::core::filesystem::human_size;
use crate::core::metadata;
use crate::services::{mounts, storage};
use crate::ui::dialogs;

struct Usage {
    name: String,
    is_dir: bool,
    bytes: u64,
}

struct UsageView {
    widget: gtk::Box,
    store: gio::ListStore,
    current: RefCell<PathBuf>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    max: Rc<Cell<u64>>,
    title: gtk::Label,
    status: gtk::Label,
    spinner: gtk::Spinner,
    up: gtk::Button,
    me: Weak<UsageView>,
}

impl UsageView {
    fn new(start: PathBuf) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let max = Rc::new(Cell::new(1u64));
        let sorter = gtk::CustomSorter::new(|a, b| {
            let (a, b) =
                (a.downcast_ref::<glib::BoxedAnyObject>().unwrap(), b.downcast_ref::<glib::BoxedAnyObject>().unwrap());
            b.borrow::<Usage>().bytes.cmp(&a.borrow::<Usage>().bytes).into()
        });
        let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter));

        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(|_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            row.append(&gtk::Image::new());
            let n = gtk::Label::new(None);
            n.set_xalign(0.0);
            n.set_hexpand(true);
            n.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            row.append(&n);
            let bar = gtk::ProgressBar::new();
            bar.set_valign(gtk::Align::Center);
            bar.set_width_request(160);
            row.append(&bar);
            let s = gtk::Label::new(None);
            s.set_width_chars(10);
            s.set_xalign(1.0);
            s.add_css_class("dim-label");
            row.append(&s);
            item.set_child(Some(&row));
        });
        let m = max.clone();
        f.connect_bind(move |_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let obj = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
            let u = obj.borrow::<Usage>();
            let row = item.child().and_downcast::<gtk::Box>().unwrap();
            let img = row.first_child().and_downcast::<gtk::Image>().unwrap();
            let name = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let bar = name.next_sibling().and_downcast::<gtk::ProgressBar>().unwrap();
            let size = bar.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let ct = if u.is_dir {
                "inode/directory".to_string()
            } else {
                gio::content_type_guess(Some(&u.name), None).0.to_string()
            };
            img.set_from_gicon(&crate::core::filesystem::icon_for(&ct, u.is_dir));
            name.set_text(&u.name);
            bar.set_fraction(u.bytes as f64 / m.get().max(1) as f64);
            size.set_text(&human_size(u.bytes as i64));
        });
        let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(sorted))), Some(f));
        list.set_single_click_activate(true);
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&list));
        sw.set_vexpand(true);

        let up = gtk::Button::from_icon_name("fm-up-symbolic");
        up.set_tooltip_text(Some("Parent folder"));
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        title.add_css_class("heading");
        let spinner = gtk::Spinner::new();
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.set_margin_top(8);
        top.set_margin_bottom(8);
        top.set_margin_start(12);
        top.set_margin_end(12);
        top.append(&up);
        top.append(&title);
        top.append(&spinner);
        let status = gtk::Label::new(None);
        status.add_css_class("dim-label");
        status.set_xalign(0.0);
        status.set_margin_start(12);
        status.set_margin_top(6);
        status.set_margin_bottom(6);
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&top);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&sw);
        widget.append(&status);

        let this = Rc::new_cyclic(|me| Self {
            widget,
            store,
            current: RefCell::new(start),
            cancel: RefCell::default(),
            max,
            title,
            status,
            spinner,
            up,
            me: me.clone(),
        });
        let w = this.me.clone();
        this.up.connect_clicked(move |_| {
            if let Some(v) = w.upgrade() {
                let p = v.current.borrow().parent().map(Path::to_path_buf);
                if let Some(p) = p {
                    v.analyze(p);
                }
            }
        });
        let w = this.me.clone();
        list.connect_activate(move |lv, pos| {
            let (Some(v), Some(o)) =
                (w.upgrade(), lv.model().and_then(|m| m.item(pos)).and_downcast::<glib::BoxedAnyObject>())
            else {
                return;
            };
            let (name, is_dir) = {
                let u = o.borrow::<Usage>();
                (u.name.clone(), u.is_dir)
            };
            if is_dir {
                let p = v.current.borrow().join(name);
                v.analyze(p);
            }
        });
        this
    }

    fn stop(&self) {
        if let Some(c) = self.cancel.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
    }

    fn analyze(&self, path: PathBuf) {
        self.stop();
        self.store.remove_all();
        self.max.set(1);
        self.title.set_text(&path.to_string_lossy());
        self.up.set_sensitive(path.parent().is_some());
        *self.current.borrow_mut() = path.clone();
        self.spinner.start();
        self.status.set_text("Measuring…");

        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = Some(cancel.clone());
        let (tx, rx) = async_channel::unbounded();
        let p = path.clone();
        std::thread::spawn(move || {
            metadata::usage_children(&p, &cancel, |n, d, b| {
                let _ = tx.send_blocking((n, d, b));
            })
        });
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let mut total = 0u64;
            while let Ok((name, is_dir, bytes)) = rx.recv().await {
                let Some(v) = w.upgrade() else { return };
                if *v.current.borrow() != path {
                    return; // superseded by another folder
                }
                total += bytes;
                v.max.set(v.max.get().max(bytes));
                v.store.append(&glib::BoxedAnyObject::new(Usage { name, is_dir, bytes }));
                v.status.set_text(&format!("{} items · {} so far", v.store.n_items(), human_size(total as i64)));
            }
            if let Some(v) = w.upgrade()
                && *v.current.borrow() == path
            {
                v.spinner.stop();
                v.status.set_text(&format!("{} items · {} total", v.store.n_items(), human_size(total as i64)));
                // Rebind so bars use the final maximum.
                let n = v.store.n_items();
                v.store.items_changed(0, n, n);
            }
        });
    }
}

fn drives_page(parent: &gtk::Window, on_open: Rc<dyn Fn(PathBuf)>, dialog: &adw::Dialog) -> gtk::Widget {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_vexpand(true);

    let loading = adw::ActionRow::new();
    loading.set_title("Reading drives…");
    list.append(&loading);
    let (parent, dialog) = (parent.clone(), dialog.clone());
    glib::spawn_future_local(async move {
        let details = storage::mount_details().await;
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        if details.is_empty() {
            let r = adw::ActionRow::new();
            r.set_title("No drives found");
            list.append(&r);
        }
        for d in details {
            let row = adw::ActionRow::new();
            row.set_title(&d.name);
            row.set_use_markup(false);
            row.set_subtitle_lines(0);
            let mut lines = Vec::new();
            if let Some(i) = &d.info {
                lines.push(format!(
                    "{} used of {} · {} free",
                    human_size(i.used as i64),
                    human_size(i.size as i64),
                    human_size(i.free as i64)
                ));
            }
            let mut meta = Vec::new();
            if !d.fs_type.is_empty() {
                meta.push(d.fs_type.clone());
            }
            if let Some(dev) = &d.device {
                meta.push(dev.clone());
            }
            if let Some(r) = &d.root {
                meta.push(format!("mounted at {}", r.display()));
            }
            if d.info.as_ref().is_some_and(|i| i.readonly) {
                meta.push("read-only".into());
            }
            if d.removable {
                meta.push("removable".into());
            }
            if !meta.is_empty() {
                lines.push(meta.join(" · "));
            }
            if let Some(u) = &d.uuid {
                lines.push(format!("UUID {u}"));
            }
            row.set_subtitle(&lines.join("\n"));
            row.add_prefix(&gtk::Image::from_gicon(&d.mount.symbolic_icon()));

            if let Some(i) = &d.info {
                let bar = gtk::LevelBar::for_interval(0.0, 1.0);
                bar.set_value(i.used_fraction());
                bar.set_width_request(130);
                bar.set_valign(gtk::Align::Center);
                bar.add_css_class("fm-usage");
                bar.add_css_class(if i.used_fraction() > 0.95 {
                    "crit"
                } else if i.used_fraction() > 0.85 {
                    "warn"
                } else {
                    "ok"
                });
                row.add_suffix(&bar);
            }
            if let Some(root) = d.root.clone() {
                let open = gtk::Button::with_label("Open");
                open.set_valign(gtk::Align::Center);
                let (cb, dd) = (on_open.clone(), dialog.clone());
                open.connect_clicked(move |_| {
                    dd.close();
                    cb(root.clone());
                });
                row.add_suffix(&open);
            }
            if d.mount.can_unmount() || d.mount.can_eject() {
                let eject = gtk::Button::from_icon_name("fm-eject-symbolic");
                eject.add_css_class("flat");
                eject.set_valign(gtk::Align::Center);
                eject.set_tooltip_text(Some(if d.mount.can_eject() { "Eject" } else { "Unmount" }));
                let (m, p, row2) = (d.mount.clone(), parent.clone(), row.clone());
                eject.connect_clicked(move |_| {
                    let (m, p, row2) = (m.clone(), p.clone(), row2.clone());
                    glib::spawn_future_local(async move {
                        match mounts::unmount(&p, &m).await {
                            Ok(()) => row2.set_sensitive(false),
                            Err(e) => dialogs::error(&p, "Could not unmount", &e),
                        }
                    });
                });
                row.add_suffix(&eject);
            }
            list.append(&row);
        }
    });
    sw.upcast()
}

/// `folder` seeds the usage analysis; `on_open` navigates the active pane.
pub fn show(parent: &gtk::Window, folder: PathBuf, on_open: Rc<dyn Fn(PathBuf)>) {
    let stack = adw::ViewStack::new();
    let tv = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let switcher = adw::ViewSwitcher::new();
    switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));
    tv.add_top_bar(&header);
    tv.set_content(Some(&stack));
    let dialog = adw::Dialog::builder().title("Storage").content_width(720).content_height(580).child(&tv).build();

    let drives = drives_page(parent, on_open, &dialog);
    stack.add_titled_with_icon(&drives, Some("drives"), "Drives", "fm-drive-symbolic");
    let usage = UsageView::new(folder.clone());
    stack.add_titled_with_icon(&usage.widget, Some("usage"), "Folder Usage", "fm-folder-symbolic");
    usage.analyze(folder);

    let u = usage.clone();
    dialog.connect_closed(move |_| u.stop());
    dialog.present(Some(parent));
}
