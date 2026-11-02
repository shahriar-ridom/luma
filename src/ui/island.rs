//! Activity island: a small piece of the sheet floating in the middle of the
//! header bar, always present.
//!
//! - Idle: where you are. At the top of a drive (Home, `/`, a mount) the
//!   ring is the drive's fill level and the text its free space; inside a
//!   folder, the ring is the share of the drive that folder takes and the
//!   text its size. A click opens a card with the details and Disk Usage.
//! - Selection: the selected file's name and size (or the count and total);
//!   the card shows its details and opens Properties.
//! - Busy: a live progress ring for running operations; the card lists each
//!   one with progress, speed, time left and pause/cancel.
//! - Messages (what used to be toasts) show for a moment, then it returns.
//! - Failures stay, with the reason and Retry, until dismissed.
//!
//! Its size and position are animated by `island_layout`.

use adw::prelude::*;
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    f64::consts::PI,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

pub type CancelFn = Rc<dyn Fn()>;
pub type PauseFn = Rc<dyn Fn(bool)>;
pub type RetryFn = Rc<dyn Fn()>;

const RING: i32 = 20;
/// Width of the opened card's content.
const CARD_WIDTH: i32 = 340;
const MESSAGE_MS: u64 = 2600;
const SUMMARY_MS: u64 = 2200;
/// Drive fuller than this shows the ring in the warning colour.
const LOW_SPACE: f64 = 0.9;

/// How an operation ended.
pub enum Outcome {
    /// Succeeded; the text ("Copied 3 items") becomes the closing headline.
    Done(String),
    Cancelled,
    /// Stays in the island until dismissed or retried.
    Failed {
        title: String,
        detail: String,
        retry: Option<RetryFn>,
    },
    /// Ended without a headline of its own (the caller posts a message).
    Quiet,
}

/// How much space the current location takes.
#[derive(Clone, Default, PartialEq)]
pub enum Storage {
    #[default]
    Unknown,
    /// At the top of a drive: (size, free).
    Drive { size: u64, free: u64 },
    /// Inside a folder: its total size (`None` while measuring) and the
    /// drive's size, when known.
    Folder { bytes: Option<u64>, drive_size: Option<u64> },
    /// The Trash: everything in it.
    Trash { bytes: u64 },
}

/// What's selected, for the pill and its card.
#[derive(Clone, Default, PartialEq)]
pub struct Selection {
    pub count: u32,
    /// Name of the item when exactly one is selected.
    pub name: String,
    /// "PNG image", "Folder"; empty for several items.
    pub kind: String,
    /// Total size of the selected files (folders not measured).
    pub bytes: i64,
    pub is_dir: bool,
    /// A single selected folder's size, measured when the card opens.
    pub folder: FolderSize,
    /// "2026-11-02 14:20" for a single item.
    pub modified: String,
}

/// Size of a selected folder: only measured on demand, since walking a big
/// folder takes a while.
#[derive(Clone, Default, PartialEq)]
pub enum FolderSize {
    #[default]
    NotMeasured,
    Measuring,
    Done {
        bytes: u64,
        files: u64,
    },
}

/// What the island shows when nothing else is going on.
#[derive(Clone, Default, PartialEq)]
pub struct Idle {
    /// Folder name as shown to people ("Documents", "Computer").
    pub name: String,
    pub path: String,
    pub items: u32,
    pub storage: Storage,
    pub selection: Option<Selection>,
}

impl Idle {
    /// Fraction of the drive the ring shows; `None` when unknown.
    fn ring(&self) -> Option<f64> {
        match self.storage {
            Storage::Drive { size, free } if size > 0 => Some((1.0 - free as f64 / size as f64).clamp(0.0, 1.0)),
            Storage::Folder { bytes: Some(b), drive_size: Some(d) } if d > 0 => {
                Some((b as f64 / d as f64).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    fn low_space(&self) -> bool {
        matches!(self.storage, Storage::Drive { .. }) && self.ring().is_some_and(|u| u > LOW_SPACE)
    }

    fn measuring(&self) -> bool {
        matches!(self.storage, Storage::Folder { bytes: None, .. })
    }
}

/// "1.2%" with sensible precision for tiny shares.
fn share(f: f64) -> String {
    let p = f * 100.0;
    if p >= 10.0 {
        format!("{p:.0}%")
    } else if p >= 0.1 {
        format!("{p:.1}%")
    } else if p > 0.0 {
        "Less than 0.1%".into()
    } else {
        "0%".into()
    }
}

struct OpView {
    id: u64,
    title: String,
    /// `None` while the total is unknown (indeterminate).
    frac: Cell<Option<f64>>,
    started: Instant,
    failed: Cell<bool>,
    row: gtk::Box,
    name: gtk::Label,
    controls: gtk::Box,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    rate: gtk::Label,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ring {
    Hidden,
    Usage,
    Spin,
    Progress,
    Check,
    Alert,
    Stop,
}

/// The card for the current location.
struct PlaceCard {
    root: gtk::Box,
    action: gtk::Button,
    name: gtk::Label,
    path: gtk::Label,
    bar: gtk::LevelBar,
    space: gtk::Label,
    items: gtk::Label,
}

/// The card for the selection.
struct SelectionCard {
    root: gtk::Box,
    name: gtk::Label,
    kind: gtk::Label,
    info: gtk::Label,
}

pub struct Island {
    /// Overlay child; `island_layout` sizes and places it.
    pub widget: gtk::Box,
    top: gtk::Box,
    ring: gtk::DrawingArea,
    text_box: gtk::Box,
    title: gtk::Label,
    pct: gtk::Label,
    details_reveal: gtk::Revealer,
    details: gtk::Box,
    place_card: PlaceCard,
    sel_card: SelectionCard,
    idle: RefCell<Idle>,
    ops: RefCell<Vec<Rc<OpView>>>,
    next_id: Cell<u64>,
    messages: RefCell<VecDeque<String>>,
    message: RefCell<Option<String>>,
    message_gen: Cell<u64>,
    /// Closing headline once everything finished.
    summary: RefCell<Option<(String, Ring)>>,
    summary_gen: Cell<u64>,
    ring_mode: Cell<Ring>,
    shown: Cell<f64>,
    spin: Cell<f64>,
    last_frame: Cell<i64>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    on_resize: RefCell<Option<Rc<dyn Fn()>>>,
    on_open: RefCell<Option<Rc<dyn Fn()>>>,
    me: Weak<Island>,
}

/// "3.2 MB/s, about 2 min left"
fn rate_text(done: u64, total: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if done == 0 || secs < 1.0 {
        return String::new();
    }
    let speed = done as f64 / secs;
    let left = total.saturating_sub(done) as f64 / speed;
    let left = match left as u64 {
        0..=4 => "a few seconds left".to_string(),
        s @ 5..=59 => format!("{s} s left"),
        s @ 60..=5399 => format!("about {} min left", (s + 30) / 60),
        s => format!("about {} h left", (s + 1800) / 3600),
    };
    format!("{}/s, {left}", glib::format_size(speed as u64))
}

fn operations(n: usize) -> String {
    if n == 1 { "1 operation".into() } else { format!("{n} operations") }
}

fn size(b: u64) -> String {
    glib::format_size(b).to_string()
}

fn icon_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.set_valign(gtk::Align::Center);
    b
}

fn meta_label() -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.add_css_class("fm-island-meta");
    l
}

/// A details line that wraps within the card instead of stretching it.
fn wrap_label() -> gtk::Label {
    let l = meta_label();
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l.set_width_chars(1);
    l.set_max_width_chars(30);
    l
}

fn action_button(label: &str, action: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("fm-island-action");
    b.set_action_name(Some(action));
    b.set_valign(gtk::Align::Center);
    b
}

fn heading_label() -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.add_css_class("fm-island-op-title");
    l
}

fn place_card() -> PlaceCard {
    let name = heading_label();
    let path = meta_label();
    path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let bar = gtk::LevelBar::new();
    bar.set_min_value(0.0);
    bar.set_max_value(1.0);
    bar.add_css_class("fm-island-usage");
    // Our own colours, not the default low/high/full offsets.
    for o in ["low", "high", "full"] {
        bar.remove_offset_value(Some(o));
    }
    let space = wrap_label();
    let items = wrap_label();
    let left = gtk::Box::new(gtk::Orientation::Vertical, 2);
    left.set_hexpand(true);
    left.append(&space);
    left.append(&items);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    foot.append(&left);
    let action = action_button("Disk Usage…", "win.storage");
    foot.append(&action);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&name);
    root.append(&path);
    root.append(&bar);
    root.append(&foot);
    PlaceCard { root, action, name, path, bar, space, items }
}

fn selection_card() -> SelectionCard {
    let name = heading_label();
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let kind = wrap_label();
    let info = wrap_label();
    let left = gtk::Box::new(gtk::Orientation::Vertical, 2);
    left.set_hexpand(true);
    left.append(&kind);
    left.append(&info);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    foot.append(&left);
    foot.append(&action_button("Properties", "win.properties"));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.append(&name);
    root.append(&foot);
    SelectionCard { root, name, kind, info }
}

impl Island {
    pub fn new() -> Rc<Self> {
        let ring = gtk::DrawingArea::new();
        ring.set_content_width(RING);
        ring.set_content_height(RING);
        ring.set_valign(gtk::Align::Center);
        ring.add_css_class("fm-island-ring");

        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(34);
        title.add_css_class("fm-island-title");
        let pct = gtk::Label::new(None);
        pct.add_css_class("fm-island-pct");
        let text_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        text_box.add_css_class("fm-island-text");
        text_box.append(&title);
        text_box.append(&pct);

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.add_css_class("fm-island-top");
        top.append(&ring);
        top.append(&text_box);
        top.set_halign(gtk::Align::Center);

        let place = place_card();
        let sel = selection_card();
        let details = gtk::Box::new(gtk::Orientation::Vertical, 14);
        details.add_css_class("fm-island-details");
        details.set_width_request(CARD_WIDTH);
        details.append(&place.root);
        details.append(&sel.root);
        sel.root.set_visible(false);
        let details_reveal = gtk::Revealer::new();
        details_reveal.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        details_reveal.set_transition_duration(240);
        // The card can be laid out narrower than its content while the pill
        // morphs into it (clipped, not squeezed), so the width can glide.
        let clip = gtk::ScrolledWindow::new();
        clip.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        clip.set_propagate_natural_width(true);
        clip.set_propagate_natural_height(true);
        clip.set_child(Some(&details));
        details_reveal.set_child(Some(&clip));
        // A slide-down revealer still asks for its child's full width when
        // closed, which would keep the pill wide. Take it out of layout then.
        details_reveal.set_visible(false);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("fm-island");
        widget.set_halign(gtk::Align::Start);
        widget.set_valign(gtk::Align::Start);
        widget.set_overflow(gtk::Overflow::Hidden);
        widget.append(&top);
        widget.append(&details_reveal);

        let this = Rc::new_cyclic(|me| Self {
            widget,
            top: top.clone(),
            ring,
            text_box,
            title,
            pct,
            details_reveal,
            details,
            place_card: place,
            sel_card: sel,
            idle: RefCell::default(),
            ops: RefCell::default(),
            next_id: Cell::new(1),
            messages: RefCell::default(),
            message: RefCell::default(),
            message_gen: Cell::new(0),
            summary: RefCell::default(),
            summary_gen: Cell::new(0),
            ring_mode: Cell::new(Ring::Hidden),
            shown: Cell::new(0.0),
            spin: Cell::new(0.0),
            last_frame: Cell::new(0),
            tick: RefCell::default(),
            on_resize: RefCell::default(),
            on_open: RefCell::default(),
            me: me.clone(),
        });

        let w = Rc::downgrade(&this);
        this.details_reveal.connect_child_revealed_notify(move |r| {
            if !r.is_child_revealed() && !r.reveals_child() {
                r.set_visible(false);
                if let Some(i) = w.upgrade() {
                    i.resized();
                }
            }
        });
        let w = Rc::downgrade(&this);
        this.ring.set_draw_func(move |area, cr, wd, ht| {
            if let Some(i) = w.upgrade() {
                i.draw_ring(area, cr, wd as f64, ht as f64);
            }
        });
        let click = gtk::GestureClick::new();
        let w = Rc::downgrade(&this);
        click.connect_released(move |_, _, _, _| {
            if let Some(i) = w.upgrade() {
                i.set_expanded(!i.is_expanded());
            }
        });
        top.add_controller(click);
        top.set_cursor_from_name(Some("pointer"));
        this.refresh();
        this
    }

    /// Close the card when the user clicks anywhere else in `win`.
    pub fn attach(&self, win: &gtk::Window) {
        let g = gtk::GestureClick::new();
        g.set_button(0);
        g.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (w, wn) = (self.me.clone(), win.downgrade());
        g.connect_pressed(move |_, _, x, y| {
            let (Some(i), Some(win)) = (w.upgrade(), wn.upgrade()) else { return };
            if !i.is_expanded() {
                return;
            }
            let hit = win.pick(x, y, gtk::PickFlags::DEFAULT);
            if !hit.is_some_and(|h| h == i.widget || h.is_ancestor(&i.widget)) {
                i.set_expanded(false);
            }
        });
        win.add_controller(g);
    }

    /// Called whenever the island's natural size may have changed.
    pub fn set_on_resize(&self, f: impl Fn() + 'static) {
        *self.on_resize.borrow_mut() = Some(Rc::new(f));
    }

    /// The text part, so very narrow windows can leave just the ring.
    pub fn text(&self) -> &gtk::Box {
        &self.text_box
    }

    pub fn is_expanded(&self) -> bool {
        self.details_reveal.reveals_child()
    }

    /// Height of the compact pill: the top row plus padding and border
    /// (matches the CSS), for centring it in the header.
    pub fn pill_height(&self) -> i32 {
        self.top.measure(gtk::Orientation::Vertical, -1).1 + 2 * 5 + 2
    }

    /// Width the island wants: the pill, or the card when opened.
    pub fn natural_width(&self) -> i32 {
        self.widget.measure(gtk::Orientation::Horizontal, -1).1
    }

    fn resized(&self) {
        let cb = self.on_resize.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    fn has_rows(&self) -> bool {
        !self.ops.borrow().is_empty()
    }

    /// Called when the card opens (e.g. to measure a selected folder).
    pub fn set_on_open(&self, f: impl Fn() + 'static) {
        *self.on_open.borrow_mut() = Some(Rc::new(f));
    }

    fn set_expanded(&self, on: bool) {
        if on == self.is_expanded() {
            return;
        }
        if on {
            self.details_reveal.set_visible(true);
            self.widget.add_css_class("expanded");
        } else {
            self.widget.remove_css_class("expanded");
        }
        self.details_reveal.set_reveal_child(on);
        self.refresh();
        self.resized();
        if on {
            let cb = self.on_open.borrow().clone();
            if let Some(cb) = cb {
                cb();
            }
        }
    }

    // ---- public API ------------------------------------------------------------

    /// Update the idle view (current folder, its storage, the selection).
    pub fn set_idle(&self, idle: Idle) {
        if *self.idle.borrow() == idle {
            return;
        }
        let c = &self.place_card;
        c.name.set_text(&idle.name);
        c.path.set_text(&idle.path);
        c.path.set_tooltip_text(Some(&idle.path));
        let ring = idle.ring();
        c.bar.set_visible(ring.is_some());
        c.bar.set_value(ring.unwrap_or(0.0));
        if idle.low_space() {
            c.bar.add_css_class("low")
        } else {
            c.bar.remove_css_class("low")
        }
        c.space.set_text(&match idle.storage {
            Storage::Drive { size: s, free } => format!("{} free of {}", size(free), size(s)),
            Storage::Folder { bytes: None, .. } => "Measuring this folder…".into(),
            Storage::Folder { bytes: Some(b), drive_size: Some(d) } if d > 0 => {
                format!("{} in this folder\n{} of the {} drive", size(b), share(b as f64 / d as f64), size(d))
            }
            Storage::Folder { bytes: Some(b), .. } => format!("{} in this folder", size(b)),
            Storage::Trash { bytes } => format!("{} in the Trash", size(bytes)),
            Storage::Unknown => "Size unknown".into(),
        });
        // In the Trash the useful action is emptying it.
        let (label, action) = if matches!(idle.storage, Storage::Trash { .. }) {
            ("Empty Trash…", "win.empty-trash")
        } else {
            ("Disk Usage…", "win.storage")
        };
        c.action.set_label(label);
        c.action.set_action_name(Some(action));
        // Nothing to empty in an empty Trash.
        c.action.set_sensitive(!matches!(idle.storage, Storage::Trash { .. }) || idle.items > 0);
        c.items.set_text(&format!("{} {}", idle.items, if idle.items == 1 { "item" } else { "items" }));

        if let Some(sel) = &idle.selection {
            let k = &self.sel_card;
            if sel.count == 1 {
                k.name.set_text(&sel.name);
                k.kind.set_text(&sel.kind);
                let size_txt = match (&sel.folder, sel.is_dir) {
                    (_, false) => size(sel.bytes.max(0) as u64),
                    (FolderSize::Done { bytes, files }, true) => {
                        format!("{} in {} {}", size(*bytes), files, if *files == 1 { "file" } else { "files" })
                    }
                    (FolderSize::Measuring, true) => "Measuring…".into(),
                    (FolderSize::NotMeasured, true) => String::new(),
                };
                k.info.set_text(&match (size_txt.is_empty(), sel.modified.is_empty()) {
                    (false, false) => format!("{size_txt}, modified {}", sel.modified),
                    (false, true) => size_txt,
                    (true, false) => format!("Modified {}", sel.modified),
                    (true, true) => String::new(),
                });
            } else {
                k.name.set_text(&format!("{} items selected", sel.count));
                k.kind.set_text(&format!("of {} in {}", idle.items, idle.name));
                k.info.set_text(&if sel.bytes > 0 {
                    format!("{} in files", size(sel.bytes as u64))
                } else {
                    "Folders only".into()
                });
            }
        }
        *self.idle.borrow_mut() = idle;
        self.refresh();
        // The card's content changed size: let the layout follow.
        if self.is_expanded() {
            self.resized();
        }
    }

    /// A short message (what a toast used to say). Messages queue up and
    /// each shows for a moment.
    pub fn message(&self, text: &str) {
        let showing = self.message.borrow().clone();
        if let Some(cur) = showing {
            let mut q = self.messages.borrow_mut();
            if q.back().map(String::as_str) != Some(text) && cur != text {
                q.push_back(text.to_string());
                while q.len() > 3 {
                    q.pop_front();
                }
            }
            return;
        }
        self.show_message(text.to_string());
    }

    fn show_message(&self, text: String) {
        *self.message.borrow_mut() = Some(text);
        let g = self.message_gen.get() + 1;
        self.message_gen.set(g);
        self.refresh();
        let w = self.me.clone();
        glib::timeout_add_local_once(Duration::from_millis(MESSAGE_MS), move || {
            let Some(i) = w.upgrade() else { return };
            if i.message_gen.get() != g {
                return;
            }
            let next = i.messages.borrow_mut().pop_front();
            match next {
                Some(n) => i.show_message(n),
                None => {
                    *i.message.borrow_mut() = None;
                    i.refresh();
                }
            }
        });
    }

    /// Register a running operation; returns its id for later updates.
    pub fn add(&self, title: &str, cancel: CancelFn, pause: Option<PauseFn>) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        *self.summary.borrow_mut() = None;

        let name = gtk::Label::new(Some(title));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.add_css_class("fm-island-op-title");
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        if let Some(pause) = pause {
            let b = gtk::ToggleButton::new();
            b.set_icon_name("luma-pause-symbolic");
            b.set_tooltip_text(Some("Pause"));
            b.add_css_class("flat");
            b.add_css_class("circular");
            b.set_valign(gtk::Align::Center);
            b.connect_toggled(move |b| {
                pause(b.is_active());
                b.set_tooltip_text(Some(if b.is_active() { "Resume" } else { "Pause" }));
            });
            controls.append(&b);
        }
        let stop = icon_button("luma-close-symbolic", "Cancel");
        stop.connect_clicked(move |b| {
            b.set_sensitive(false);
            cancel();
        });
        controls.append(&stop);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        head.append(&name);
        head.append(&controls);
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("fm-island-bar");
        let status = meta_label();
        status.set_text("Preparing…");
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let rate = meta_label();
        rate.set_visible(false);
        let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
        row.add_css_class("fm-island-op");
        row.append(&head);
        row.append(&bar);
        row.append(&status);
        row.append(&rate);
        self.details.append(&row);

        self.ops.borrow_mut().push(Rc::new(OpView {
            id,
            title: title.to_string(),
            frac: Cell::new(None),
            started: Instant::now(),
            failed: Cell::new(false),
            row,
            name,
            controls,
            bar,
            status,
            rate,
        }));
        self.refresh();
        id
    }

    /// Live progress. `frac` is `None` while the size is unknown; `bytes`
    /// (done, total) enables the speed and time-left line.
    pub fn progress(&self, id: u64, frac: Option<f64>, status: &str, bytes: Option<(u64, u64)>) {
        let Some(op) = self.find(id) else { return };
        op.frac.set(frac);
        match frac {
            Some(f) => op.bar.set_fraction(f),
            None => op.bar.pulse(),
        }
        if !status.is_empty() {
            op.status.set_text(status);
        }
        let rate = bytes.map(|(d, t)| rate_text(d, t, op.started.elapsed())).unwrap_or_default();
        op.rate.set_visible(!rate.is_empty());
        op.rate.set_text(&rate);
        self.refresh();
    }

    pub fn finish(&self, id: u64, outcome: Outcome) {
        let Some(op) = self.find(id) else { return };
        let last = !self.ops.borrow().iter().any(|o| o.id != id && !o.failed.get());
        match outcome {
            Outcome::Failed { title, detail, retry } => {
                self.fail_row(&op, &title, &detail, retry);
                self.refresh();
                // Failures need attention: open the card to show what happened.
                self.set_expanded(true);
                return;
            }
            Outcome::Done(text) if last => self.set_summary(text, Ring::Check),
            Outcome::Cancelled if last => self.set_summary("Cancelled".into(), Ring::Stop),
            _ => {}
        }
        self.remove_row(id);
        self.refresh();
    }

    // ---- internals -------------------------------------------------------------

    fn find(&self, id: u64) -> Option<Rc<OpView>> {
        self.ops.borrow().iter().find(|o| o.id == id).cloned()
    }

    fn remove_row(&self, id: u64) {
        let gone: Vec<Rc<OpView>> = {
            let mut ops = self.ops.borrow_mut();
            let (gone, keep): (Vec<_>, Vec<_>) = ops.drain(..).partition(|o| o.id == id);
            *ops = keep;
            gone
        };
        for o in gone {
            self.details.remove(&o.row);
        }
    }

    fn fail_row(&self, op: &Rc<OpView>, title: &str, detail: &str, retry: Option<RetryFn>) {
        op.failed.set(true);
        op.row.add_css_class("failed");
        op.name.set_text(title);
        op.bar.set_visible(false);
        op.rate.set_visible(false);
        op.status.set_text(detail);
        op.status.set_ellipsize(gtk::pango::EllipsizeMode::None);
        op.status.set_wrap(true);
        op.status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        op.status.set_max_width_chars(40);
        while let Some(c) = op.controls.first_child() {
            op.controls.remove(&c);
        }
        let id = op.id;
        if let Some(retry) = retry {
            let b = gtk::Button::with_label("Retry");
            b.add_css_class("fm-island-action");
            b.set_valign(gtk::Align::Center);
            let w = self.me.clone();
            b.connect_clicked(move |_| {
                if let Some(i) = w.upgrade() {
                    i.remove_row(id);
                    i.refresh();
                }
                retry();
            });
            op.controls.append(&b);
        }
        let dismiss = icon_button("luma-close-symbolic", "Dismiss");
        let w = self.me.clone();
        dismiss.connect_clicked(move |_| {
            if let Some(i) = w.upgrade() {
                i.remove_row(id);
                i.refresh();
            }
        });
        op.controls.append(&dismiss);
    }

    fn set_summary(&self, text: String, ring: Ring) {
        let text = if text.is_empty() { "Done".to_string() } else { text };
        *self.summary.borrow_mut() = Some((text, ring));
        let g = self.summary_gen.get() + 1;
        self.summary_gen.set(g);
        let w = self.me.clone();
        glib::timeout_add_local_once(Duration::from_millis(SUMMARY_MS), move || {
            let Some(i) = w.upgrade() else { return };
            if i.summary_gen.get() == g {
                *i.summary.borrow_mut() = None;
                i.refresh();
            }
        });
    }

    fn running(&self) -> Vec<Rc<OpView>> {
        self.ops.borrow().iter().filter(|o| !o.failed.get()).cloned().collect()
    }

    fn failed(&self) -> Vec<Rc<OpView>> {
        self.ops.borrow().iter().filter(|o| o.failed.get()).cloned().collect()
    }

    /// Overall fraction of what's running; `None` = indeterminate.
    fn target(&self) -> Option<f64> {
        let fr: Vec<f64> = self.running().iter().filter_map(|o| o.frac.get()).collect();
        (!fr.is_empty()).then(|| fr.iter().sum::<f64>() / fr.len() as f64)
    }

    fn progress_ring(&self) -> Ring {
        if self.target().is_some() { Ring::Progress } else { Ring::Spin }
    }

    /// Where the ring is heading (0..1).
    fn ring_goal(&self) -> f64 {
        match self.ring_mode.get() {
            Ring::Progress => self.target().unwrap_or(0.0),
            Ring::Usage => self.idle.borrow().ring().unwrap_or(0.0),
            Ring::Spin | Ring::Hidden => self.shown.get(),
            _ => 1.0,
        }
    }

    fn refresh(&self) {
        let running = self.running();
        let failed = self.failed();
        let message = self.message.borrow().clone();
        let summary = self.summary.borrow().clone();
        let expanded = self.is_expanded();
        let (title, pct, ring, state) = if let Some(m) = message {
            let ring = if running.is_empty() { Ring::Hidden } else { self.progress_ring() };
            (m, String::new(), ring, "message")
        } else if !running.is_empty() {
            // Opened, the rows carry the names: the pill sums up instead.
            let title = match (running.as_slice(), expanded) {
                ([one], false) => one.title.clone(),
                (_, true) => format!("{} running", operations(running.len())),
                (many, false) => operations(many.len()),
            };
            let pct = self.target().map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_default();
            (title, pct, self.progress_ring(), "running")
        } else if !failed.is_empty() {
            let title = match (failed.as_slice(), expanded) {
                ([one], false) => one.name.text().to_string(),
                (many, _) => format!("{} failed", operations(many.len())),
            };
            (title, String::new(), Ring::Alert, "error")
        } else if let Some((t, r)) = summary {
            (t, String::new(), r, if r == Ring::Check { "done" } else { "stopped" })
        } else {
            let idle = self.idle.borrow();
            let (title, detail, ring) = match &idle.selection {
                Some(sel) if sel.count == 1 => {
                    let d = match (&sel.folder, sel.is_dir) {
                        (_, false) => size(sel.bytes.max(0) as u64),
                        (FolderSize::Done { bytes, .. }, true) => size(*bytes),
                        _ => sel.kind.clone(),
                    };
                    (sel.name.clone(), d, Ring::Hidden)
                }
                Some(sel) => {
                    let d = if sel.bytes > 0 { size(sel.bytes as u64) } else { String::new() };
                    (format!("{} selected", sel.count), d, Ring::Hidden)
                }
                None => {
                    let detail = match idle.storage {
                        Storage::Drive { free, .. } => format!("{} free", size(free)),
                        Storage::Folder { bytes: Some(b), .. } | Storage::Trash { bytes: b } => size(b),
                        _ => String::new(),
                    };
                    let ring = if idle.measuring() {
                        Ring::Spin
                    } else if idle.ring().is_some() {
                        Ring::Usage
                    } else {
                        Ring::Hidden
                    };
                    (idle.name.clone(), detail, ring)
                }
            };
            if idle.low_space() && idle.selection.is_none() {
                self.widget.add_css_class("low-space");
            } else {
                self.widget.remove_css_class("low-space");
            }
            (title, detail, ring, "idle")
        };
        for c in ["idle", "message", "running", "error", "done", "stopped"] {
            if c == state { self.widget.add_css_class(c) } else { self.widget.remove_css_class(c) }
        }
        // Operation rows take the card over; otherwise it describes the
        // selection, or the location when nothing is selected.
        let has_sel = self.idle.borrow().selection.is_some();
        self.place_card.root.set_visible(!self.has_rows() && !has_sel);
        self.sel_card.root.set_visible(!self.has_rows() && has_sel);

        let text_changed = self.title.text() != title;
        let changed = text_changed || self.pct.text() != pct || self.ring.is_visible() != (ring != Ring::Hidden);
        if text_changed {
            // Cross-fade the new words in while the pill's width glides.
            self.text_box.remove_css_class("fm-island-swap");
            let tb = self.text_box.clone();
            glib::idle_add_local_once(move || tb.add_css_class("fm-island-swap"));
        }
        self.title.set_text(&title);
        self.pct.set_text(&pct);
        self.pct.set_visible(!pct.is_empty());
        self.ring.set_visible(ring != Ring::Hidden);
        let mode_changed = self.ring_mode.replace(ring) != ring;
        let tip = if self.has_rows() {
            "Show details"
        } else if has_sel {
            "Show selection details"
        } else {
            "Show location details"
        };
        self.widget.set_tooltip_text((!expanded).then_some(tip));
        if changed {
            self.resized();
        }
        if mode_changed || (self.ring_goal() - self.shown.get()).abs() > 0.001 || ring == Ring::Spin {
            self.start_ticking();
        }
        self.ring.queue_draw();
    }

    /// Animate the ring only while it has somewhere to go.
    fn start_ticking(&self) {
        if self.tick.borrow().is_some() {
            return;
        }
        self.last_frame.set(0);
        let w = self.me.clone();
        let id = self.ring.add_tick_callback(move |ring, clock| {
            let Some(i) = w.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let dt = match i.last_frame.replace(now) {
                0 => 0.016,
                prev => ((now - prev) as f64 / 1_000_000.0).min(0.1),
            };
            let goal = i.ring_goal();
            let s = i.shown.get();
            let next = s + (goal - s) * (1.0 - (-dt * 8.0).exp());
            i.shown.set(next);
            i.spin.set((i.spin.get() + dt * 2.0 * PI * 0.85) % (2.0 * PI));
            ring.queue_draw();
            if i.ring_mode.get() != Ring::Spin && (goal - next).abs() < 0.0005 {
                i.shown.set(goal);
                i.tick.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(id);
    }

    fn draw_ring(&self, area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: f64, h: f64) {
        let fg = area.color();
        let (cx, cy) = (w / 2.0, h / 2.0);
        let lw = 2.5;
        let r = (w.min(h) - lw) / 2.0;
        cr.set_line_width(lw);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        let paint =
            |a: f64| cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, fg.alpha() as f64 * a);
        let start = -PI / 2.0;
        let mode = self.ring_mode.get();

        // Track: faint while measuring something, solid once there's an outcome.
        paint(if matches!(mode, Ring::Progress | Ring::Spin | Ring::Usage) { 0.2 } else { 1.0 });
        cr.arc(cx, cy, r, 0.0, 2.0 * PI);
        let _ = cr.stroke();
        paint(1.0);
        cr.set_line_width(2.2);
        match mode {
            Ring::Hidden => {}
            Ring::Spin => {
                cr.set_line_width(lw);
                let a = self.spin.get();
                cr.arc(cx, cy, r, start + a, start + a + PI * 0.6);
                let _ = cr.stroke();
            }
            Ring::Progress | Ring::Usage => {
                cr.set_line_width(lw);
                let mut f = self.shown.get().clamp(0.0, 1.0);
                // A folder that takes a sliver of the drive still shows a sliver.
                if mode == Ring::Usage && self.ring_goal() > 0.0 {
                    f = f.max(0.025);
                }
                if f > 0.002 {
                    cr.arc(cx, cy, r, start, start + f * 2.0 * PI);
                    let _ = cr.stroke();
                }
            }
            Ring::Check => {
                cr.move_to(cx - r * 0.42, cy + r * 0.02);
                cr.line_to(cx - r * 0.1, cy + r * 0.34);
                cr.line_to(cx + r * 0.44, cy - r * 0.3);
                let _ = cr.stroke();
            }
            Ring::Alert => {
                cr.move_to(cx, cy - r * 0.42);
                cr.line_to(cx, cy + r * 0.08);
                cr.move_to(cx, cy + r * 0.4);
                cr.line_to(cx, cy + r * 0.41);
                let _ = cr.stroke();
            }
            Ring::Stop => {
                cr.move_to(cx - r * 0.35, cy);
                cr.line_to(cx + r * 0.35, cy);
                let _ = cr.stroke();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Idle, Storage, rate_text, share};
    use std::time::Duration;

    #[test]
    fn speed_and_time_left() {
        assert_eq!(rate_text(0, 100, Duration::from_secs(5)), "");
        assert_eq!(rate_text(10, 100, Duration::from_millis(200)), "");
        let t = rate_text(50_000_000, 100_000_000, Duration::from_secs(10));
        assert!(t.contains("MB/s, ") && t.ends_with("10 s left"), "{t}");
        let t = rate_text(1_000_000, 1_000_000_000, Duration::from_secs(1));
        assert!(t.ends_with("about 17 min left"), "{t}");
    }

    #[test]
    fn ring_fraction() {
        let mut i = Idle { storage: Storage::Drive { size: 1000, free: 250 }, ..Default::default() };
        assert_eq!(i.ring(), Some(0.75));
        i.storage = Storage::Folder { bytes: Some(50), drive_size: Some(1000) };
        assert_eq!(i.ring(), Some(0.05));
        i.storage = Storage::Folder { bytes: None, drive_size: Some(1000) };
        assert_eq!(i.ring(), None);
        assert!(i.measuring());
        i.storage = Storage::Drive { size: 0, free: 0 };
        assert_eq!(i.ring(), None);
    }

    #[test]
    fn share_text() {
        assert_eq!(share(0.5), "50%");
        assert_eq!(share(0.012), "1.2%");
        assert_eq!(share(0.000_01), "Less than 0.1%");
        assert_eq!(share(0.0), "0%");
    }
}
