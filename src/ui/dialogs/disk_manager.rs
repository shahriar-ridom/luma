//! Disk Manager: find things to clean up, review them, and free the space.
//!
//! Nothing is removed without a review step that lists what will happen, and
//! the review shows the very plan that is then carried out.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::application::state::AppState;
use crate::core::diskmgr::{Action, Distro, Item, Plan, Risk, Scope, Suggestion, ToolSet, privileged};
use crate::core::filesystem::human_size;
use crate::services::{
    cleanup_schedule,
    diskmgr::{self, CleanEvent, CleanReport, ScanEvent},
    storage,
};
use crate::ui::dialogs;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Scanning,
    Cleaning,
}

/// A suggestion as the page tracks it; `items[i].selected` follows the check boxes.
struct Sug {
    id: String,
    items: Vec<Item>,
}

struct Page {
    widget: gtk::Box,
    dialog: adw::Dialog,
    parent: gtk::Window,
    state: Rc<AppState>,
    list: gtk::ListBox,
    empty: gtk::Label,
    history_box: gtk::Box,
    history: gtk::ListBox,
    scan_btn: gtk::Button,
    heavy: gtk::CheckButton,
    clean_btn: gtk::Button,
    spinner: gtk::Spinner,
    status: gtk::Label,
    sugs: RefCell<Vec<Sug>>,
    mode: Cell<Mode>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
}

/// What the user calls each provider, for progress text.
fn provider_name(id: &str) -> &str {
    match id {
        "trash" => "the Trash",
        "user-cache" => "application caches",
        "thumbnails" => "thumbnails",
        "x11-logs" => "old logs",
        "pkg-cache" => "the package cache",
        "journal" => "the system journal",
        "coredumps" => "crash dumps",
        "orphans" => "unused packages",
        "large-files" => "large files",
        "duplicates" => "duplicate files",
        other => other,
    }
}

fn margin(w: &impl IsA<gtk::Widget>, top: i32, bottom: i32) {
    w.set_margin_top(top);
    w.set_margin_bottom(bottom);
    w.set_margin_start(12);
    w.set_margin_end(12);
}

fn size(b: u64) -> String {
    human_size(b.min(i64::MAX as u64) as i64)
}

impl Page {
    fn new(parent: &gtk::Window, state: Rc<AppState>, dialog: &adw::Dialog) -> Rc<Self> {
        let disk = gtk::ListBox::new();
        disk.add_css_class("boxed-list");
        disk.set_selection_mode(gtk::SelectionMode::None);
        margin(&disk, 12, 0);
        let disk_row = adw::ActionRow::new();
        disk_row.set_use_markup(false);
        disk_row.set_subtitle_lines(0);
        let d = Distro::detect();
        disk_row.set_title(&d.name);
        disk_row.set_subtitle(&format!("Package manager: {} · reading your home drive…", d.pm.name()));
        disk.append(&disk_row);

        let scan_btn = gtk::Button::with_label("Scan");
        scan_btn.add_css_class("suggested-action");
        let heavy = gtk::CheckButton::with_label("Include large files and duplicates (slower)");
        heavy.set_hexpand(true);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        margin(&controls, 12, 4);
        controls.append(&heavy);
        controls.append(&scan_btn);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_visible(false);
        let empty = gtk::Label::new(Some("Press Scan to look for things you can clean up."));
        empty.add_css_class("dim-label");
        empty.set_margin_top(24);
        empty.set_margin_bottom(24);

        let history = gtk::ListBox::new();
        history.add_css_class("boxed-list");
        history.set_selection_mode(gtk::SelectionMode::None);
        let heading = gtk::Label::new(Some("Recent cleanups"));
        heading.add_css_class("heading");
        heading.set_xalign(0.0);
        let history_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        history_box.set_margin_top(18);
        history_box.append(&heading);
        history_box.append(&history);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        margin(&content, 8, 12);
        content.append(&list);
        content.append(&empty);
        content.append(&history_box);
        let sw = gtk::ScrolledWindow::new();
        sw.set_vexpand(true);
        sw.set_child(Some(&content));

        let spinner = gtk::Spinner::new();
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.add_css_class("dim-label");
        let clean_btn = gtk::Button::with_label("Review and Clean…");
        clean_btn.add_css_class("destructive-action");
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margin(&foot, 8, 8);
        foot.append(&spinner);
        foot.append(&status);
        foot.append(&clean_btn);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&disk);
        widget.append(&controls);
        widget.append(&sw);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&foot);

        let page = Rc::new(Self {
            widget,
            dialog: dialog.clone(),
            parent: parent.clone(),
            state,
            list,
            empty,
            history_box,
            history,
            scan_btn,
            heavy,
            clean_btn,
            spinner,
            status,
            sugs: RefCell::default(),
            mode: Cell::new(Mode::Idle),
            cancel: RefCell::default(),
        });

        let w = Rc::downgrade(&page);
        page.scan_btn.connect_clicked(move |_| {
            let Some(p) = w.upgrade() else { return };
            match p.mode.get() {
                Mode::Idle => p.scan(),
                Mode::Scanning => p.stop(),
                Mode::Cleaning => {}
            }
        });
        let w = Rc::downgrade(&page);
        page.clean_btn.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.review_and_clean();
            }
        });

        let w = Rc::downgrade(&page);
        glib::spawn_future_local(async move {
            let home = glib::home_dir();
            let info = storage::fs_info(&gio::File::for_path(&home)).await;
            let Some(p) = w.upgrade() else { return };
            let mut sub = format!("Package manager: {}", d.pm.name());
            if let Some(i) = info {
                sub.push_str(&format!(
                    "\nHome drive: {} used of {} · {} free",
                    size(i.used),
                    size(i.size),
                    size(i.free)
                ));
                let bar = gtk::LevelBar::for_interval(0.0, 1.0);
                bar.set_value(i.used_fraction());
                bar.set_width_request(130);
                bar.set_valign(gtk::Align::Center);
                bar.add_css_class("fm-usage");
                bar.add_css_class(match i.used_fraction() {
                    f if f > 0.95 => "crit",
                    f if f > 0.85 => "warn",
                    _ => "ok",
                });
                disk_row.add_suffix(&bar);
            }
            disk_row.set_subtitle(&sub);
            drop(p);
        });

        page.refresh_history();
        page.set_mode(Mode::Idle, "");
        page
    }

    fn set_mode(&self, mode: Mode, text: &str) {
        self.mode.set(mode);
        self.spinner.set_spinning(mode != Mode::Idle);
        self.status.set_text(text);
        self.scan_btn.set_label(if mode == Mode::Scanning { "Stop" } else { "Scan" });
        self.scan_btn.set_sensitive(mode != Mode::Cleaning);
        self.heavy.set_sensitive(mode == Mode::Idle);
        // Closing mid-clean would lose the result, so it is blocked meanwhile.
        self.dialog.set_can_close(mode != Mode::Cleaning);
        self.update_clean_button();
    }

    fn selected(&self) -> (usize, u64) {
        let sugs = self.sugs.borrow();
        sugs.iter().flat_map(|s| &s.items).filter(|i| i.selected).fold((0, 0), |(n, b), i| (n + 1, b + i.bytes))
    }

    fn update_clean_button(&self) {
        let (n, bytes) = self.selected();
        self.clean_btn.set_sensitive(self.mode.get() == Mode::Idle && n > 0);
        if self.mode.get() == Mode::Idle && !self.sugs.borrow().is_empty() {
            self.status.set_text(&format!("{n} selected · {}", size(bytes)));
        }
    }

    fn stop(&self) {
        if let Some(c) = self.cancel.borrow().as_ref() {
            c.store(true, Ordering::Relaxed);
        }
        self.status.set_text("Stopping…");
    }

    fn reset_results(&self) {
        self.sugs.borrow_mut().clear();
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        self.list.set_visible(false);
        self.empty.set_visible(false);
    }

    fn scan(self: &Rc<Self>) {
        self.reset_results();
        self.set_mode(Mode::Scanning, "Looking for things to clean up…");
        let (cancel, rx) = diskmgr::spawn_scan(self.heavy.is_active());
        *self.cancel.borrow_mut() = Some(cancel.clone());
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(p) = w.upgrade() else {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                };
                match ev {
                    ScanEvent::Working(id) => p.status.set_text(&format!("Checking {}…", provider_name(id))),
                    ScanEvent::Found(s) => p.add_suggestion(s),
                    ScanEvent::Done => break,
                }
            }
            if let Some(p) = w.upgrade() {
                let none = p.sugs.borrow().is_empty();
                p.empty.set_visible(none);
                if none {
                    p.empty.set_text(if cancel.load(Ordering::Relaxed) {
                        "Scan stopped."
                    } else {
                        "Nothing to clean up. Your drive looks tidy."
                    });
                }
                p.set_mode(Mode::Idle, "");
            }
        });
    }

    fn add_suggestion(self: &Rc<Self>, s: Suggestion) {
        let idx = self.sugs.borrow().len();
        let row = adw::ExpanderRow::new();
        row.set_use_markup(false);
        row.set_title(&s.title);
        row.set_subtitle(&s.detail);
        row.set_subtitle_lines(0);
        let badge = gtk::Label::new(Some(match (s.risk, s.scope) {
            (Risk::SuggestOnly, _) => "Manual",
            (_, Scope::Root) => "Needs admin",
            (Risk::Safe, _) => "Safe",
            (Risk::Review, _) => "Review first",
        }));
        badge.add_css_class("dim-label");
        badge.set_valign(gtk::Align::Center);
        row.add_suffix(&badge);
        if let Some(b) = s.est_bytes {
            let l = gtk::Label::new(Some(&size(b)));
            l.set_valign(gtk::Align::Center);
            l.add_css_class("numeric");
            row.add_suffix(&l);
        }

        if s.risk == Risk::SuggestOnly {
            if let Some(cmd) = s.hint.as_ref().map(shell_words::join) {
                row.set_subtitle(&format!("{}\n{cmd}", s.detail));
                let copy = gtk::Button::with_label("Copy command");
                copy.set_valign(gtk::Align::Center);
                let w = self.parent.downgrade();
                copy.connect_clicked(move |_| {
                    if let Some(p) = w.upgrade() {
                        p.clipboard().set_text(&cmd);
                    }
                });
                row.add_suffix(&copy);
            }
            self.list.append(&row);
            self.list.set_visible(true);
            return;
        }

        let checks: Rc<RefCell<Vec<gtk::CheckButton>>> = Rc::default();
        for (j, it) in s.items.iter().enumerate() {
            let r = adw::ActionRow::new();
            r.set_use_markup(false);
            r.set_title(&it.label);
            let c = gtk::CheckButton::new();
            c.set_active(it.selected);
            c.set_valign(gtk::Align::Center);
            r.add_prefix(&c);
            let l = gtk::Label::new(Some(&size(it.bytes)));
            l.add_css_class("dim-label");
            r.add_suffix(&l);
            r.set_activatable_widget(Some(&c));
            let w: Weak<Page> = Rc::downgrade(self);
            c.connect_toggled(move |c| {
                let Some(p) = w.upgrade() else { return };
                if let Some(it) = p.sugs.borrow_mut().get_mut(idx).and_then(|s| s.items.get_mut(j)) {
                    it.selected = c.is_active();
                }
                p.update_clean_button();
            });
            checks.borrow_mut().push(c);
            row.add_row(&r);
        }
        let master = gtk::CheckButton::new();
        master.set_active(s.items.iter().all(|i| i.selected));
        master.set_valign(gtk::Align::Center);
        master.set_tooltip_text(Some("Select or clear everything in this group"));
        let cs = checks.clone();
        master.connect_toggled(move |m| {
            for c in cs.borrow().iter() {
                c.set_active(m.is_active());
            }
        });
        row.add_prefix(&master);

        self.sugs.borrow_mut().push(Sug { id: s.id, items: s.items });
        self.list.append(&row);
        self.list.set_visible(true);
        self.update_clean_button();
    }

    fn refresh_history(&self) {
        while let Some(c) = self.history.first_child() {
            self.history.remove(&c);
        }
        let runs = self.state.db.cleanup_runs(5).unwrap_or_default();
        self.history_box.set_visible(!runs.is_empty());
        for r in runs {
            let row = adw::ActionRow::new();
            row.set_use_markup(false);
            let when = glib::DateTime::from_unix_local(r.ts / 1000)
                .ok()
                .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let scheduled = r.source == "scheduled";
            row.set_title(&if scheduled { format!("{when} · scheduled") } else { when });
            row.set_subtitle(&r.summary);
            let l = gtk::Label::new(Some(&format!("{} freed", size(r.freed.max(0) as u64))));
            l.add_css_class("dim-label");
            row.add_suffix(&l);
            self.history.append(&row);
        }
    }

    /// Plan the selection, show exactly what will happen, then do it.
    fn review_and_clean(self: &Rc<Self>) {
        let selection: Vec<(String, Vec<Item>)> = self
            .sugs
            .borrow()
            .iter()
            .map(|s| (s.id.clone(), s.items.iter().filter(|i| i.selected).cloned().collect::<Vec<_>>()))
            .filter(|(_, items)| !items.is_empty())
            .collect();
        if selection.is_empty() {
            return;
        }
        self.set_mode(Mode::Cleaning, "Checking what will be removed…");
        let rx = diskmgr::spawn_plan(selection.clone());
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(plans) = rx.recv().await else { return };
            let Some(p) = w.upgrade() else { return };
            if plans.is_empty() {
                p.set_mode(Mode::Idle, "");
                dialogs::error(&p.parent, "Nothing to clean", "The selected items are gone or no longer old enough.");
                p.scan();
                return;
            }
            let body = review_text(&selection, &plans);
            if !dialogs::confirm(&p.dialog, "Clean up these files?", &body, "Clean Up", true).await {
                p.set_mode(Mode::Idle, "");
                return;
            }
            p.run(plans);
        });
    }

    fn run(self: &Rc<Self>, plans: Vec<(String, Plan)>) {
        self.set_mode(Mode::Cleaning, "Cleaning…");
        let (_cancel, rx) = diskmgr::spawn_clean(plans);
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(p) = w.upgrade() else { return };
                match ev {
                    CleanEvent::Progress(n) => p.status.set_text(&format!("Cleaning… {} so far", size(n))),
                    CleanEvent::Finished(report) => {
                        p.finished(&report);
                        return;
                    }
                }
            }
        });
    }

    fn finished(self: &Rc<Self>, r: &CleanReport) {
        let o = &r.outcome;
        let mut parts = vec![];
        if o.freed > 0 {
            parts.push(format!("{} deleted", size(o.freed)));
        }
        if o.trashed > 0 {
            parts.push(format!("{} moved to the Trash", size(o.trashed)));
        }
        if o.commands_ok > 0 {
            parts.push(format!("{} administrator task(s) ran", o.commands_ok));
        }
        if !o.errors.is_empty() {
            parts.push(format!("{} failed", o.errors.len()));
        }
        let summary = if parts.is_empty() { "Nothing was removed".to_string() } else { parts.join(", ") };
        if let Err(e) = self.state.db.add_cleanup_run("manual", o.freed.min(i64::MAX as u64) as i64, &summary) {
            tracing::warn!("could not save cleanup history: {e}");
        }
        self.refresh_history();

        let mut body = vec![summary.clone() + "."];
        if o.trashed > 0 {
            body.push("Space used by Trash items comes back when you empty the Trash.".into());
        }
        if let Some(d) = r.free_delta() {
            let sign = if d < 0 { "-" } else { "" };
            body.push(format!("Free space on your home drive changed by {sign}{}.", size(d.unsigned_abs())));
        }
        if !o.errors.is_empty() {
            body.push(format!("\nProblems:\n{}", o.errors.iter().take(4).cloned().collect::<Vec<_>>().join("\n")));
            if o.errors.len() > 4 {
                body.push(format!("…and {} more.", o.errors.len() - 4));
            }
        }
        let d = adw::AlertDialog::new(Some("Cleanup finished"), Some(&body.join("\n")));
        d.add_response("ok", "OK");
        d.present(Some(&self.dialog));
        self.set_mode(Mode::Idle, "");
        self.scan();
    }
}

/// The review text: counts, sizes, a few example paths and the exact
/// administrator commands, all taken from the real plan.
fn review_text(selection: &[(String, Vec<Item>)], plans: &[(String, Plan)]) -> String {
    let (mut deleted, mut trashed) = ((0usize, 0u64), (0usize, 0u64));
    let mut admin_bytes = 0u64;
    let mut samples: Vec<String> = vec![];
    let mut files = 0usize;
    let mut admin: Vec<String> = vec![];
    for (id, plan) in plans {
        let bytes: u64 =
            selection.iter().filter(|(sid, _)| sid == id).flat_map(|(_, items)| items).map(|i| i.bytes).sum();
        let n = plan.actions.iter().filter(|a| !matches!(a, Action::Exec(_))).count();
        if plan.actions.iter().any(|a| matches!(a, Action::RemovePath(_))) {
            deleted = (deleted.0 + n, deleted.1 + bytes);
        } else if plan.actions.iter().any(|a| matches!(a, Action::TrashPath(_))) {
            trashed = (trashed.0 + n, trashed.1 + bytes);
        } else {
            admin_bytes += bytes;
        }
        for a in &plan.actions {
            match a {
                Action::RemovePath(p) | Action::TrashPath(p) => {
                    files += 1;
                    if samples.len() < 6 {
                        samples.push(p.display().to_string());
                    }
                }
                Action::Exec(c) if c.privileged => admin.push(privileged::display(c)),
                Action::Exec(c) => admin.push(c.display()),
            }
        }
    }
    let mut lines = vec![];
    if deleted.0 > 0 {
        lines.push(format!("{} files, about {}, will be deleted permanently.", deleted.0, size(deleted.1)));
    }
    if trashed.0 > 0 {
        lines.push(format!("{} files, about {}, will be moved to the Trash.", trashed.0, size(trashed.1)));
    }
    if files > 0 {
        lines.push(String::new());
        lines.extend(samples);
        if files > 6 {
            lines.push(format!("…and {} more.", files - 6));
        }
    }
    if !admin.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        let about = if admin_bytes > 0 { format!(" (about {})", size(admin_bytes)) } else { String::new() };
        lines
            .push(format!("These run with administrator rights{about}. You will be asked for your password for each:"));
        lines.extend(admin);
    }
    lines.join("\n")
}

/// Which page to open first.
pub enum Start {
    Cleanup,
    /// The Health page, with the disk that is or holds this device opened.
    Health(Option<String>),
}

pub fn show(parent: &gtk::Window, state: Rc<AppState>) {
    show_at(parent, state, Start::Cleanup);
}

pub fn show_at(parent: &gtk::Window, state: Rc<AppState>, start: Start) {
    // Pick up what scheduled runs recorded since the last time.
    let _ = state.db.import_scheduled_runs(&cleanup_schedule::recorded_runs(&cleanup_schedule::Paths::detect()));
    let dialog = adw::Dialog::builder().title("Disk Manager").content_width(720).content_height(640).build();
    let page = Page::new(parent, state, &dialog);
    let health = dialogs::disk_health::HealthPage::new(parent, &dialog);

    let stack = adw::ViewStack::new();
    stack.add_titled(&page.widget, Some("cleanup"), "Cleanup");
    stack.add_titled(&health.widget, Some("health"), "Health");
    // Without a systemd user session there is nothing to schedule with.
    if cleanup_schedule::available(&ToolSet::detect()) {
        let schedule = dialogs::disk_schedule::SchedulePage::new(parent, &dialog);
        stack.add_titled(&schedule.widget, Some("schedule"), "Schedule");
    }
    let h = health.clone();
    stack.connect_visible_child_name_notify(move |s| {
        if s.visible_child_name().as_deref() == Some("health") {
            h.ensure_loaded();
        }
    });
    let switcher = adw::ViewSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&stack));
    dialog.set_child(Some(&tv));

    if let Start::Health(dev) = start {
        if let Some(dev) = dev {
            health.select(&dev);
        }
        stack.set_visible_child_name("health");
    }
    let p = page.clone();
    dialog.connect_closed(move |_| p.stop());
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diskmgr::CmdSpec;
    use std::path::PathBuf;

    fn item(bytes: u64) -> Item {
        Item { label: "x".into(), path: None, bytes, selected: true }
    }

    #[test]
    fn review_lists_exact_admin_commands_and_file_counts() {
        let admin = CmdSpec { argv: vec!["/usr/bin/paccache".into(), "-r".into()], privileged: true };
        let plans = vec![
            ("user-cache".to_string(), Plan { actions: vec![Action::RemovePath(PathBuf::from("/h/.cache/a"))] }),
            ("large-files".to_string(), Plan { actions: vec![Action::TrashPath(PathBuf::from("/h/big.iso"))] }),
            ("pkg-cache".to_string(), Plan { actions: vec![Action::Exec(admin)] }),
        ];
        let sel = vec![
            ("user-cache".to_string(), vec![item(2048)]),
            ("large-files".to_string(), vec![item(4096)]),
            ("pkg-cache".to_string(), vec![item(8192)]),
        ];
        let t = review_text(&sel, &plans);
        assert!(t.contains("1 files, about") && t.contains("will be deleted permanently"), "{t}");
        assert!(t.contains("moved to the Trash"), "{t}");
        assert!(t.contains("/h/.cache/a") && t.contains("/h/big.iso"), "{t}");
        assert!(t.contains("pkexec /usr/bin/paccache -r"), "{t}");
        assert!(t.contains("administrator rights"), "{t}");
    }

    #[test]
    fn review_of_user_only_plans_mentions_no_admin() {
        let plans = vec![("trash".to_string(), Plan { actions: vec![Action::RemovePath(PathBuf::from("/t/a"))] })];
        let t = review_text(&[("trash".to_string(), vec![item(10)])], &plans);
        assert!(!t.contains("administrator"), "{t}");
    }
}
