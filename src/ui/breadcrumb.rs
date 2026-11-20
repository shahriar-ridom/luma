use adw::prelude::*;
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
};

/// Clickable path components. Rebuilt on every navigation.
pub struct Breadcrumb {
    pub widget: gtk::ScrolledWindow,
    row: gtk::Box,
    on_nav: Rc<dyn Fn(PathBuf)>,
}

impl Breadcrumb {
    pub fn new(on_nav: impl Fn(PathBuf) + 'static) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("fm-crumbs");
        let widget = gtk::ScrolledWindow::new();
        widget.set_child(Some(&row));
        widget.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        widget.set_hexpand(true);
        widget.add_css_class("fm-crumb-scroll");
        // A focused crumb (the first one, usually) would otherwise scroll itself
        // into view and undo the pinning below.
        if let Some(viewport) = widget.child().and_downcast::<gtk::Viewport>() {
            viewport.set_scroll_to_focus(false);
        }
        // There is no scrollbar, so keep the current folder (the last crumb) in
        // view whenever the content or the width changes: a new path, a split,
        // a resize. Doing it once after `set_path` missed panes that had no
        // final width yet.
        let adj = widget.hadjustment();
        // Moved after the layout pass that reported the change: moving it
        // during that pass is not drawn until something else relayouts.
        let pending = Rc::new(Cell::new(false));
        let pin = move |a: &gtk::Adjustment| {
            if pending.replace(true) {
                return;
            }
            let (a, pending) = (a.clone(), pending.clone());
            gtk::glib::idle_add_local_once(move || {
                pending.set(false);
                a.set_value(a.upper() - a.page_size());
            });
        };
        adj.connect_upper_notify(pin.clone());
        adj.connect_page_size_notify(pin);
        Self { widget, row, on_nav: Rc::new(on_nav) }
    }

    pub fn set_path(&self, path: &Path) {
        while let Some(c) = self.row.first_child() {
            self.row.remove(&c);
        }
        let mut acc = PathBuf::from("/");
        let mut parts: Vec<(String, PathBuf)> = vec![("/".into(), acc.clone())];
        if crate::core::operations::trash::is_trash(path) {
            // Not a real folder: one crumb.
            parts = vec![("Trash".into(), path.to_path_buf())];
        } else {
            for c in path.components().skip(1) {
                acc.push(c);
                parts.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
            }
        }
        let last = parts.len() - 1;
        for (i, (name, p)) in parts.into_iter().enumerate() {
            if i > 0 {
                let sep = gtk::Image::from_icon_name(crate::application::icons::themed("luma-forward-symbolic"));
                sep.set_pixel_size(10);
                sep.add_css_class("fm-crumb-sep");
                self.row.append(&sep);
            }
            let b = gtk::Button::with_label(&name);
            b.add_css_class("flat");
            if i == last {
                b.add_css_class("fm-crumb-current");
            }
            let cb = self.on_nav.clone();
            b.connect_clicked(move |_| cb(p.clone()));
            self.row.append(&b);
        }
    }
}
