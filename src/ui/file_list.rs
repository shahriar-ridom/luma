use adw::prelude::*;
use gtk::gio;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use crate::core::filesystem::{human_size, is_dir, natural_cmp};
use crate::ui::interact::{self, ItemCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Type,
    Modified,
}
impl SortKey {
    pub const ALL: [SortKey; 4] = [SortKey::Name, SortKey::Size, SortKey::Type, SortKey::Modified];
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Size => "Size",
            SortKey::Type => "Kind",
            SortKey::Modified => "Date Modified",
        }
    }
}

#[derive(Default)]
struct FilterState {
    hidden: Cell<bool>,
    query: RefCell<String>,
}

/// Shared model (store → filter → sort → selection) plus the detailed list
/// view. The sorter lives on the list view's columns so header clicks and the
/// toolbar sort menu stay in sync; the grid view reuses the same selection.
pub struct FileList {
    /// The list, inside a breakpoint bin so its columns adapt to the pane's
    /// own width (split view halves it).
    pub widget: adw::BreakpointBin,
    pub store: gio::ListStore,
    pub selection: gtk::MultiSelection,
    pub dir: Rc<RefCell<PathBuf>>,
    view: gtk::ColumnView,
    cols: Vec<gtk::ColumnViewColumn>,
    filter: gtk::CustomFilter,
    filter_model: gtk::FilterListModel,
    state: Rc<FilterState>,
}

fn name_cmp(a: &gio::FileInfo, b: &gio::FileInfo) -> gtk::Ordering {
    natural_cmp(&a.display_name(), &b.display_name()).into()
}

/// Column sorter; ties fall back to the name so equal sizes/kinds stay ordered.
fn sorter(f: impl Fn(&gio::FileInfo, &gio::FileInfo) -> gtk::Ordering + 'static) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let a = a.downcast_ref::<gio::FileInfo>().unwrap();
        let b = b.downcast_ref::<gio::FileInfo>().unwrap();
        match f(a, b) {
            gtk::Ordering::Equal => name_cmp(a, b),
            o => o,
        }
    })
}

/// Folders before files. Kept outside the column sorter because the column
/// view inverts its sorter for descending order, which would put folders last.
fn dirs_first() -> gtk::CustomSorter {
    gtk::CustomSorter::new(|a, b| {
        let a = a.downcast_ref::<gio::FileInfo>().unwrap();
        let b = b.downcast_ref::<gio::FileInfo>().unwrap();
        match (is_dir(a), is_dir(b)) {
            (true, false) => gtk::Ordering::Smaller,
            (false, true) => gtk::Ordering::Larger,
            _ => gtk::Ordering::Equal,
        }
    })
}

fn text_col(
    title: &str,
    xalign: f32,
    text: impl Fn(&gio::FileInfo) -> String + 'static,
    sorter: gtk::CustomSorter,
    ctx: &ItemCtx,
) -> gtk::ColumnViewColumn {
    let f = gtk::SignalListItemFactory::new();
    let ctx = ctx.clone();
    f.connect_setup(move |_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let l = gtk::Label::new(None);
        interact::setup_item(item, &l, &ctx);
        l.set_xalign(xalign);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        item.set_child(Some(&l));
    });
    f.connect_bind(move |_, o| {
        let item = o.downcast_ref::<gtk::ListItem>().unwrap();
        let info = item.item().and_downcast::<gio::FileInfo>().unwrap();
        let l = item.child().and_downcast::<gtk::Label>().unwrap();
        l.set_text(&text(&info));
    });
    let c = gtk::ColumnViewColumn::new(Some(title), Some(f));
    c.set_resizable(true);
    c.set_sorter(Some(&sorter));
    c
}

/// The name keeps at least ~220 px; as the pane narrows the other columns
/// step out, least useful first: Kind (the icon already says it), then
/// Size, then Modified. Thresholds are the columns' widths plus the name's
/// minimum, so a column leaves just before it would squeeze the name.
fn responsive_columns(bin: &adw::BreakpointBin, cols: &[gtk::ColumnViewColumn]) {
    let [_, size, kind, modified] = cols else { return };
    let bp = |max: u32, hide: &[&gtk::ColumnViewColumn]| {
        let b =
            adw::Breakpoint::new(adw::BreakpointCondition::parse(&format!("max-width: {max}px")).expect("breakpoint"));
        for c in hide {
            b.add_setter(*c, "visible", Some(&false.to_value()));
        }
        bin.add_breakpoint(b);
    };
    // Name 220 + Size 96 + Kind 190 + Modified 160 + row padding ≈ 700.
    bp(700, &[kind]);
    bp(510, &[kind, size]);
    bp(380, &[kind, size, modified]);
}

impl FileList {
    pub fn new(ctx: &ItemCtx, on_open: impl Fn(&gio::FileInfo) + 'static) -> Self {
        let store = gio::ListStore::new::<gio::FileInfo>();
        let state = Rc::new(FilterState::default());
        let st = state.clone();
        let filter = gtk::CustomFilter::new(move |o| {
            let Some(i) = o.downcast_ref::<gio::FileInfo>() else { return false };
            if !st.hidden.get() && (i.is_hidden() || i.is_backup()) {
                return false;
            }
            let q = st.query.borrow();
            q.is_empty() || i.display_name().to_lowercase().contains(q.as_str())
        });
        let filter_model = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));

        let view = gtk::ColumnView::new(None::<gtk::SelectionModel>);
        view.set_show_row_separators(false);
        view.add_css_class("fm-list");
        view.set_enable_rubberband(true);

        let nf = gtk::SignalListItemFactory::new();
        let (nctx, bctx) = (ctx.clone(), ctx.clone());
        nf.connect_setup(move |_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            interact::setup_item(item, &b, &nctx);
            b.append(&gtk::Image::new());
            let l = gtk::Label::new(None);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            b.append(&l);
            let dots = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            dots.set_visible(false);
            b.append(&dots);
            item.set_child(Some(&b));
        });
        nf.connect_bind(move |_, o| {
            let item = o.downcast_ref::<gtk::ListItem>().unwrap();
            let info = item.item().and_downcast::<gio::FileInfo>().unwrap();
            let b = item.child().and_downcast::<gtk::Box>().unwrap();
            let img = b.first_child().and_downcast::<gtk::Image>().unwrap();
            let lbl = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
            if let Some(icon) = crate::core::filesystem::file_icon(&info) {
                img.set_from_gicon(&icon);
            }
            lbl.set_text(&info.display_name());
            if let Some(dots) = lbl.next_sibling().and_downcast::<gtk::Box>() {
                let t = bctx.tags.borrow().get(info.name().to_string_lossy().as_ref()).cloned().unwrap_or_default();
                let name = info.name().to_string_lossy().into_owned();
                let g = {
                    let m = bctx.git.borrow();
                    m.get(&name).or_else(|| m.get("*")).copied()
                };
                crate::ui::tags::fill_badges(&dots, &t, g);
            }
        });
        let name_col = gtk::ColumnViewColumn::new(Some("Name"), Some(nf));
        name_col.set_expand(true);
        name_col.set_resizable(true);
        name_col.set_sorter(Some(&sorter(name_cmp)));

        let size_col = text_col(
            "Size",
            1.0,
            |i| if is_dir(i) { "—".into() } else { human_size(i.size()) },
            sorter(|a, b| a.size().cmp(&b.size()).into()),
            ctx,
        );
        let type_col = text_col(
            "Kind",
            0.0,
            |i| gio::content_type_get_description(&i.content_type().unwrap_or_default()).to_string(),
            sorter(|a, b| a.content_type().unwrap_or_default().cmp(&b.content_type().unwrap_or_default()).into()),
            ctx,
        );
        let mod_col = text_col(
            "Modified",
            0.0,
            |i| {
                i.modification_date_time()
                    .and_then(|d| d.to_local().ok())
                    .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            },
            sorter(|a, b| a.modification_date_time().cmp(&b.modification_date_time()).into()),
            ctx,
        );
        // Order must match SortKey::ALL.
        size_col.set_fixed_width(96);
        type_col.set_fixed_width(190);
        mod_col.set_fixed_width(160);
        let cols = vec![name_col, size_col, type_col, mod_col];
        for c in &cols {
            view.append_column(c);
        }

        let order = gtk::MultiSorter::new();
        order.append(dirs_first());
        if let Some(s) = view.sorter() {
            order.append(s);
        }
        let sort_model = gtk::SortListModel::new(Some(filter_model.clone()), Some(order));
        let selection = gtk::MultiSelection::new(Some(sort_model));
        let _ = ctx.selection.set(selection.clone());
        view.set_model(Some(&selection));
        view.sort_by_column(Some(&cols[0]), gtk::SortType::Ascending);

        view.connect_activate(move |v, pos| {
            if let Some(info) = v.model().and_then(|m| m.item(pos)).and_downcast::<gio::FileInfo>() {
                on_open(&info);
            }
        });

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&view));
        scroller.set_hexpand(true);
        scroller.set_vexpand(true);
        let widget = adw::BreakpointBin::new();
        widget.set_child(Some(&scroller));
        // A breakpoint bin needs an explicit minimum size.
        widget.set_size_request(200, 120);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        responsive_columns(&widget, &cols);
        Self { widget, store, selection, view, cols, filter, filter_model, state, dir: ctx.dir.clone() }
    }

    pub fn set_show_hidden(&self, show: bool) {
        self.state.hidden.set(show);
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn set_query(&self, q: &str) {
        *self.state.query.borrow_mut() = q.to_lowercase();
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn set_sort(&self, key: SortKey, ascending: bool) {
        let idx = SortKey::ALL.iter().position(|k| *k == key).unwrap();
        let order = if ascending { gtk::SortType::Ascending } else { gtk::SortType::Descending };
        self.view.sort_by_column(Some(&self.cols[idx]), order);
    }

    pub fn position_of(&self, name: &str) -> Option<u32> {
        (0..self.store.n_items()).find(|&i| {
            self.store.item(i).and_downcast::<gio::FileInfo>().is_some_and(|f| f.name().to_string_lossy() == name)
        })
    }

    /// Insert or replace an entry (used by the directory watcher).
    pub fn upsert(&self, info: gio::FileInfo) {
        let name = info.name().to_string_lossy().into_owned();
        match self.position_of(&name) {
            Some(i) => self.store.splice(i, 1, &[info]),
            None => self.store.append(&info),
        }
    }

    pub fn remove_name(&self, name: &str) {
        if let Some(i) = self.position_of(name) {
            self.store.remove(i);
        }
    }

    /// Select the entry called `name` in the sorted view; returns its row.
    pub fn select_name(&self, name: &str) -> Option<u32> {
        let pos = (0..self.selection.n_items()).find(|&i| {
            self.selection.item(i).and_downcast::<gio::FileInfo>().is_some_and(|f| f.name().to_string_lossy() == name)
        })?;
        self.selection.select_item(pos, true);
        self.view.scroll_to(pos, None::<&gtk::ColumnViewColumn>, gtk::ListScrollFlags::FOCUS, None);
        Some(pos)
    }

    pub fn selected_infos(&self) -> Vec<gio::FileInfo> {
        let bits = self.selection.selection();
        (0..bits.size() as u32)
            .filter_map(|n| self.selection.item(bits.nth(n)).and_downcast::<gio::FileInfo>())
            .collect()
    }

    pub fn visible_count(&self) -> u32 {
        self.filter_model.n_items()
    }

    pub fn selected_count(&self) -> u32 {
        self.selection.selection().size() as u32
    }

    /// Sum of sizes of selected non-directory items.
    pub fn selected_bytes(&self) -> i64 {
        let sel = self.selection.selection();
        (0..sel.size() as u32)
            .filter_map(|n| self.selection.item(sel.nth(n)).and_downcast::<gio::FileInfo>())
            .filter(|i| !is_dir(i))
            .map(|i| i.size())
            .sum()
    }
}
