//! Finder-style Miller columns. Each column lists one directory; selecting a
//! folder opens the next column, selecting a file shows a preview card.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};

use crate::core::filesystem::{self, LoadToken, human_size, is_dir};

const COL_WIDTH: i32 = 240;

pub struct MillerColumns {
    pub widget: gtk::ScrolledWindow,
    row: gtk::Box,
    show_hidden: Cell<bool>,
    root: RefCell<PathBuf>,
    on_open_file: Rc<dyn Fn(PathBuf)>,
}

impl MillerColumns {
    pub fn new(on_open_file: impl Fn(PathBuf) + 'static) -> Rc<Self> {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let widget = gtk::ScrolledWindow::new();
        widget.set_child(Some(&row));
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        Rc::new(Self {
            widget,
            row,
            show_hidden: Cell::new(false),
            root: RefCell::default(),
            on_open_file: Rc::new(on_open_file),
        })
    }

    pub fn set_show_hidden(self: &Rc<Self>, v: bool) {
        self.show_hidden.set(v);
        let r = self.root.borrow().clone();
        if !r.as_os_str().is_empty() {
            self.set_root(r);
        }
    }

    pub fn set_root(self: &Rc<Self>, path: PathBuf) {
        *self.root.borrow_mut() = path.clone();
        self.truncate(0);
        self.push_dir(path);
    }

    fn truncate(&self, keep: usize) {
        let mut idx = 0;
        let mut child = self.row.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if idx >= keep {
                self.row.remove(&c);
            }
            idx += 1;
        }
    }

    fn scroll_end(&self) {
        let adj = self.widget.hadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
    }

    fn push_dir(self: &Rc<Self>, dir: PathBuf) {
        let store = gio::ListStore::new::<gio::FileInfo>();
        let hidden = self.show_hidden.get();
        let filter = gtk::CustomFilter::new(move |o| {
            o.downcast_ref::<gio::FileInfo>().is_some_and(|i| hidden || !(i.is_hidden() || i.is_backup()))
        });
        let fm = gtk::FilterListModel::new(Some(store.clone()), Some(filter));
        let sorter = gtk::CustomSorter::new(|a, b| {
            let (a, b) = (a.downcast_ref::<gio::FileInfo>().unwrap(), b.downcast_ref::<gio::FileInfo>().unwrap());
            match (is_dir(a), is_dir(b)) {
                (true, false) => gtk::Ordering::Smaller,
                (false, true) => gtk::Ordering::Larger,
                _ => a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()).into(),
            }
        });
        let sorted = gtk::SortListModel::new(Some(fm), Some(sorter));
        let sel = gtk::SingleSelection::new(Some(sorted));
        sel.set_autoselect(false);
        sel.set_can_unselect(true);
        sel.set_selected(gtk::INVALID_LIST_POSITION);

        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(|_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            b.append(&gtk::Image::new());
            let l = gtk::Label::new(None);
            l.set_hexpand(true);
            l.set_xalign(0.0);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            b.append(&l);
            let chev = gtk::Image::new();
            chev.set_pixel_size(11);
            chev.add_css_class("fm-chev");
            b.append(&chev);
            item.set_child(Some(&b));
        });
        f.connect_bind(|_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let info = item.item().and_downcast::<gio::FileInfo>().unwrap();
            let b = item.child().and_downcast::<gtk::Box>().unwrap();
            let icon = b.first_child().and_downcast::<gtk::Image>().unwrap();
            let lbl = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let chev = lbl.next_sibling().and_downcast::<gtk::Image>().unwrap();
            if let Some(i) = crate::core::filesystem::file_icon(&info) {
                icon.set_from_gicon(&i);
            }
            lbl.set_text(&info.display_name());
            chev.set_icon_name(if is_dir(&info) { Some("fm-forward-symbolic") } else { None });
        });
        let lv = gtk::ListView::new(Some(sel.clone()), Some(f));
        lv.add_css_class("fm-miller");
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&lv));
        sw.set_size_request(COL_WIDTH, -1);
        sw.set_vexpand(true);
        sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let sep = gtk::Separator::new(gtk::Orientation::Vertical);
        let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        holder.append(&sw);
        holder.append(&sep);

        let position = self.row.observe_children().n_items() as usize;
        self.row.append(&holder);

        let this = self.clone();
        let dir2 = dir.clone();
        sel.connect_selection_changed(move |s, _, _| {
            this.truncate(position + 1);
            if let Some(info) = s.selected_item().and_downcast::<gio::FileInfo>() {
                let p = dir2.join(info.name());
                if is_dir(&info) {
                    this.push_dir(p);
                } else {
                    this.push_preview(p, &info);
                }
            }
        });
        let this = self.clone();
        let dir3 = dir.clone();
        lv.connect_activate(move |lv, pos| {
            if let Some(info) = lv.model().and_then(|m| m.item(pos)).and_downcast::<gio::FileInfo>()
                && !is_dir(&info)
            {
                (this.on_open_file)(dir3.join(info.name()));
            }
        });

        // Load asynchronously, incrementally.
        let token = LoadToken::default();
        let id = token.next();
        glib::spawn_future_local(async move {
            let _ = filesystem::load_dir(&gio::File::for_path(&dir), &token, id, |batch| {
                let items: Vec<glib::Object> = batch.into_iter().map(Into::into).collect();
                store.extend_from_slice(&items);
            })
            .await;
        });
        self.scroll_end();
    }

    fn push_preview(&self, path: PathBuf, info: &gio::FileInfo) {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.set_size_request(COL_WIDTH + 40, -1);
        card.set_margin_top(24);
        card.set_margin_start(16);
        card.set_margin_end(16);
        let img = gtk::Image::new();
        img.set_pixel_size(128);
        if let Some(i) = info.icon() {
            img.set_from_gicon(&i);
        }
        // Image content: decode a preview asynchronously.
        if info.content_type().is_some_and(|c| c.starts_with("image/")) && info.size() < 64 << 20 {
            let img = img.clone();
            let p = path.clone();
            glib::spawn_future_local(async move {
                let f = gio::File::for_path(&p);
                if let Ok(tex) = gtk::gdk::Texture::from_file(&f) {
                    img.set_paintable(Some(&tex));
                    img.set_pixel_size(200);
                }
            });
        }
        card.append(&img);
        let name = gtk::Label::new(Some(&info.display_name()));
        name.add_css_class("title-3");
        name.set_wrap(true);
        name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        name.set_justify(gtk::Justification::Center);
        card.append(&name);
        let kind = gio::content_type_get_description(&info.content_type().unwrap_or_default());
        let sub = gtk::Label::new(Some(&format!("{} — {}", kind, human_size(info.size()))));
        sub.add_css_class("dim-label");
        card.append(&sub);
        if let Some(d) =
            info.modification_date_time().and_then(|d| d.to_local().ok()).and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        {
            let m = gtk::Label::new(Some(&format!("Modified {d}")));
            m.add_css_class("dim-label");
            card.append(&m);
        }
        let _: &Path = &path;
        self.row.append(&card);
        self.scroll_end();
    }
}
