//! Side-by-side facts plus a line diff (text) or first differing byte (binary).

use adw::prelude::*;
use gtk::{gdk, glib};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::core::compare::{self, Kind, Outcome};
use crate::core::filesystem::human_size;

fn facts(p: &std::path::Path) -> String {
    let md = std::fs::metadata(p);
    let size = md.as_ref().map(|m| human_size(m.len() as i64)).unwrap_or_else(|_| "?".into());
    let when = md
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
        .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    format!("{size} · modified {when}")
}

pub fn show(parent: &gtk::Window, a: PathBuf, b: PathBuf) {
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let header = gtk::Grid::new();
    header.set_column_spacing(16);
    header.set_row_spacing(2);
    header.set_margin_top(10);
    header.set_margin_bottom(10);
    header.set_margin_start(12);
    header.set_margin_end(12);
    for (col, (tag, p)) in [("−", &a), ("+", &b)].into_iter().enumerate() {
        let t = gtk::Label::new(Some(&format!("{tag} {}", name(p))));
        t.add_css_class("heading");
        t.set_xalign(0.0);
        t.set_hexpand(true);
        t.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let f = gtk::Label::new(Some(&facts(p)));
        f.add_css_class("dim-label");
        f.set_xalign(0.0);
        header.attach(&t, col as i32, 0, 1, 1);
        header.attach(&f, col as i32, 1, 1, 1);
    }
    let verdict = gtk::Label::new(Some("Comparing…"));
    verdict.set_xalign(0.0);
    verdict.set_margin_start(12);
    verdict.set_margin_bottom(8);
    let spinner = gtk::Spinner::new();
    spinner.start();

    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_left_margin(8);
    let buf = view.buffer();
    let table = buf.tag_table();
    for (n, rgba) in [("add", (0.2, 0.7, 0.3, 0.22)), ("del", (0.85, 0.25, 0.25, 0.22))] {
        table.add(
            &gtk::TextTag::builder()
                .name(n)
                .paragraph_background_rgba(&gdk::RGBA::new(rgba.0, rgba.1, rgba.2, rgba.3))
                .build(),
        );
    }
    table.add(
        &gtk::TextTag::builder().name("hunk").weight(700).foreground_rgba(&gdk::RGBA::new(0.5, 0.5, 0.8, 1.0)).build(),
    );
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&view));
    sw.set_vexpand(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&header);
    body.append(&verdict);
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    body.append(&sw);
    let tv = adw::ToolbarView::new();
    let hb = adw::HeaderBar::new();
    hb.pack_end(&spinner);
    tv.add_top_bar(&hb);
    tv.set_content(Some(&body));
    let dialog =
        adw::Dialog::builder().title("Compare Files").content_width(820).content_height(620).child(&tv).build();

    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    dialog.connect_closed(move |_| c.store(true, Ordering::Relaxed));
    glib::spawn_future_local(async move {
        let c = cancel.clone();
        let r = gtk::gio::spawn_blocking(move || compare::compare(&a, &b, &c)).await;
        spinner.stop();
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match r {
            Ok(Ok(Outcome::Identical)) => verdict.set_text("The files are identical."),
            Ok(Ok(Outcome::Binary { first_difference })) => verdict.set_text(&match first_difference {
                Some(o) => format!("Binary files differ — first difference at byte {o}."),
                None => "The files differ.".into(),
            }),
            Ok(Ok(Outcome::Text { lines, added, removed, truncated })) => {
                verdict.set_text(&format!(
                    "{added} line{} added, {removed} removed{}",
                    if added == 1 { "" } else { "s" },
                    if truncated { " (diff truncated)" } else { "" }
                ));
                for l in lines {
                    let (prefix, tag) = match l.kind {
                        Kind::Context => ("  ", None),
                        Kind::Added => ("+ ", Some("add")),
                        Kind::Removed => ("− ", Some("del")),
                        Kind::Hunk => ("", Some("hunk")),
                    };
                    let mut end = buf.end_iter();
                    let text = format!("{prefix}{}\n", l.text);
                    match tag {
                        Some(t) => buf.insert_with_tags_by_name(&mut end, &text, &[t]),
                        None => buf.insert(&mut end, &text),
                    }
                }
            }
            Ok(Err(e)) => verdict.set_text(&format!("Could not compare: {e}")),
            Err(_) => verdict.set_text("Comparison failed."),
        }
    });
    dialog.present(Some(parent));
}
