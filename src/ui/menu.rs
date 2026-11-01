//! Context menus drawn from a `gio::Menu` model.
//!
//! GTK's own `PopoverMenu` caps its height and scrolls (about a third of the
//! window here), which hid the lower entries. A plain popover has no such
//! limit, so menus are rendered as ordinary widgets: sections separated by
//! hairlines, submenus that slide in with a back button, and the user's own
//! shortcut next to each entry.

use gtk::{gio, glib, prelude::*};
use std::rc::Rc;

type AccelOf = Rc<dyn Fn(&str) -> Option<String>>;

fn label_of(model: &gio::MenuModel, i: i32) -> String {
    model.item_attribute_value(i, "label", None).and_then(|v| v.str().map(str::to_string)).unwrap_or_default()
}

fn accel_text(action: &str, accel_of: &AccelOf) -> Option<String> {
    let accel = accel_of(action.strip_prefix("win.").unwrap_or(action))?;
    let (key, mods) = gtk::accelerator_parse(&accel)?;
    Some(gtk::accelerator_get_label(key, mods).to_string())
}

/// Fill `page` with the entries of `model`; submenus become new stack pages.
fn fill(page: &gtk::Box, model: &gio::MenuModel, pop: &gtk::Popover, accel_of: &AccelOf) {
    let mut first_section = true;
    for i in 0..model.n_items() {
        if let Some(section) = model.item_link(i, "section") {
            if !first_section {
                let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
                sep.add_css_class("fm-menu-sep");
                page.append(&sep);
            }
            first_section = false;
            let title = label_of(model, i);
            if !title.is_empty() {
                let l = gtk::Label::new(Some(&title));
                l.add_css_class("fm-section");
                l.set_xalign(0.0);
                l.set_margin_start(10);
                l.set_margin_top(4);
                page.append(&l);
            }
            fill(page, &section, pop, accel_of);
            continue;
        }
        let title = label_of(model, i);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let name = gtk::Label::new(Some(&title));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        row.append(&name);
        let btn = gtk::Button::new();
        btn.add_css_class("flat");
        btn.add_css_class("fm-menu-item");

        if let Some(sub) = model.item_link(i, "submenu") {
            let chev = gtk::Image::from_icon_name("fm-forward-symbolic");
            chev.set_pixel_size(11);
            chev.add_css_class("fm-chev");
            row.append(&chev);
            // A second popover beside the entry. (Swapping pages inside one
            // popover made it close as soon as the entry was pressed.)
            let sub_page = gtk::Box::new(gtk::Orientation::Vertical, 1);
            sub_page.set_width_request(230);
            fill(&sub_page, &sub, pop, accel_of);
            let sub_pop = gtk::Popover::new();
            sub_pop.set_has_arrow(false);
            sub_pop.set_position(gtk::PositionType::Right);
            sub_pop.add_css_class("fm-menu");
            sub_pop.set_child(Some(&sub_page));
            sub_pop.set_parent(&btn);
            let sp = sub_pop.clone();
            btn.connect_destroy(move |_| sp.unparent());
            btn.connect_clicked(move |_| sub_pop.popup());
        } else if let Some(action) =
            model.item_attribute_value(i, "action", None).and_then(|v| v.str().map(str::to_string))
        {
            btn.set_action_name(Some(&action));
            if let Some(t) = model.item_attribute_value(i, "target", None) {
                btn.set_action_target_value(Some(&t));
            }
            if let Some(a) = accel_text(&action, accel_of) {
                let l = gtk::Label::new(Some(&a));
                l.add_css_class("dim-label");
                l.add_css_class("fm-menu-accel");
                row.append(&l);
            }
            let p = pop.clone();
            btn.connect_clicked(move |_| p.popdown());
        }
        btn.set_child(Some(&row));
        page.append(&btn);
    }
}

/// Show `model` as a popover on `parent`, anchored at `at`.
pub fn popup(parent: &gtk::Widget, model: &gio::Menu, at: gtk::gdk::Rectangle, accel_of: AccelOf) {
    let pop = gtk::Popover::new();
    pop.set_has_arrow(false);
    pop.set_halign(gtk::Align::Start);
    pop.add_css_class("fm-menu");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 1);
    root.set_width_request(240);
    fill(&root, model.upcast_ref(), &pop, &accel_of);
    pop.set_child(Some(&root));
    pop.set_parent(parent);
    pop.set_pointing_to(Some(&at));
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
    #[cfg(debug_assertions)]
    if let Ok(l) = std::env::var("FM_MENU_CLICK") {
        let p = pop.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || debug_click(p.upcast_ref(), &l));
    }
}

/// Development aid: `FM_MENU_CLICK=More` presses the entry with that label.
#[cfg(debug_assertions)]
pub fn debug_click(root: &gtk::Widget, label: &str) {
    fn walk(w: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(b) = w.downcast_ref::<gtk::Button>()
            && let Some(row) = b.child().and_downcast::<gtk::Box>()
            && row.first_child().and_downcast::<gtk::Label>().is_some_and(|l| l.text() == label)
        {
            return Some(b.clone());
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            if let Some(b) = walk(&ch, label) {
                return Some(b);
            }
            c = ch.next_sibling();
        }
        None
    }
    if let Some(b) = walk(root, label) {
        b.emit_clicked();
    }
}
