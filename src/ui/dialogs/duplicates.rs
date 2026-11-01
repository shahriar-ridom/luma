//! Duplicate finder. Never offers to remove every copy of a file, and sends
//! selections to the Trash (undoable) rather than deleting.

use adw::prelude::*;
use gtk::glib;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::atomic::Ordering};

use crate::core::duplicates::{self, DupEvent};
use crate::core::filesystem::human_size;

type Group = Vec<(PathBuf, gtk::CheckButton)>;

pub fn show(parent: &gtk::Window, root: PathBuf, on_trash: Rc<dyn Fn(Vec<PathBuf>)>) {
    let sizes = gtk::DropDown::from_strings(&["Any size", "At least 1 KB", "At least 1 MB", "At least 100 MB"]);
    sizes.set_selected(1);
    let hidden = gtk::CheckButton::with_label("Hidden files");
    let scan = gtk::Button::with_label("Scan");
    scan.add_css_class("suggested-action");
    let where_ = gtk::Label::new(Some(&format!("In {}", root.display())));
    where_.set_xalign(0.0);
    where_.set_hexpand(true);
    where_.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    where_.add_css_class("dim-label");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.set_margin_top(10);
    top.set_margin_bottom(10);
    top.set_margin_start(12);
    top.set_margin_end(12);
    for w in [where_.upcast_ref::<gtk::Widget>(), sizes.upcast_ref(), hidden.upcast_ref(), scan.upcast_ref()] {
        top.append(w);
    }

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_bottom(12);
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_vexpand(true);

    let status = gtk::Label::new(Some("Press Scan to look for identical files."));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.add_css_class("dim-label");
    let spinner = gtk::Spinner::new();
    let extras = gtk::Button::with_label("Select Extras");
    extras.set_tooltip_text(Some("In every group, select all but the first file"));
    let clear = gtk::Button::with_label("Clear");
    let trash = gtk::Button::with_label("Move Selected to Trash");
    trash.add_css_class("destructive-action");
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    foot.set_margin_top(8);
    foot.set_margin_bottom(8);
    foot.set_margin_start(12);
    foot.set_margin_end(12);
    for w in [
        spinner.upcast_ref::<gtk::Widget>(),
        status.upcast_ref(),
        extras.upcast_ref(),
        clear.upcast_ref(),
        trash.upcast_ref(),
    ] {
        foot.append(w);
    }

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&top);
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    body.append(&sw);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&body));
    tv.add_bottom_bar(&foot);
    let dialog =
        adw::Dialog::builder().title("Find Duplicate Files").content_width(780).content_height(640).child(&tv).build();

    let groups: Rc<RefCell<Vec<Group>>> = Rc::default();
    let cancel: Rc<RefCell<Option<Rc<std::sync::atomic::AtomicBool>>>> = Rc::default();
    let running: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>> = Rc::default();
    let _ = &cancel;

    let (g2, list2, status2, spinner2, running2, root2) =
        (groups.clone(), list.clone(), status.clone(), spinner.clone(), running.clone(), root.clone());
    let (sizes2, hidden2) = (sizes.clone(), hidden.clone());
    scan.connect_clicked(move |_| {
        if let Some(c) = running2.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
        while let Some(c) = list2.first_child() {
            list2.remove(&c);
        }
        g2.borrow_mut().clear();
        let min = [0u64, 1 << 10, 1 << 20, 100 << 20][sizes2.selected() as usize];
        let (c, rx) = duplicates::spawn(root2.clone(), hidden2.is_active(), min);
        *running2.borrow_mut() = Some(c);
        spinner2.start();
        status2.set_text("Scanning…");
        let (groups, list, status, spinner) = (g2.clone(), list2.clone(), status2.clone(), spinner2.clone());
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                match ev {
                    DupEvent::Progress(stage, done, total) => {
                        status.set_text(&if total > 0 {
                            format!("{stage}… {done} of {total}")
                        } else {
                            format!("{stage}… {done} files")
                        });
                    }
                    DupEvent::Group(g) => {
                        let exp = adw::ExpanderRow::new();
                        exp.set_use_markup(false);
                        exp.set_title(&format!("{} copies of a {} file", g.files.len(), human_size(g.size as i64)));
                        exp.set_subtitle(&format!(
                            "{} wasted",
                            human_size((g.size * (g.files.len() as u64 - 1)) as i64)
                        ));
                        let mut checks: Group = Vec::new();
                        for f in g.files {
                            let row = adw::ActionRow::new();
                            row.set_use_markup(false);
                            row.set_title(&f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                            row.set_subtitle(&f.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
                            let cb = gtk::CheckButton::new();
                            cb.set_valign(gtk::Align::Center);
                            row.add_prefix(&cb);
                            row.set_activatable_widget(Some(&cb));
                            exp.add_row(&row);
                            checks.push((f, cb));
                        }
                        list.append(&exp);
                        groups.borrow_mut().push(checks);
                    }
                    DupEvent::Done { groups: n, wasted } => {
                        spinner.stop();
                        status.set_text(&if n == 0 {
                            "No duplicates found.".to_string()
                        } else {
                            format!("{n} groups · {} could be freed", human_size(wasted as i64))
                        });
                    }
                }
            }
            spinner.stop();
        });
    });

    let g = groups.clone();
    extras.connect_clicked(move |_| {
        for grp in g.borrow().iter() {
            for (i, (_, cb)) in grp.iter().enumerate() {
                cb.set_active(i != 0);
            }
        }
    });
    let g = groups.clone();
    clear.connect_clicked(move |_| {
        for grp in g.borrow().iter() {
            for (_, cb) in grp {
                cb.set_active(false);
            }
        }
    });
    let (g, st, dd, p) = (groups, status.clone(), dialog.clone(), parent.clone());
    trash.connect_clicked(move |_| {
        let groups = g.borrow();
        if groups.iter().any(|grp| grp.iter().all(|(_, cb)| cb.is_active())) {
            crate::ui::dialogs::error(
                &p,
                "Keep at least one copy",
                "Every copy in one of the groups is selected. Uncheck at least one file in each group.",
            );
            return;
        }
        let picked: Vec<PathBuf> =
            groups.iter().flatten().filter(|(_, cb)| cb.is_active()).map(|(p, _)| p.clone()).collect();
        if picked.is_empty() {
            st.set_text("Nothing selected.");
            return;
        }
        dd.close();
        on_trash(picked);
    });
    let r = running;
    dialog.connect_closed(move |_| {
        if let Some(c) = r.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
    });
    dialog.present(Some(parent));
}
