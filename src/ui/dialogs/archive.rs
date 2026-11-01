//! Archive dialogs: contents browser, compress options, passphrase prompt.

use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use crate::core::filesystem::human_size;
use crate::services::archive::{Entry, Format, Level};

pub async fn prompt_passphrase(parent: &gtk::Window, archive: &str, retry: bool) -> Option<String> {
    let body = if retry {
        format!("That passphrase didn’t work for “{archive}”.")
    } else {
        format!("“{archive}” is password protected.")
    };
    let d = adw::AlertDialog::new(Some("Passphrase required"), Some(&body));
    d.add_responses(&[("cancel", "Cancel"), ("ok", "Unlock")]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    let entry = gtk::PasswordEntry::new();
    entry.set_show_peek_icon(true);
    entry.set_activates_default(true);
    d.set_extra_child(Some(&entry));
    if d.choose_future(Some(parent)).await != "ok" {
        return None;
    }
    let t = entry.text().to_string();
    (!t.is_empty()).then_some(t)
}

pub struct CompressChoice {
    pub name: String,
    pub format: Format,
    pub level: Level,
}

pub async fn compress(parent: &gtk::Window, default_stem: &str, count: usize) -> Option<CompressChoice> {
    let body = if count == 1 { "Compress 1 item".to_string() } else { format!("Compress {count} items") };
    let d = adw::AlertDialog::new(Some("Create Archive"), Some(&body));
    d.add_responses(&[("cancel", "Cancel"), ("ok", "Create")]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");

    let name = gtk::Entry::new();
    name.set_text(default_stem);
    name.set_activates_default(true);
    let fmts: Vec<&str> = Format::ALL.iter().map(|f| f.label()).collect();
    let fmt = gtk::DropDown::from_strings(&fmts);
    let lvls: Vec<&str> = Level::ALL.iter().map(|l| l.label()).collect();
    let lvl = gtk::DropDown::from_strings(&lvls);
    lvl.set_selected(1);
    let l = lvl.clone();
    fmt.connect_selected_notify(move |f| l.set_sensitive(Format::ALL[f.selected() as usize].has_levels()));

    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    for (i, (label, w)) in
        [("Name", name.upcast_ref::<gtk::Widget>()), ("Format", fmt.upcast_ref()), ("Compression", lvl.upcast_ref())]
            .into_iter()
            .enumerate()
    {
        let l = gtk::Label::new(Some(label));
        l.set_xalign(0.0);
        grid.attach(&l, 0, i as i32, 1, 1);
        w.set_hexpand(true);
        grid.attach(w, 1, i as i32, 1, 1);
    }
    d.set_extra_child(Some(&grid));
    let n = name.clone();
    glib::idle_add_local_once(move || {
        n.grab_focus();
    });
    if d.choose_future(Some(parent)).await != "ok" {
        return None;
    }
    let nm = name.text().trim().to_string();
    if nm.is_empty() || nm.contains('/') {
        return None;
    }
    Some(CompressChoice {
        name: nm,
        format: Format::ALL[fmt.selected() as usize],
        level: Level::ALL[lvl.selected() as usize],
    })
}

/// Read-only listing of the archive. `on_extract(true)` = here, `false` = choose a folder.
pub fn browse(parent: &gtk::Window, archive_name: &str, entries: Vec<Entry>, on_extract: Rc<dyn Fn(bool)>) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let mut entries = entries;
    entries.sort_by_key(|a| a.path.to_lowercase());
    let (files, total): (usize, u64) =
        (entries.iter().filter(|e| !e.is_dir).count(), entries.iter().map(|e| e.size).sum());
    for e in entries {
        store.append(&glib::BoxedAnyObject::new(e));
    }

    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Image::new());
        let l = gtk::Label::new(None);
        l.set_xalign(0.0);
        l.set_hexpand(true);
        l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&l);
        let s = gtk::Label::new(None);
        s.add_css_class("dim-label");
        row.append(&s);
        item.set_child(Some(&row));
    });
    f.connect_bind(|_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let obj = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
        let e = obj.borrow::<Entry>();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let img = row.first_child().and_downcast::<gtk::Image>().unwrap();
        let name = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let size = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let ct = if e.is_dir {
            "inode/directory".to_string()
        } else {
            gio::content_type_guess(Some(&e.path), None).0.to_string()
        };
        img.set_from_gicon(&crate::core::filesystem::icon_for(&ct, e.is_dir));
        name.set_text(&e.path);
        size.set_text(&if e.is_dir { String::new() } else { human_size(e.size as i64) });
    });
    let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store))), Some(f));
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_vexpand(true);

    let summary = gtk::Label::new(Some(&format!("{files} files · {} uncompressed", human_size(total as i64))));
    summary.add_css_class("dim-label");
    summary.set_xalign(0.0);
    summary.set_hexpand(true);
    let here = gtk::Button::with_label("Extract Here");
    here.add_css_class("suggested-action");
    let to = gtk::Button::with_label("Extract To…");
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.set_margin_top(8);
    bar.set_margin_bottom(8);
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    bar.append(&summary);
    bar.append(&to);
    bar.append(&here);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&sw));
    tv.add_bottom_bar(&bar);
    let d = adw::Dialog::builder().title(archive_name).content_width(600).content_height(520).child(&tv).build();
    let (cb, dd) = (on_extract.clone(), d.clone());
    here.connect_clicked(move |_| {
        dd.close();
        cb(true);
    });
    let dd = d.clone();
    to.connect_clicked(move |_| {
        dd.close();
        on_extract(false);
    });
    d.present(Some(parent));
}
