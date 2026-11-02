//! "Open With" chooser: pick an application for a file, optionally making it
//! the default for that file type so later opens don't ask again.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::Cell, rc::Rc};

pub struct Choice {
    pub app: gio::AppInfo,
    /// The user asked to always use this app for the file type.
    pub make_default: bool,
}

/// Apps that can open `content_type`: the current default first, then the
/// recommended ones, then everything else that declares support.
fn candidates(content_type: &str) -> Vec<gio::AppInfo> {
    let mut out: Vec<gio::AppInfo> = Vec::new();
    let mut push = |a: gio::AppInfo| {
        if a.should_show() && !out.iter().any(|b| b.equal(&a)) {
            out.push(a);
        }
    };
    if let Some(d) = gio::AppInfo::default_for_type(content_type, false) {
        push(d);
    }
    gio::AppInfo::recommended_for_type(content_type).into_iter().for_each(&mut push);
    gio::AppInfo::all_for_type(content_type).into_iter().for_each(&mut push);
    out
}

fn app_row(app: &gio::AppInfo, is_default: bool) -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    b.set_margin_top(6);
    b.set_margin_bottom(6);
    b.set_margin_start(8);
    b.set_margin_end(8);
    let img = match app.icon() {
        Some(i) => gtk::Image::from_gicon(&i),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    img.set_pixel_size(32);
    b.append(&img);
    let l = gtk::Label::new(Some(&app.display_name()));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    b.append(&l);
    if is_default {
        let d = gtk::Label::new(Some("Default"));
        d.add_css_class("dim-label");
        d.add_css_class("caption");
        b.append(&d);
    }
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&b));
    row
}

/// Ask which app should open `name` (of type `content_type`). `default_on`
/// pre-checks "Always use…" (used when the type has no default yet).
pub async fn choose(parent: &gtk::Window, name: &str, content_type: &str, default_on: bool) -> Option<Choice> {
    let apps = candidates(content_type);
    let desc = gio::content_type_get_description(content_type);
    let current = gio::AppInfo::default_for_type(content_type, false);
    let body = if apps.is_empty() {
        format!("No installed application can open “{name}” ({desc}).")
    } else {
        format!("Choose an application to open “{name}”.")
    };
    let d = adw::AlertDialog::new(Some("Open With"), Some(&body));
    d.add_responses(&[("cancel", "Cancel"), ("open", "Open")]);
    d.set_response_appearance("open", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("open"));
    d.set_close_response("cancel");
    if apps.is_empty() {
        d.set_response_enabled("open", false);
        d.choose_future(Some(parent)).await;
        return None;
    }

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    for a in &apps {
        list.append(&app_row(a, current.as_ref().is_some_and(|c| c.equal(a))));
    }
    list.select_row(list.row_at_index(0).as_ref());
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_propagate_natural_height(true);
    sw.set_max_content_height(320);
    sw.set_min_content_width(320);
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    let always = gtk::CheckButton::with_label(&format!("Always use for {desc}"));
    always.set_active(default_on);
    let v = gtk::Box::new(gtk::Orientation::Vertical, 10);
    v.append(&sw);
    v.append(&always);
    d.set_extra_child(Some(&v));

    // Double-click / Enter on a row opens it straight away.
    let activated = Rc::new(Cell::new(false));
    let (a, dd) = (activated.clone(), d.clone());
    list.connect_row_activated(move |_, _| {
        a.set(true);
        dd.close();
    });
    let l = list.clone();
    glib::idle_add_local_once(move || {
        if let Some(r) = l.selected_row() {
            r.grab_focus();
        }
    });

    let r = d.choose_future(Some(parent)).await;
    if r != "open" && !activated.get() {
        return None;
    }
    let idx = list.selected_row()?.index();
    let app = apps.get(usize::try_from(idx).ok()?)?.clone();
    Some(Choice { app, make_default: always.is_active() })
}
