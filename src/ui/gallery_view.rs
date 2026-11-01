//! Gallery: large preview, previous/next buttons and a thumbnail strip.
//! It shares the pane's selection model, so the strip, the keyboard and the
//! other views always agree on what is selected.

use adw::prelude::*;
use gtk::gio;
use std::rc::Rc;

use crate::services::thumbnail::Thumbnailer;
use crate::ui::{
    file_grid,
    interact::ItemCtx,
    preview::{Preview, Target},
};

pub struct GalleryView {
    pub widget: gtk::Box,
    preview: Rc<Preview>,
    selection: gtk::MultiSelection,
    ctx: ItemCtx,
    strip: gtk::ListView,
}

impl GalleryView {
    pub fn new(selection: &gtk::MultiSelection, ctx: &ItemCtx, thumbs: Thumbnailer) -> Rc<Self> {
        let preview = Preview::new(true);
        let prev = gtk::Button::from_icon_name("fm-back-symbolic");
        let next = gtk::Button::from_icon_name("fm-forward-symbolic");
        for b in [&prev, &next] {
            b.add_css_class("circular");
            b.add_css_class("fm-gallery-nav");
            b.set_valign(gtk::Align::Center);
        }
        let main = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        main.append(&prev);
        preview.widget.set_hexpand(true);
        main.append(&preview.widget);
        main.append(&next);
        main.set_vexpand(true);

        let strip = gtk::ListView::new(Some(selection.clone()), Some(file_grid::factory(64, ctx.clone(), thumbs)));
        strip.set_orientation(gtk::Orientation::Horizontal);
        strip.add_css_class("fm-strip");
        let strip_sw = gtk::ScrolledWindow::new();
        strip_sw.set_child(Some(&strip));
        strip_sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        strip_sw.set_min_content_height(112);
        strip_sw.add_css_class("fm-gallery-strip");

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&main);
        widget.append(&strip_sw);

        let this = Rc::new(Self { widget, preview, selection: selection.clone(), ctx: ctx.clone(), strip });
        let w = Rc::downgrade(&this);
        selection.connect_selection_changed(move |_, _, _| {
            if let Some(g) = w.upgrade()
                && g.widget.is_mapped()
            {
                g.update();
            }
        });
        let w = Rc::downgrade(&this);
        this.widget.connect_map(move |_| {
            if let Some(g) = w.upgrade() {
                g.update();
            }
        });
        let (w, w2) = (Rc::downgrade(&this), Rc::downgrade(&this));
        prev.connect_clicked(move |_| {
            if let Some(g) = w.upgrade() {
                g.step(-1)
            }
        });
        next.connect_clicked(move |_| {
            if let Some(g) = w2.upgrade() {
                g.step(1)
            }
        });
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(g) = w.upgrade() else { return gtk::glib::Propagation::Proceed };
            match key {
                gtk::gdk::Key::Left => g.step(-1),
                gtk::gdk::Key::Right => g.step(1),
                _ => return gtk::glib::Propagation::Proceed,
            }
            gtk::glib::Propagation::Stop
        });
        this.widget.add_controller(keys);
        this
    }

    fn current(&self) -> Option<u32> {
        let bits = self.selection.selection();
        (!bits.is_empty()).then(|| bits.minimum())
    }

    fn step(&self, delta: i32) {
        let n = self.selection.n_items() as i32;
        if n == 0 {
            return;
        }
        let pos = self.current().map(|p| p as i32).unwrap_or(-delta.clamp(-1, 0));
        let to = (pos + delta).clamp(0, n - 1);
        self.selection.select_item(to as u32, true);
    }

    pub fn update(&self) {
        let target: Option<Target> = self.current().and_then(|p| {
            let info = self.selection.item(p).and_downcast::<gio::FileInfo>()?;
            Some((self.ctx.dir.borrow().join(info.name()), info))
        });
        if let Some(p) = self.current() {
            self.strip.scroll_to(p, gtk::ListScrollFlags::NONE, None);
        }
        self.preview.show(target);
    }
}
