//! Container holding one or two panes side by side.

use gtk::prelude::*;

pub struct SplitView {
    pub widget: gtk::Paned,
}

impl SplitView {
    pub fn new() -> Self {
        let p = gtk::Paned::new(gtk::Orientation::Horizontal);
        p.set_hexpand(true);
        p.set_vexpand(true);
        p.set_wide_handle(false);
        p.set_resize_start_child(true);
        p.set_resize_end_child(true);
        p.set_shrink_start_child(false);
        p.set_shrink_end_child(false);
        Self { widget: p }
    }

    /// Show one or two panes, splitting the space evenly.
    pub fn set_panes(&self, panes: &[gtk::Widget]) {
        self.widget.set_start_child(None::<&gtk::Widget>);
        self.widget.set_end_child(None::<&gtk::Widget>);
        self.widget.set_start_child(panes.first());
        self.widget.set_end_child(panes.get(1));
        if panes.len() > 1 {
            let paned = self.widget.clone();
            paned.add_tick_callback(|p, _| {
                if p.width() > 0 {
                    p.set_position(p.width() / 2);
                    gtk::glib::ControlFlow::Break
                } else {
                    gtk::glib::ControlFlow::Continue
                }
            });
        }
    }
}
