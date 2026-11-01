//! Tag editor for the selection, and a browser for all files with a tag.

use adw::prelude::*;
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, rc::Rc};

use crate::application::state::AppState;
use crate::ui::tags;

struct TagRow {
    check: gtk::CheckButton,
    /// How many selected items had the tag when the dialog opened.
    initial: usize,
}

pub fn edit(parent: &gtk::Window, paths: Vec<PathBuf>, state: Rc<AppState>, on_done: Rc<dyn Fn()>) {
    let n = paths.len();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for t in state.db.all_tags().unwrap_or_default() {
        counts.insert(t.0, 0);
    }
    for p in &paths {
        for t in state.db.tags_of(p).unwrap_or_default() {
            *counts.entry(t).or_default() += 1;
        }
    }

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let rows: Rc<RefCell<BTreeMap<String, TagRow>>> = Rc::default();

    let add_row = {
        let (list, rows) = (list.clone(), rows.clone());
        Rc::new(move |tag: &str, initial: usize, n: usize, active: bool| {
            let row = adw::ActionRow::new();
            row.set_use_markup(false);
            row.set_title(tag);
            row.add_prefix(&tags::dot(tag));
            let cb = gtk::CheckButton::new();
            cb.set_valign(gtk::Align::Center);
            if initial > 0 && initial < n {
                cb.set_inconsistent(true); // some, not all
            } else {
                cb.set_active(active || initial == n && n > 0);
            }
            row.add_suffix(&cb);
            row.set_activatable_widget(Some(&cb));
            list.append(&row);
            rows.borrow_mut().insert(tag.to_string(), TagRow { check: cb, initial });
        })
    };
    for (t, c) in &counts {
        add_row(t, *c, n, false);
    }

    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("New tag"));
    entry.set_hexpand(true);
    let add = gtk::Button::with_label("Add");
    let new_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    new_row.append(&entry);
    new_row.append(&add);
    let (rows2, add_row2, entry2) = (rows.clone(), add_row.clone(), entry.clone());
    let do_add = Rc::new(move || {
        let t = entry2.text().trim().to_string();
        if t.is_empty() || t.len() > 40 || t.contains(['\n', '\0']) {
            return;
        }
        match rows2.borrow().get(&t) {
            Some(r) => {
                r.check.set_inconsistent(false);
                r.check.set_active(true);
            }
            None => {
                drop(rows2.borrow());
                add_row2(&t, 0, n, true);
            }
        }
        entry2.set_text("");
    });
    let d = do_add.clone();
    add.connect_clicked(move |_| d());
    entry.connect_activate(move |_| do_add());

    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_vexpand(true);
    sw.set_min_content_height(200);
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    apply.set_halign(gtk::Align::End);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let sub = gtk::Label::new(Some(&if n == 1 {
        paths[0].file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        format!("{n} items")
    }));
    sub.add_css_class("dim-label");
    sub.set_xalign(0.0);
    body.append(&sub);
    body.append(&sw);
    body.append(&new_row);
    body.append(&apply);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&body));
    let dialog = adw::Dialog::builder().title("Tags").content_width(420).content_height(520).child(&tv).build();

    let dd = dialog.clone();
    apply.connect_clicked(move |_| {
        let (mut add_t, mut rem_t) = (Vec::new(), Vec::new());
        for (tag, r) in rows.borrow().iter() {
            if r.check.is_inconsistent() {
                continue; // untouched mixed state
            }
            if r.check.is_active() && r.initial < n {
                add_t.push(tag.clone());
            } else if !r.check.is_active() && r.initial > 0 {
                rem_t.push(tag.clone());
            }
        }
        let _ = state.db.add_tags(&paths, &add_t);
        let _ = state.db.remove_tags(&paths, &rem_t);
        dd.close();
        on_done();
    });
    dialog.present(Some(parent));
}

/// List files carrying `tag`; `on_open(path, reveal)`.
pub fn browse(parent: &gtk::Window, tag: &str, state: Rc<AppState>, on_open: Rc<dyn Fn(PathBuf)>) {
    let files = state.db.files_with_tag(tag).unwrap_or_default();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);
    if files.is_empty() {
        let r = adw::ActionRow::new();
        r.set_title("No items have this tag");
        list.append(&r);
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_vexpand(true);
    tv.set_content(Some(&sw));
    let dialog = adw::Dialog::builder()
        .title(format!("Tagged “{tag}”"))
        .content_width(560)
        .content_height(480)
        .child(&tv)
        .build();
    for f in files {
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        row.set_title(&f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        row.set_subtitle(&f.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
        row.set_activatable(f.exists());
        if !f.exists() {
            row.add_css_class("dim-label");
            row.set_tooltip_text(Some("This item no longer exists"));
        }
        let (cb, dd) = (on_open.clone(), dialog.clone());
        row.connect_activated(move |_| {
            dd.close();
            cb(f.clone());
        });
        list.append(&row);
    }
    dialog.present(Some(parent));
}
