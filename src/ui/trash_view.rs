//! The Trash: every trashed item across the home trash and each mounted
//! drive's own trash, with where it came from and when it was deleted.
//! Right-click restores or deletes for good; "Empty Trash" clears it all.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc, sync::atomic::AtomicBool};

use crate::core::operations::trash::{self, Item};
use crate::ui::{dialogs, operations::OpsUi};

/// A trashed item plus what the list shows about it (read off the main thread).
#[derive(Clone)]
struct Row {
    item: Item,
    is_dir: bool,
    size: u64,
}

pub struct TrashView {
    pub widget: gtk::Box,
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    stack: gtk::Stack,
    empty_btn: gtk::Button,
    ops: OpsUi,
    window: glib::WeakRef<gtk::Window>,
    /// Pane status line, kept in step with the list.
    on_summary: RefCell<Option<Rc<dyn Fn(&str)>>>,
    loading: std::cell::Cell<u64>,
    totals: std::cell::Cell<(usize, u64)>,
    me: std::rc::Weak<TrashView>,
}

fn row_of(o: &glib::Object) -> Option<Row> {
    o.downcast_ref::<glib::BoxedAnyObject>().map(|b| b.borrow::<Row>().clone())
}

/// "2024-03-01T09:00:00" → "2024-03-01 09:00"
fn when(deleted: &str) -> String {
    deleted.get(..16).map(|s| s.replace('T', " ")).unwrap_or_else(|| deleted.to_string())
}

/// Right-clicking a cell selects its row first (unless it's already part of
/// the selection), so the menu acts on what was clicked.
fn select_on_right_click(item: &gtk::ListItem, cell: &impl IsA<gtk::Widget>, sel: &gtk::MultiSelection) {
    let g = gtk::GestureClick::new();
    g.set_button(3);
    let (wi, sel) = (item.downgrade(), sel.clone());
    g.connect_pressed(move |_, _, _, _| {
        if let Some(item) = wi.upgrade()
            && !sel.is_selected(item.position())
        {
            sel.select_item(item.position(), true);
        }
    });
    cell.add_controller(g);
}

fn text_column(
    title: &str,
    sel: &gtk::MultiSelection,
    text: impl Fn(&Row) -> String + 'static,
) -> gtk::ColumnViewColumn {
    let f = gtk::SignalListItemFactory::new();
    let sel = sel.clone();
    f.connect_setup(move |_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let l = gtk::Label::new(None);
        l.set_xalign(0.0);
        l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        select_on_right_click(item, &l, &sel);
        item.set_child(Some(&l));
    });
    f.connect_bind(move |_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let (Some(r), Some(l)) = (item.item().and_then(|i| row_of(&i)), item.child().and_downcast::<gtk::Label>())
        else {
            return;
        };
        l.set_text(&text(&r));
    });
    let c = gtk::ColumnViewColumn::new(Some(title), Some(f));
    c.set_resizable(true);
    c
}

impl TrashView {
    pub fn new(ops: OpsUi, window: glib::WeakRef<gtk::Window>) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let view = gtk::ColumnView::new(Some(selection.clone()));
        view.add_css_class("fm-list");
        view.set_show_row_separators(false);
        view.set_enable_rubberband(true);

        // Name, with the item's icon.
        let nf = gtk::SignalListItemFactory::new();
        let s = selection.clone();
        nf.connect_setup(move |_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            b.append(&gtk::Image::new());
            let l = gtk::Label::new(None);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            b.append(&l);
            select_on_right_click(item, &b, &s);
            item.set_child(Some(&b));
        });
        nf.connect_bind(|_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(r), Some(b)) = (item.item().and_then(|i| row_of(&i)), item.child().and_downcast::<gtk::Box>())
            else {
                return;
            };
            let name = r.item.name();
            let mime = if r.is_dir {
                "inode/directory".to_string()
            } else {
                gio::content_type_guess(Some(&name), None).0.to_string()
            };
            if let Some(img) = b.first_child().and_downcast::<gtk::Image>() {
                img.set_from_gicon(&crate::core::filesystem::icon_for(&mime, r.is_dir));
            }
            if let Some(l) = b.last_child().and_downcast::<gtk::Label>() {
                l.set_text(&name);
            }
        });
        let name = gtk::ColumnViewColumn::new(Some("Name"), Some(nf));
        name.set_expand(true);
        name.set_resizable(true);
        let from = text_column("Original Location", &selection, |r| {
            r.item.orig.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "Unknown".into())
        });
        from.set_fixed_width(220);
        let deleted = text_column("Deleted", &selection, |r| when(&r.item.deleted));
        deleted.set_fixed_width(170);
        let size = text_column("Size", &selection, |r| {
            if r.is_dir { "—".into() } else { glib::format_size(r.size).to_string() }
        });
        size.set_fixed_width(90);
        for c in [&name, &from, &deleted, &size] {
            view.append_column(c);
        }
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&view));
        sw.set_vexpand(true);
        // Narrow panes (split view) keep the name readable: drop Size, then
        // the original location.
        let bin = adw::BreakpointBin::new();
        bin.set_size_request(200, 120);
        bin.set_child(Some(&sw));
        for (max, hide) in [(560, vec![&size]), (420, vec![&size, &from])] {
            let b = adw::Breakpoint::new(adw::BreakpointCondition::parse(&format!("max-width: {max}px")).unwrap());
            for c in hide {
                b.add_setter(c, "visible", Some(&false.to_value()));
            }
            bin.add_breakpoint(b);
        }

        let empty = adw::StatusPage::builder()
            .icon_name("luma-trash-symbolic")
            .title("Trash is empty")
            .description("Items you delete stay here until you empty the Trash.")
            .build();
        empty.add_css_class("compact");
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(140);
        stack.add_named(&bin, Some("list"));
        stack.add_named(&empty, Some("empty"));

        // The count lives in the status line; this says what the list is.
        let summary = gtk::Label::new(Some("Deleted items from all drives"));
        summary.set_xalign(0.0);
        summary.set_hexpand(true);
        summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        summary.add_css_class("fm-trash-note");
        let empty_btn = gtk::Button::with_label("Empty Trash");
        empty_btn.add_css_class("fm-trash-empty");
        empty_btn.set_action_name(Some("trash.empty"));
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("fm-trash-bar");
        bar.append(&summary);
        bar.append(&empty_btn);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("fm-trash");
        widget.append(&bar);
        widget.append(&stack);

        let this = Rc::new_cyclic(|me| Self {
            widget,
            store,
            selection,
            stack,
            empty_btn,
            ops,
            window,
            on_summary: RefCell::default(),
            loading: std::cell::Cell::new(0),
            totals: std::cell::Cell::new((0, 0)),
            me: me.clone(),
        });

        // Actions: the context menu and the bar's button use these.
        let group = gio::SimpleActionGroup::new();
        let act = |name: &str, f: fn(&Rc<TrashView>)| {
            let a = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(&this);
            a.connect_activate(move |_, _| {
                if let Some(t) = w.upgrade() {
                    f(&t);
                }
            });
            group.add_action(&a);
        };
        act("restore", |t| t.restore_selected());
        act("delete", |t| t.delete_selected());
        act("empty", |t| t.empty());
        this.widget.insert_action_group("trash", Some(&group));

        // Right-click: act on the clicked row (selecting it if needed).
        let click = gtk::GestureClick::new();
        click.set_button(3);
        let w = Rc::downgrade(&this);
        click.connect_pressed(move |g, _, x, y| {
            let Some(t) = w.upgrade() else { return };
            let Some(view) = g.widget() else { return };
            if t.selection.selection().is_empty() {
                return;
            }
            let menu = gio::Menu::new();
            menu.append(Some("Restore"), Some("trash.restore"));
            menu.append(Some("Delete Permanently…"), Some("trash.delete"));
            let Some(pt) = view.compute_point(&t.widget, &gtk::graphene::Point::new(x as f32, y as f32)) else {
                return;
            };
            crate::ui::menu::popup(
                t.widget.upcast_ref(),
                &menu,
                gtk::gdk::Rectangle::new(pt.x() as i32, pt.y() as i32, 1, 1),
                Rc::new(|_| None),
            );
        });
        view.add_controller(click);
        this.show_summary(0, 0);
        this
    }

    /// Keep the pane's status line updated.
    pub fn set_on_summary(&self, f: impl Fn(&str) + 'static) {
        *self.on_summary.borrow_mut() = Some(Rc::new(f));
    }

    /// (items, bytes) as last listed.
    pub fn summary(&self) -> (usize, u64) {
        self.totals.get()
    }

    fn show_summary(&self, n: usize, bytes: u64) {
        self.totals.set((n, bytes));
        let text = match n {
            0 => "Trash is empty".to_string(),
            1 => format!("1 item, {}", glib::format_size(bytes)),
            n => format!("{n} items, {}", glib::format_size(bytes)),
        };
        self.empty_btn.set_sensitive(n > 0);
        self.stack.set_visible_child_name(if n == 0 { "empty" } else { "list" });
        let cb = self.on_summary.borrow().clone();
        if let Some(cb) = cb {
            cb(&text);
        }
    }

    /// Re-read every trash (off the main thread).
    pub fn reload(&self) {
        let token = self.loading.get() + 1;
        self.loading.set(token);
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let rows = gio::spawn_blocking(|| {
                trash::list_all()
                    .into_iter()
                    .map(|item| {
                        let md = std::fs::symlink_metadata(&item.file).ok();
                        let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
                        let size = if is_dir {
                            crate::core::metadata::dir_stats(&item.file, &AtomicBool::new(false)).bytes
                        } else {
                            md.map(|m| m.len()).unwrap_or(0)
                        };
                        Row { item, is_dir, size }
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            let Some(t) = w.upgrade() else { return };
            if t.loading.get() != token {
                return;
            }
            let total = rows.iter().map(|r| r.size).sum();
            let objs: Vec<glib::BoxedAnyObject> = rows.iter().cloned().map(glib::BoxedAnyObject::new).collect();
            t.store.remove_all();
            t.store.extend_from_slice(&objs);
            t.show_summary(rows.len(), total);
        });
    }

    fn selected(&self) -> Vec<Item> {
        let bits = self.selection.selection();
        (0..bits.size() as u32)
            .filter_map(|n| self.selection.item(bits.nth(n)))
            .filter_map(|o| row_of(&o))
            .map(|r| r.item)
            .collect()
    }

    fn restore_selected(self: &Rc<Self>) {
        let items = self.selected();
        if items.is_empty() {
            return;
        }
        let t = self.clone();
        let n = items.len();
        self.ops.run_job(
            &format!("Restoring {}", if n == 1 { "1 item".into() } else { format!("{n} items") }),
            move |_| {
                let mut fails: Vec<String> = Vec::new();
                for it in &items {
                    if let Err(e) = trash::restore_item(it) {
                        fails.push(format!("{}: {e}", it.name()));
                    }
                }
                Ok::<_, ()>((n - fails.len(), fails))
            },
            move |r| {
                if let Ok((ok, fails)) = r {
                    t.ops.toast(&match (ok, fails.first()) {
                        (_, Some(f)) => format!("Could not restore {f}"),
                        (1, None) => "Restored 1 item".into(),
                        (n, None) => format!("Restored {n} items"),
                    });
                }
                t.reload();
            },
        );
    }

    fn delete_selected(self: &Rc<Self>) {
        let items = self.selected();
        let Some(win) = self.window.upgrade() else { return };
        let Some(first) = items.first() else { return };
        let (t, first_name, n) = (self.clone(), first.name(), items.len());
        glib::spawn_future_local(async move {
            if !dialogs::confirm_delete(&win, n, &first_name).await {
                return;
            }
            let t2 = t.clone();
            t.ops.run_job(
                "Deleting from Trash",
                move |_| {
                    let fails = items.iter().filter(|it| trash::delete_item(it).is_err()).count();
                    Ok::<_, ()>((n - fails, fails))
                },
                move |r| {
                    if let Ok((ok, fails)) = r {
                        t2.ops.toast(&if fails > 0 {
                            format!("Deleted {ok}; {fails} could not be deleted")
                        } else if ok == 1 {
                            "Deleted 1 item".into()
                        } else {
                            format!("Deleted {ok} items")
                        });
                    }
                    t2.reload();
                },
            );
        });
    }

    fn empty(self: &Rc<Self>) {
        let Some(win) = self.window.upgrade() else { return };
        let t = self.clone();
        empty_trash(&win, &self.ops, move || t.reload());
    }
}

/// Ask, then delete everything in every trash. `done` runs afterwards.
pub fn empty_trash(win: &gtk::Window, ops: &OpsUi, done: impl Fn() + 'static) {
    let (win, ops) = (win.clone(), ops.clone());
    glib::spawn_future_local(async move {
        let n = gio::spawn_blocking(|| trash::list_all().len()).await.unwrap_or(0);
        if n == 0 {
            ops.toast("Trash is already empty");
            return;
        }
        let body = if n == 1 {
            "The item in the Trash will be deleted permanently. This cannot be undone.".to_string()
        } else {
            format!("All {n} items in the Trash will be deleted permanently. This cannot be undone.")
        };
        if !dialogs::confirm(&win, "Empty Trash?", &body, "Empty Trash", true).await {
            return;
        }
        let o = ops.clone();
        ops.run_job(
            "Emptying Trash",
            |cancel| Ok::<_, ()>(trash::empty_all(&cancel)),
            move |r| {
                match r {
                    Ok((_, Some(e))) => o.toast(&format!("Some items could not be deleted: {e}")),
                    Ok((n, None)) => {
                        o.toast(&format!("Emptied the Trash ({n} {})", if n == 1 { "item" } else { "items" }))
                    }
                    Err(()) => {}
                }
                done();
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::when;

    #[test]
    fn deletion_time_is_readable() {
        assert_eq!(when("2024-03-01T09:00:00"), "2024-03-01 09:00");
        assert_eq!(when("odd"), "odd");
    }
}
