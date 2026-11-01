//! Search window: name / content / type / size / date filters over a folder,
//! streaming results from a worker thread.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::application::state::AppState;
use crate::core::filesystem::human_size;
use crate::core::search::{self, Hit, KindFilter, SearchEvent, SearchQuery};

const SIZES: &[(&str, Option<u64>, Option<u64>)] = &[
    ("Any size", None, None),
    ("Larger than 1 MB", Some(1 << 20), None),
    ("Larger than 10 MB", Some(10 << 20), None),
    ("Larger than 100 MB", Some(100 << 20), None),
    ("Larger than 1 GB", Some(1 << 30), None),
    ("Smaller than 100 KB", None, Some(100 << 10)),
];
const DATES: &[(&str, Option<i64>)] = &[
    ("Any time", None),
    ("Past 24 hours", Some(86_400)),
    ("Past 7 days", Some(7 * 86_400)),
    ("Past 30 days", Some(30 * 86_400)),
    ("Past year", Some(365 * 86_400)),
];

struct State {
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    root: RefCell<PathBuf>,
}

/// `on_open(path, reveal)`: reveal = show it in its folder instead of opening.
pub fn show(parent: &gtk::Window, root: PathBuf, app: Rc<AppState>, on_open: Rc<dyn Fn(PathBuf, bool)>) {
    let st = Rc::new(State { cancel: RefCell::default(), root: RefCell::new(root) });

    let name = gtk::SearchEntry::new();
    name.set_placeholder_text(Some("File name — supports * and ?"));
    name.set_hexpand(true);
    let content = gtk::Entry::new();
    content.set_placeholder_text(Some("Containing text"));
    let go = gtk::Button::with_label("Search");
    go.add_css_class("suggested-action");
    let history = gtk::MenuButton::new();
    history.set_icon_name("fm-history-symbolic");
    history.set_tooltip_text(Some("Search history"));

    let kinds: Vec<&str> = KindFilter::ALL.iter().map(|k| k.label()).collect();
    let kind = gtk::DropDown::from_strings(&kinds);
    let sizes: Vec<&str> = SIZES.iter().map(|s| s.0).collect();
    let size = gtk::DropDown::from_strings(&sizes);
    let dates: Vec<&str> = DATES.iter().map(|s| s.0).collect();
    let date = gtk::DropDown::from_strings(&dates);
    let exts = gtk::Entry::new();
    exts.set_placeholder_text(Some("Extensions: txt, md"));
    exts.set_width_chars(16);
    let recursive = gtk::CheckButton::with_label("Subfolders");
    recursive.set_active(true);
    let hidden = gtk::CheckButton::with_label("Hidden files");

    let where_label = gtk::Label::new(None);
    where_label.set_xalign(0.0);
    where_label.set_hexpand(true);
    where_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    where_label.add_css_class("dim-label");
    let change = gtk::Button::with_label("Change…");
    change.add_css_class("flat");

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.append(&name);
    top.append(&history);
    top.append(&go);
    let opts = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for w in [kind.upcast_ref::<gtk::Widget>(), size.upcast_ref(), date.upcast_ref(), exts.upcast_ref()] {
        opts.append(w);
    }
    let checks = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    checks.append(&recursive);
    checks.append(&hidden);
    let loc = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    loc.append(&where_label);
    loc.append(&change);
    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    {
        let m = &form;
        m.set_margin_top(10);
        m.set_margin_bottom(10);
        m.set_margin_start(12);
        m.set_margin_end(12);
    }
    form.append(&top);
    form.append(&content);
    form.append(&opts);
    form.append(&checks);
    form.append(&loc);

    // ---- results ---------------------------------------------------------------
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        row.append(&gtk::Image::new());
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        col.set_hexpand(true);
        for (i, cls) in ["", "caption", "caption"].iter().enumerate() {
            let l = gtk::Label::new(None);
            l.set_xalign(0.0);
            l.set_ellipsize(if i == 1 { gtk::pango::EllipsizeMode::Start } else { gtk::pango::EllipsizeMode::End });
            if !cls.is_empty() {
                l.add_css_class(cls);
                l.add_css_class("dim-label");
            }
            col.append(&l);
        }
        row.append(&col);
        let s = gtk::Label::new(None);
        s.add_css_class("dim-label");
        row.append(&s);
        item.set_child(Some(&row));
    });
    f.connect_bind(|_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let obj = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
        let h = obj.borrow::<Hit>();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let img = row.first_child().and_downcast::<gtk::Image>().unwrap();
        let col = img.next_sibling().and_downcast::<gtk::Box>().unwrap();
        let size = col.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let name = col.first_child().and_downcast::<gtk::Label>().unwrap();
        let dir = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let snip = dir.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let fname = h.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ct = if h.is_dir {
            "inode/directory".to_string()
        } else {
            gio::content_type_guess(Some(&fname), None).0.to_string()
        };
        img.set_from_gicon(&crate::core::filesystem::icon_for(&ct, h.is_dir));
        name.set_text(&fname);
        dir.set_text(&h.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
        snip.set_visible(h.snippet.is_some());
        snip.set_text(h.snippet.as_deref().unwrap_or(""));
        let when = glib::DateTime::from_unix_local(h.mtime)
            .ok()
            .and_then(|d| d.format("%Y-%m-%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        size.set_text(&if h.is_dir { when } else { format!("{} · {when}", human_size(h.size as i64)) });
    });
    let sel = gtk::SingleSelection::new(Some(store.clone()));
    sel.set_autoselect(false);
    let list = gtk::ListView::new(Some(sel.clone()), Some(f));
    list.add_css_class("rich-list");
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&list));
    sw.set_vexpand(true);

    let spinner = gtk::Spinner::new();
    let status = gtk::Label::new(Some("Enter a name or text and press Search."));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_hexpand(true);
    let reveal = gtk::Button::with_label("Show in Folder");
    let open = gtk::Button::with_label("Open");
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    foot.set_margin_top(8);
    foot.set_margin_bottom(8);
    foot.set_margin_start(12);
    foot.set_margin_end(12);
    foot.append(&spinner);
    foot.append(&status);
    foot.append(&reveal);
    foot.append(&open);

    let tv = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Search", "")));
    tv.add_top_bar(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&form);
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    body.append(&sw);
    tv.set_content(Some(&body));
    tv.add_bottom_bar(&foot);
    let win = adw::Window::builder().transient_for(parent).default_width(780).default_height(680).content(&tv).build();
    win.set_title(Some("Search"));

    let sync_where = {
        let (st, l) = (st.clone(), where_label.clone());
        Rc::new(move || l.set_text(&format!("In {}", st.root.borrow().display())))
    };
    sync_where();

    // ---- behaviour ---------------------------------------------------------------
    let run = {
        let (st, store, status, spinner, app) =
            (st.clone(), store.clone(), status.clone(), spinner.clone(), app.clone());
        let (name, content, kind, size, date, exts, recursive, hidden) = (
            name.clone(),
            content.clone(),
            kind.clone(),
            size.clone(),
            date.clone(),
            exts.clone(),
            recursive.clone(),
            hidden.clone(),
        );
        Rc::new(move || {
            if let Some(c) = st.cancel.borrow_mut().take() {
                c.store(true, Ordering::Relaxed);
            }
            store.remove_all();
            let (n, t) = (name.text().trim().to_string(), content.text().trim().to_string());
            if n.is_empty() && t.is_empty() && kind.selected() == 0 && exts.text().trim().is_empty() {
                status.set_text("Enter a name, text, type or extension to search for.");
                return;
            }
            let _ = app.db.record_search(&n);
            let (min_size, max_size) = (SIZES[size.selected() as usize].1, SIZES[size.selected() as usize].2);
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            let q = SearchQuery {
                root: st.root.borrow().clone(),
                name: n,
                content: t,
                exts: exts
                    .text()
                    .split([',', ' ', ';'])
                    .map(|e| e.trim().trim_start_matches('.').to_string())
                    .filter(|e| !e.is_empty())
                    .collect(),
                kind: KindFilter::ALL[kind.selected() as usize],
                min_size,
                max_size,
                modified_after: DATES[date.selected() as usize].1.map(|s| now - s),
                recursive: recursive.is_active(),
                hidden: hidden.is_active(),
                no_content: vec![crate::integrations::cloud::base_dir()],
            };
            let (cancel, rx) = search::spawn(q);
            *st.cancel.borrow_mut() = Some(cancel);
            spinner.start();
            status.set_text("Searching…");
            let (store, status, spinner) = (store.clone(), status.clone(), spinner.clone());
            glib::spawn_future_local(async move {
                while let Ok(ev) = rx.recv().await {
                    match ev {
                        SearchEvent::Hits(h) => {
                            let objs: Vec<glib::BoxedAnyObject> =
                                h.into_iter().map(glib::BoxedAnyObject::new).collect();
                            store.extend_from_slice(&objs);
                            status.set_text(&format!("{} results…", store.n_items()));
                        }
                        SearchEvent::Done { scanned, found, truncated } => {
                            spinner.stop();
                            status.set_text(&if truncated {
                                format!("First {found} results shown — refine your search ({scanned} items scanned)")
                            } else {
                                format!("{found} result{} ({scanned} items scanned)", if found == 1 { "" } else { "s" })
                            });
                        }
                    }
                }
            });
        })
    };
    let r = run.clone();
    go.connect_clicked(move |_| r());
    let r = run.clone();
    name.connect_activate(move |_| r());
    let r = run.clone();
    content.connect_activate(move |_| r());
    let r = run.clone();
    exts.connect_activate(move |_| r());

    let picked = {
        let (sel, store) = (sel.clone(), store.clone());
        Rc::new(move || -> Option<Hit> {
            let o = store.item(sel.selected())?.downcast::<glib::BoxedAnyObject>().ok()?;
            let h = o.borrow::<Hit>().clone();
            Some(h)
        })
    };
    let (p, cb) = (picked.clone(), on_open.clone());
    list.connect_activate(move |lv, pos| {
        if let Some(o) = lv.model().and_then(|m| m.item(pos)).and_downcast::<glib::BoxedAnyObject>() {
            let _ = &p;
            cb(o.borrow::<Hit>().path.clone(), false);
        }
    });
    let (p, cb) = (picked.clone(), on_open.clone());
    open.connect_clicked(move |_| {
        if let Some(h) = p() {
            cb(h.path, false);
        }
    });
    let (p, cb) = (picked, on_open);
    reveal.connect_clicked(move |_| {
        if let Some(h) = p() {
            cb(h.path, true);
        }
    });

    let (st2, sync, w) = (st.clone(), sync_where, win.clone());
    change.connect_clicked(move |_| {
        let (st, sync, w) = (st2.clone(), sync.clone(), w.clone());
        glib::spawn_future_local(async move {
            let dlg = gtk::FileDialog::new();
            dlg.set_initial_folder(Some(&gio::File::for_path(&*st.root.borrow())));
            if let Some(p) = dlg.select_folder_future(Some(&w)).await.ok().and_then(|f| f.path()) {
                *st.root.borrow_mut() = p;
                sync();
            }
        });
    });

    // Search history popover, rebuilt each time it opens.
    let (app2, name2) = (app, name.clone());
    history.set_create_popup_func(move |btn| {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let items = app2.db.search_history(12).unwrap_or_default();
        let pop = gtk::Popover::new();
        if items.is_empty() {
            col.append(&gtk::Label::new(Some("No searches yet")));
        }
        for q in items {
            let b = gtk::Button::with_label(&q);
            b.add_css_class("flat");
            let (n, p, q2) = (name2.clone(), pop.clone(), q.clone());
            b.connect_clicked(move |_| {
                n.set_text(&q2);
                p.popdown();
            });
            col.append(&b);
        }
        col.set_margin_top(6);
        col.set_margin_bottom(6);
        col.set_margin_start(6);
        col.set_margin_end(6);
        pop.set_child(Some(&col));
        btn.set_popover(Some(&pop));
    });

    let st3 = st;
    win.connect_close_request(move |_| {
        if let Some(c) = st3.cancel.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
        glib::Propagation::Proceed
    });
    win.present();
    name.grab_focus();
}
