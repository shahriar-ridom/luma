//! Disk Manager, Health page: drive health (SMART) and filesystem checks.
//!
//! Reading a drive's SMART data and checking a filesystem both need
//! administrator rights, so every action asks first and shows the exact
//! command. Nothing here repairs anything: the checks only read.

use adw::prelude::*;
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::core::diskmgr::{
    ToolSet,
    devices::{Disk, Volume},
    fscheck::{self, CheckResult, Verdict},
    privileged::{self, RootAction},
    smart::{Health, SmartReport},
};
use crate::core::filesystem::human_size;
use crate::services::diskmgr;
use crate::ui::dialogs;

pub struct HealthPage {
    pub widget: gtk::Box,
    dialog: adw::Dialog,
    parent: gtk::Window,
    list: gtk::ListBox,
    empty: gtk::Label,
    spinner: gtk::Spinner,
    status: gtk::Label,
    refresh: gtk::Button,
    tools: ToolSet,
    loaded: Cell<bool>,
    busy: Cell<bool>,
    /// A device (disk or volume) to open when the list arrives.
    select: RefCell<Option<String>>,
    rows: RefCell<Vec<(Disk, adw::ExpanderRow)>>,
}

fn size(b: u64) -> String {
    human_size(b.min(i64::MAX as u64) as i64)
}

fn disk_subtitle(d: &Disk) -> String {
    let kind = match (d.transport.as_deref(), d.rotational) {
        (Some("nvme"), _) => "NVMe",
        (_, true) => "Hard disk",
        _ => "SSD",
    };
    let mut parts = vec![d.path.clone(), size(d.size), kind.to_string()];
    if d.removable {
        parts.push("Removable".into());
    }
    parts.join(" · ")
}

fn badge(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class(class);
    l.add_css_class("heading");
    l.set_valign(gtk::Align::Center);
    l
}

fn info_row(title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_use_markup(false);
    r.set_title(title);
    r.set_subtitle(value);
    r.add_css_class("property");
    r
}

fn hours(h: u64) -> String {
    match h {
        0..=47 => format!("{h} hours"),
        _ => format!("{h} hours (about {} days)", h / 24),
    }
}

/// The detail rows for a SMART report, in the order people look for them.
fn report_rows(r: &SmartReport) -> Vec<adw::ActionRow> {
    let mut rows = vec![];
    let mut add = |t: &str, v: String| rows.push(info_row(t, &v));
    if let Some(t) = r.temp_c {
        add("Temperature", format!("{t} °C"));
    }
    if let Some(h) = r.power_on_hours {
        add("Powered on", hours(h));
    }
    if let Some(a) = &r.ata {
        for (t, v) in [
            ("Reallocated sectors", a.reallocated),
            ("Pending sectors", a.pending),
            ("Uncorrectable sectors", a.uncorrectable),
        ] {
            if let Some(v) = v {
                add(t, v.to_string());
            }
        }
    }
    if let Some(n) = &r.nvme {
        if let Some(u) = n.percentage_used {
            add("Lifetime used", format!("{u}%"));
        }
        if let Some(s) = n.available_spare {
            add("Spare capacity", format!("{s}%"));
        }
        if let Some(m) = n.media_errors {
            add("Media errors", m.to_string());
        }
        if let Some(u) = n.unsafe_shutdowns {
            add("Unsafe shutdowns", u.to_string());
        }
    }
    if let Some(t) = &r.last_test {
        let when = t.hours.map(|h| format!(" at {} hours", h)).unwrap_or_default();
        add("Last self-test", format!("{}: {}{when}", t.kind, t.result));
    }
    if let Some(p) = r.test_remaining {
        add("Self-test running", format!("{p}% left"));
    }
    rows
}

impl HealthPage {
    pub fn new(parent: &gtk::Window, dialog: &adw::Dialog) -> Rc<Self> {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_visible(false);
        let empty = gtk::Label::new(None);
        empty.add_css_class("dim-label");
        empty.set_wrap(true);
        empty.set_margin_top(24);
        empty.set_margin_bottom(24);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for w in [list.upcast_ref::<gtk::Widget>(), empty.upcast_ref()] {
            w.set_margin_top(12);
            w.set_margin_start(12);
            w.set_margin_end(12);
        }
        content.append(&list);
        content.append(&empty);
        let sw = gtk::ScrolledWindow::new();
        sw.set_vexpand(true);
        sw.set_child(Some(&content));

        let spinner = gtk::Spinner::new();
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.add_css_class("dim-label");
        let refresh = gtk::Button::with_label("Refresh");
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for side in [gtk::Widget::set_margin_start, gtk::Widget::set_margin_end] {
            side(foot.upcast_ref(), 12);
        }
        foot.set_margin_top(8);
        foot.set_margin_bottom(8);
        foot.append(&spinner);
        foot.append(&status);
        foot.append(&refresh);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&sw);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&foot);

        let page = Rc::new(Self {
            widget,
            dialog: dialog.clone(),
            parent: parent.clone(),
            list,
            empty,
            spinner,
            status,
            refresh,
            tools: ToolSet::detect(),
            loaded: Cell::new(false),
            busy: Cell::new(false),
            select: RefCell::default(),
            rows: RefCell::default(),
        });
        let w = Rc::downgrade(&page);
        page.refresh.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.load();
            }
        });
        page
    }

    /// Load the drives the first time the page is shown.
    pub fn ensure_loaded(self: &Rc<Self>) {
        if !self.loaded.get() {
            self.load();
        }
    }

    /// Open the disk that is, or holds, `dev`.
    pub fn select(self: &Rc<Self>, dev: &str) {
        *self.select.borrow_mut() = Some(dev.to_string());
        if self.loaded.get() && !self.busy.get() {
            self.apply_selection();
        }
    }

    fn set_busy(&self, busy: bool, text: &str) {
        self.busy.set(busy);
        self.spinner.set_spinning(busy);
        self.status.set_text(text);
        self.refresh.set_sensitive(!busy);
        self.list.set_sensitive(!busy);
    }

    fn apply_selection(&self) {
        let Some(dev) = self.select.borrow_mut().take() else { return };
        for (d, row) in self.rows.borrow().iter() {
            let hit = d.path == dev || d.volumes.iter().any(|v| v.path == dev);
            row.set_expanded(hit);
        }
    }

    fn load(self: &Rc<Self>) {
        if !self.tools.has("lsblk") {
            self.show_empty("Luma needs the lsblk tool (util-linux) to list your drives.");
            self.loaded.set(true);
            return;
        }
        self.set_busy(true, "Looking for drives…");
        let rx = diskmgr::spawn_disks();
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(res) = rx.recv().await else { return };
            let Some(p) = w.upgrade() else { return };
            p.loaded.set(true);
            p.set_busy(false, "");
            match res {
                Ok(disks) if disks.is_empty() => p.show_empty("No drives were found."),
                Ok(disks) => p.show_disks(disks),
                Err(e) => p.show_empty(&e),
            }
        });
    }

    fn show_empty(&self, text: &str) {
        self.list.set_visible(false);
        self.empty.set_text(text);
        self.empty.set_visible(true);
    }

    /// Why SMART cannot be read here, if it cannot.
    fn smart_missing(&self) -> Option<String> {
        let missing: Vec<&str> = [("smartctl", "smartmontools"), ("pkexec", "polkit")]
            .iter()
            .filter(|(t, _)| !self.tools.has(t))
            .map(|(_, p)| *p)
            .collect();
        (!missing.is_empty()).then(|| format!("Needs {} to be installed.", missing.join(" and ")))
    }

    fn show_disks(self: &Rc<Self>, disks: Vec<Disk>) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        self.rows.borrow_mut().clear();
        if !self.tools.has("smartctl") {
            self.status.set_text("smartmontools is not installed: drive health cannot be read.");
        }
        for d in disks {
            let row = self.disk_row(&d);
            self.list.append(&row);
            self.rows.borrow_mut().push((d, row));
        }
        self.empty.set_visible(false);
        self.list.set_visible(true);
        self.apply_selection();
    }

    fn disk_row(self: &Rc<Self>, d: &Disk) -> adw::ExpanderRow {
        let row = adw::ExpanderRow::new();
        row.set_use_markup(false);
        row.set_title(&d.name());
        row.set_subtitle(&disk_subtitle(d));

        // Drive health
        let health = adw::ActionRow::new();
        health.set_use_markup(false);
        health.set_title("Drive health");
        health.set_subtitle_lines(0);
        let badge_slot: Rc<RefCell<Option<gtk::Label>>> = Rc::default();
        let shown: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
        let test_row = adw::ActionRow::new();
        test_row.set_title("Self-test");
        test_row.set_subtitle("The drive tests itself in the background. It takes a couple of minutes.");
        test_row.set_subtitle_lines(0);

        if let Some(why) = self.smart_missing() {
            health.set_subtitle(&why);
            test_row.set_subtitle(&why);
            test_row.set_sensitive(false);
        } else {
            health.set_subtitle("Not read yet. Reading it needs administrator permission.");
            let check = gtk::Button::with_label("Check Health…");
            check.set_valign(gtk::Align::Center);
            health.add_suffix(&check);
            let (w, dev) = (Rc::downgrade(self), d.path.clone());
            let (hr, rowc, shown2) = (health.clone(), row.clone(), shown.clone());
            check.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.check_health(dev.clone(), hr.clone(), rowc.clone(), shown2.clone(), badge_slot.clone());
                }
            });

            let start = gtk::Button::with_label("Start Short Test…");
            start.set_valign(gtk::Align::Center);
            test_row.add_suffix(&start);
            let (w, dev, tr) = (Rc::downgrade(self), d.path.clone(), test_row.clone());
            start.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.start_test(dev.clone(), tr.clone());
                }
            });
        }
        row.add_row(&health);
        row.add_row(&test_row);

        for v in &d.volumes {
            row.add_row(&self.volume_row(v));
        }
        row
    }

    fn volume_row(self: &Rc<Self>, v: &Volume) -> adw::ActionRow {
        let r = adw::ActionRow::new();
        r.set_use_markup(false);
        r.set_title(&format!("{} · {}", v.path, v.fstype));
        r.set_subtitle_lines(0);
        let state = match v.mounts.first() {
            Some(m) => format!("{} · mounted at {m}", size(v.size)),
            None => format!("{} · not mounted", size(v.size)),
        };
        match fscheck::check_action(v) {
            Ok(action) => {
                r.set_subtitle(&state);
                let scrub = matches!(action, RootAction::BtrfsScrub { .. });
                let b = gtk::Button::with_label(if scrub { "Scrub…" } else { "Check…" });
                b.set_valign(gtk::Align::Center);
                if self.tools.has("pkexec") && action.spec(&self.tools).is_some() {
                    let (w, v2, rc) = (Rc::downgrade(self), v.clone(), r.clone());
                    b.connect_clicked(move |_| {
                        if let Some(p) = w.upgrade() {
                            p.check_volume(v2.clone(), action.clone(), rc.clone(), state.clone());
                        }
                    });
                } else {
                    b.set_sensitive(false);
                    b.set_tooltip_text(Some("The needed tools are not installed."));
                }
                r.add_suffix(&b);
            }
            // Say why not, instead of a dead button.
            Err(why) if v.checkable() => r.set_subtitle(&format!("{state}\n{why}")),
            Err(_) => r.set_subtitle(&state),
        }
        r
    }

    /// Ask, showing the command that will run as administrator.
    async fn confirm(&self, action: &RootAction, heading: &str, what: &str, label: &str) -> bool {
        let Some(spec) = action.spec(&self.tools) else { return false };
        let body = format!(
            "{what}\n\nThis runs as administrator:\n{}\n\nNothing on the drive is changed.",
            privileged::display(&spec)
        );
        dialogs::confirm(&self.dialog, heading, &body, label, false).await
    }

    fn check_health(
        self: &Rc<Self>,
        dev: String,
        health: adw::ActionRow,
        parent: adw::ExpanderRow,
        shown: Rc<RefCell<Vec<adw::ActionRow>>>,
        badge_slot: Rc<RefCell<Option<gtk::Label>>>,
    ) {
        if self.busy.get() {
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(p) = w.upgrade() else { return };
            let action = RootAction::SmartRead { dev: dev.clone() };
            if !p
                .confirm(
                    &action,
                    "Read drive health?",
                    &format!("Luma will read the health report of {dev}."),
                    "Continue",
                )
                .await
            {
                return;
            }
            p.set_busy(true, &format!("Reading the health of {dev}…"));
            let rx = diskmgr::spawn_smart(dev);
            let res = rx.recv().await;
            let Some(p) = w.upgrade() else { return };
            p.set_busy(false, "");
            for old in shown.borrow_mut().drain(..) {
                parent.remove(&old);
            }
            if let Some(old) = badge_slot.borrow_mut().take() {
                health.remove(&old);
            }
            match res {
                Ok(Ok(report)) => {
                    let (h, why) = report.verdict();
                    let (text, class, line) = match h {
                        Health::Good => ("Good", "success", "The drive reports that it is healthy."),
                        Health::Warning => ("Warning", "warning", "The drive works, but something deserves attention."),
                        Health::Failing => ("Failing", "error", "Back up your data now."),
                        Health::Unknown => ("Unknown", "dim-label", "The drive did not give a clear answer."),
                    };
                    let mut sub = line.to_string();
                    for w in &why {
                        sub.push_str(&format!("\n• {w}"));
                    }
                    health.set_subtitle(&sub);
                    let b = badge(text, class);
                    health.add_suffix(&b);
                    *badge_slot.borrow_mut() = Some(b);
                    for r in report_rows(&report) {
                        // Details go right after the health row.
                        parent.add_row(&r);
                        shown.borrow_mut().push(r);
                    }
                }
                Ok(Err(e)) => health.set_subtitle(&e),
                Err(_) => {}
            }
        });
    }

    fn start_test(self: &Rc<Self>, dev: String, row: adw::ActionRow) {
        if self.busy.get() {
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(p) = w.upgrade() else { return };
            let action = RootAction::SmartSelfTest { dev: dev.clone() };
            let what = format!("The drive {dev} will run its short self-test. It keeps working normally meanwhile.");
            if !p.confirm(&action, "Start a self-test?", &what, "Start Test").await {
                return;
            }
            p.set_busy(true, &format!("Starting the self-test of {dev}…"));
            let res = diskmgr::spawn_self_test(dev).recv().await;
            let Some(p) = w.upgrade() else { return };
            p.set_busy(false, "");
            match res {
                Ok(Ok(())) => row.set_subtitle("Started. Check the health again in a few minutes to see the result."),
                Ok(Err(e)) => row.set_subtitle(&e),
                Err(_) => {}
            }
        });
    }

    fn check_volume(self: &Rc<Self>, v: Volume, action: RootAction, row: adw::ActionRow, state: String) {
        if self.busy.get() {
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(p) = w.upgrade() else { return };
            let (heading, what, label) = match &action {
                RootAction::BtrfsScrub { mount } => (
                    "Scrub this drive?",
                    format!(
                        "Luma will read everything on {mount} and verify it. On a large drive this can take hours, and the drive is busy meanwhile."
                    ),
                    "Start Scrub",
                ),
                _ => {
                    ("Check this drive?", format!("Luma will check the filesystem on {} for errors.", v.path), "Check")
                }
            };
            if !p.confirm(&action, heading, &what, label).await {
                return;
            }
            p.set_busy(true, &format!("Checking {}… this can take a while", v.path));
            let res = diskmgr::spawn_check(v.path.clone()).recv().await;
            let Some(p) = w.upgrade() else { return };
            p.set_busy(false, "");
            match res {
                Ok(Ok(r)) => p.show_result(&row, &state, &r),
                Ok(Err(e)) => row.set_subtitle(&format!("{state}\n{e}")),
                Err(_) => {}
            }
        });
    }

    fn show_result(&self, row: &adw::ActionRow, state: &str, r: &CheckResult) {
        let mark = match r.verdict {
            Verdict::Clean => "✓",
            Verdict::Problems => "⚠",
            Verdict::Failed => "✗",
        };
        row.set_subtitle(&format!("{state}\n{mark} {}", r.summary));
        if r.verdict != Verdict::Clean && !r.details.is_empty() {
            dialogs::error(&self.parent, &r.summary, &r.details);
        }
    }
}
