//! Per-item interaction shared by the list and grid views: context menu,
//! drag source (files out) and drop target (files onto folders).

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, OnceCell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use crate::core::filesystem::is_dir;

pub type DropFn = Rc<dyn Fn(Vec<gio::File>, PathBuf, bool)>;

#[derive(Clone)]
pub struct ItemCtx {
    pub selection: Rc<OnceCell<gtk::MultiSelection>>,
    pub dir: Rc<RefCell<PathBuf>>,
    /// Show the item context menu at (x, y) in the widget's coordinates.
    pub on_context: Rc<dyn Fn(&gtk::Widget, f64, f64)>,
    pub on_drop: DropFn,
    /// Set by an item's right-click so the background menu stays closed.
    pub item_claimed: Rc<Cell<bool>>,
    /// File name → tags, for the folder being shown.
    pub tags: Rc<RefCell<std::collections::HashMap<String, Vec<String>>>>,
    /// File name → Git state ("*" = everything in this folder).
    pub git: Rc<RefCell<std::collections::HashMap<String, crate::integrations::git::FileState>>>,
}

fn files_from_bitset(sel: &gtk::MultiSelection, bits: &gtk::Bitset, dir: &std::path::Path) -> Vec<gio::File> {
    (0..bits.size() as u32)
        .filter_map(|n| sel.item(bits.nth(n)).and_downcast::<gio::FileInfo>())
        .map(|i| gio::File::for_path(dir.join(i.name())))
        .collect()
}

pub fn ctrl_held(c: &impl IsA<gtk::EventController>) -> bool {
    c.current_event().is_some_and(|e| e.modifier_state().contains(gdk::ModifierType::CONTROL_MASK))
}

pub fn drop_target(on_drop: DropFn, dest: impl Fn() -> Option<PathBuf> + 'static) -> gtk::DropTarget {
    let dt = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
    dt.connect_drop(move |t, value, _, _| {
        let (Ok(list), Some(dest)) = (value.get::<gdk::FileList>(), dest()) else { return false };
        on_drop(list.files(), dest, ctrl_held(t));
        true
    });
    dt
}

/// Wire up a freshly created item widget. Call from the factory's `setup`.
pub fn setup_item(item: &gtk::ListItem, widget: &impl IsA<gtk::Widget>, ctx: &ItemCtx) {
    let widget = widget.as_ref();

    // Context menu.
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let (wi, c) = (item.downgrade(), ctx.clone());
    click.connect_pressed(move |g, _, x, y| {
        let (Some(item), Some(sel)) = (wi.upgrade(), c.selection.get()) else { return };
        let pos = item.position();
        if !sel.is_selected(pos) {
            sel.select_item(pos, true);
        }
        c.item_claimed.set(true);
        if let Some(w) = g.widget() {
            (c.on_context)(&w, x, y);
        }
    });
    widget.add_controller(click);

    // Drag out. Pressing an unselected item makes GTK select it, and a drag
    // that starts on an item can also start the view's rubberband. Neither
    // should happen when the user is dragging files, so: snapshot the
    // selection at press time, suppress rubberband while the button is down,
    // and restore the snapshot once a drag really begins.
    let snap: Rc<RefCell<Option<gtk::Bitset>>> = Rc::default();
    let rubber = |w: &gtk::Widget, on: bool| {
        for t in [gtk::ColumnView::static_type(), gtk::GridView::static_type()] {
            if let Some(v) = w.ancestor(t) {
                v.set_property("enable-rubberband", on);
            }
        }
    };
    let press = gtk::GestureClick::new();
    press.set_button(1);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (c, sn) = (ctx.clone(), snap.clone());
    let ww = widget.downgrade();
    press.connect_pressed(move |_, _, _, _| {
        if let Some(sel) = c.selection.get() {
            *sn.borrow_mut() = Some(sel.selection());
        }
        if let Some(w) = ww.upgrade() {
            rubber(&w, false);
        }
    });
    let ww = widget.downgrade();
    press.connect_released(move |_, _, _, _| {
        if let Some(w) = ww.upgrade() {
            rubber(&w, true);
        }
    });
    let ww = widget.downgrade();
    press.connect_cancel(move |_, _| {
        if let Some(w) = ww.upgrade() {
            rubber(&w, true);
        }
    });
    widget.add_controller(press);

    let ds = gtk::DragSource::new();
    ds.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let (wi, c, sn) = (item.downgrade(), ctx.clone(), snap.clone());
    ds.connect_prepare(move |_, _, _| {
        let (item, sel) = (wi.upgrade()?, c.selection.get()?);
        let pos = item.position();
        let dir = c.dir.borrow().clone();
        // Drag the whole selection only if the pressed item was already part
        // of it; otherwise drag just that item, leaving the selection alone.
        let files = match sn.borrow().as_ref() {
            Some(bits) if bits.contains(pos) => files_from_bitset(sel, bits, &dir),
            _ => {
                let info = item.item().and_downcast::<gio::FileInfo>()?;
                vec![gio::File::for_path(dir.join(info.name()))]
            }
        };
        (!files.is_empty()).then(|| gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()))
    });
    let (c, sn) = (ctx.clone(), snap.clone());
    ds.connect_drag_begin(move |_, _| {
        if let (Some(sel), Some(bits)) = (c.selection.get(), sn.borrow().as_ref()) {
            sel.set_selection(bits, &gtk::Bitset::new_range(0, sel.n_items()));
        }
    });
    let ww = widget.downgrade();
    ds.connect_drag_end(move |_, _, _| {
        if let Some(w) = ww.upgrade() {
            rubber(&w, true);
        }
    });
    widget.add_controller(ds);

    // Drop onto folders only; everything else bubbles to the view-level target.
    let wi = item.downgrade();
    let c = ctx.clone();
    let dt = drop_target(ctx.on_drop.clone(), move || {
        let info = wi.upgrade()?.item().and_downcast::<gio::FileInfo>()?;
        is_dir(&info).then(|| c.dir.borrow().join(info.name()))
    });
    let wi = item.downgrade();
    dt.connect_accept(move |_, _| {
        wi.upgrade().and_then(|i| i.item()).and_downcast::<gio::FileInfo>().is_some_and(|i| is_dir(&i))
    });
    let w = widget.downgrade();
    dt.connect_current_drop_notify(move |t| {
        if let Some(w) = w.upgrade() {
            if t.current_drop().is_some() { w.add_css_class("drop-hover") } else { w.remove_css_class("drop-hover") }
        }
    });
    widget.add_controller(dt);
}

#[allow(unused)]
fn _use(_: glib::Object) {}
