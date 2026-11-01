use adw::prelude::*;
use std::{
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
        Self { widget, row, on_nav: Rc::new(on_nav) }
    }

    pub fn set_path(&self, path: &Path) {
        while let Some(c) = self.row.first_child() {
            self.row.remove(&c);
        }
        let mut acc = PathBuf::from("/");
        let mut parts: Vec<(String, PathBuf)> = vec![("/".into(), acc.clone())];
        for c in path.components().skip(1) {
            acc.push(c);
            parts.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
        }
        let last = parts.len() - 1;
        for (i, (name, p)) in parts.into_iter().enumerate() {
            if i > 0 {
                let sep = gtk::Image::from_icon_name("luma-forward-symbolic");
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
        // Keep the tail (current folder) visible.
        let adj = self.widget.hadjustment();
        gtk::glib::idle_add_local_once(move || adj.set_value(adj.upper()));
    }
}
