use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;

use crate::services::thumbnail::{self, Thumbnailer};
use crate::ui::interact::{self, ItemCtx};

pub const MIN_ICON: i32 = 48;
pub const MAX_ICON: i32 = 256;

pub struct FileGrid {
    pub widget: gtk::ScrolledWindow,
    view: gtk::GridView,
    icon_size: Cell<i32>,
    ctx: ItemCtx,
    thumbs: Thumbnailer,
}

pub fn factory(size: i32, ctx: ItemCtx, thumbs: Thumbnailer) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    let ctx2 = ctx.clone();
    f.connect_setup(move |_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
        b.set_margin_top(6);
        b.set_margin_bottom(6);
        b.set_margin_start(6);
        b.set_margin_end(6);
        b.set_width_request(size + 16);
        b.add_css_class("fm-grid-item");
        interact::setup_item(item, &b, &ctx);
        let img = gtk::Image::new();
        img.set_pixel_size(size);
        img.set_height_request(size);
        img.add_css_class("fm-thumb");
        img.set_overflow(gtk::Overflow::Hidden);
        b.append(&img);
        let l = gtk::Label::new(None);
        l.set_wrap(true);
        l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        l.set_lines(2);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars((size / 7).max(8));
        l.set_justify(gtk::Justification::Center);
        b.append(&l);
        let dots = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        dots.set_halign(gtk::Align::Center);
        dots.set_visible(false);
        b.append(&dots);
        item.set_child(Some(&b));
    });
    f.connect_bind(move |_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let info = item.item().and_downcast::<gio::FileInfo>().unwrap();
        let b = item.child().and_downcast::<gtk::Box>().unwrap();
        let img = b.first_child().and_downcast::<gtk::Image>().unwrap();
        let lbl = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
        lbl.set_text(&info.display_name());
        if let Some(dots) = lbl.next_sibling().and_downcast::<gtk::Box>() {
            let t = ctx2.tags.borrow().get(info.name().to_string_lossy().as_ref()).cloned().unwrap_or_default();
            let name = info.name().to_string_lossy().into_owned();
            let g = {
                let m = ctx2.git.borrow();
                m.get(&name).or_else(|| m.get("*")).copied()
            };
            crate::ui::tags::fill_badges(&dots, &t, g);
        }
        img.set_pixel_size(size);
        if let Some(icon) = crate::core::filesystem::file_icon(&info) {
            img.set_from_gicon(&icon);
        }
        let full = ctx2.dir.borrow().join(info.name());
        if let Some(src) = thumbnail::source_for(&info, &full) {
            let mtime = info.modification_date_time().map(|d| d.to_unix() as u64).unwrap_or(0);
            let (wi, wo) = (item.downgrade(), item.downgrade());
            let want = info.clone();
            let want2 = info.clone();
            let img_w = img.downgrade();
            thumbs.request(
                src,
                mtime,
                size,
                move || wi.upgrade().and_then(|i| i.item()).is_some_and(|c| c == want.clone().upcast::<glib::Object>()),
                move |tex| {
                    let still = wo
                        .upgrade()
                        .and_then(|i| i.item())
                        .is_some_and(|c| c == want2.clone().upcast::<glib::Object>());
                    if let (true, Some(img)) = (still, img_w.upgrade()) {
                        img.set_paintable(Some(&tex));
                    }
                },
            );
        }
    });
    f
}

impl FileGrid {
    pub fn new(
        selection: &gtk::MultiSelection,
        ctx: &ItemCtx,
        thumbs: Thumbnailer,
        on_open: impl Fn(&gio::FileInfo) + 'static,
    ) -> Self {
        let size = 96;
        let view = gtk::GridView::new(Some(selection.clone()), Some(factory(size, ctx.clone(), thumbs.clone())));
        view.set_enable_rubberband(true);
        view.add_css_class("fm-grid");
        view.set_max_columns(32);
        view.connect_activate(move |v, pos| {
            if let Some(info) = v.model().and_then(|m| m.item(pos)).and_downcast::<gio::FileInfo>() {
                on_open(&info);
            }
        });
        let widget = gtk::ScrolledWindow::new();
        widget.set_child(Some(&view));
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        Self { widget, view, icon_size: Cell::new(size), ctx: ctx.clone(), thumbs }
    }

    pub fn scroll_to(&self, pos: u32) {
        self.view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
    }

    pub fn icon_size(&self) -> i32 {
        self.icon_size.get()
    }

    pub fn set_icon_size(&self, size: i32) {
        let size = size.clamp(MIN_ICON, MAX_ICON);
        if size == self.icon_size.get() {
            return;
        }
        self.icon_size.set(size);
        self.view.set_factory(Some(&factory(size, self.ctx.clone(), self.thumbs.clone())));
    }
}
