//! Disk Manager, Schedule page: clean up on a timer without opening Luma.
//!
//! The timer is a systemd user unit that runs `luma --cleanup-run`. That run
//! can only do user-level cleanup of what is ticked here; it never asks for
//! administrator rights.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::Cell, path::PathBuf, rc::Rc, sync::atomic::AtomicBool};

use crate::core::diskmgr::{
    Env, ToolSet,
    exec::StdRunner,
    headless,
    schedule::{CleanupConfig, Interval, SCHEDULABLE},
};
use crate::services::cleanup_schedule::{self as sched, Paths, UnitState};
use crate::ui::dialogs;

pub struct SchedulePage {
    pub widget: gtk::Box,
    dialog: adw::Dialog,
    parent: gtk::Window,
    paths: Paths,
    tools: ToolSet,
    enable: adw::SwitchRow,
    interval: adw::ComboRow,
    providers: Vec<(&'static str, adw::SwitchRow)>,
    trash_days: adw::SpinRow,
    cache_days: adw::SpinRow,
    thumb_days: adw::SpinRow,
    status: adw::ActionRow,
    reinstall: gtk::Button,
    unit_preview: gtk::Label,
    message: gtk::Label,
    apply: gtk::Button,
    try_now: gtk::Button,
    busy: Cell<bool>,
}

/// The program the unit should run: this one, even after it was replaced on disk.
fn this_exe() -> PathBuf {
    let p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("luma"));
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s))
}

fn days_row(title: &str, subtitle: &str) -> adw::SpinRow {
    let r = adw::SpinRow::with_range(1.0, 3650.0, 1.0);
    r.set_title(title);
    r.set_subtitle(subtitle);
    r
}

impl SchedulePage {
    pub fn new(parent: &gtk::Window, dialog: &adw::Dialog) -> Rc<Self> {
        let page = adw::PreferencesPage::new();

        let enable = adw::SwitchRow::new();
        enable.set_title("Clean up automatically");
        let interval = adw::ComboRow::new();
        interval.set_title("How often");
        interval.set_model(Some(&gtk::StringList::new(&Interval::ALL.map(Interval::label))));
        let main = adw::PreferencesGroup::new();
        main.set_title("Automatic cleanup");
        main.set_description(Some(
            "Runs in the background, even when Luma is closed. Only your own files are touched.",
        ));
        main.add(&enable);
        main.add(&interval);
        page.add(&main);

        let what = adw::PreferencesGroup::new();
        what.set_title("What to clean");
        let providers: Vec<_> = SCHEDULABLE
            .iter()
            .map(|(id, label)| {
                let r = adw::SwitchRow::new();
                r.set_title(label);
                what.add(&r);
                (*id, r)
            })
            .collect();
        page.add(&what);

        let age = adw::PreferencesGroup::new();
        age.set_title("Only things older than");
        age.set_description(Some("In days. Anything changed in the last 24 hours is always kept."));
        let trash_days = days_row("The Trash", "Judged by the day it was deleted");
        let cache_days = days_row("Application caches and old logs", "Judged by the last change");
        let thumb_days = days_row("Thumbnails", "Judged by the last change");
        for r in [&trash_days, &cache_days, &thumb_days] {
            age.add(r);
        }
        page.add(&age);

        let status = adw::ActionRow::new();
        status.set_title("Status");
        status.set_subtitle_lines(0);
        status.set_use_markup(false);
        let reinstall = gtk::Button::with_label("Reinstall");
        reinstall.set_valign(gtk::Align::Center);
        reinstall.set_visible(false);
        status.add_suffix(&reinstall);
        let unit_preview = gtk::Label::new(None);
        unit_preview.set_selectable(true);
        unit_preview.set_xalign(0.0);
        unit_preview.add_css_class("monospace");
        unit_preview.set_margin_top(8);
        unit_preview.set_margin_bottom(8);
        unit_preview.set_margin_start(12);
        let preview = adw::ExpanderRow::new();
        preview.set_title("Show what will be installed");
        preview.set_subtitle("Two small files in your user systemd folder");
        let holder = adw::ActionRow::new();
        holder.set_child(Some(&unit_preview));
        preview.add_row(&holder);
        let st = adw::PreferencesGroup::new();
        st.set_title("Timer");
        st.add(&status);
        st.add(&preview);
        page.add(&st);

        let try_now = gtk::Button::with_label("Preview a Run");
        try_now.set_tooltip_text(Some("Shows what a scheduled run would remove now, without removing anything"));
        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        let message = gtk::Label::new(None);
        message.set_xalign(0.0);
        message.set_hexpand(true);
        message.set_wrap(true);
        message.add_css_class("dim-label");
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        foot.set_margin_top(8);
        foot.set_margin_bottom(8);
        foot.set_margin_start(12);
        foot.set_margin_end(12);
        foot.append(&message);
        foot.append(&try_now);
        foot.append(&apply);

        let sw = gtk::ScrolledWindow::new();
        sw.set_vexpand(true);
        sw.set_child(Some(&page));
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&sw);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&foot);

        let me = Rc::new(Self {
            widget,
            dialog: dialog.clone(),
            parent: parent.clone(),
            paths: Paths::detect(),
            tools: ToolSet::detect(),
            enable,
            interval,
            providers,
            trash_days,
            cache_days,
            thumb_days,
            status,
            reinstall,
            unit_preview,
            message,
            apply,
            try_now,
            busy: Cell::new(false),
        });
        me.load();

        let w = Rc::downgrade(&me);
        me.apply.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.apply();
            }
        });
        let w = Rc::downgrade(&me);
        me.reinstall.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.apply();
            }
        });
        let w = Rc::downgrade(&me);
        me.try_now.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.preview_run();
            }
        });
        // Keep the unit preview in step with what is chosen.
        for w in [me.interval.upcast_ref::<gtk::Widget>(), me.enable.upcast_ref()] {
            let weak = Rc::downgrade(&me);
            w.connect_notify_local(None, move |_, _| {
                if let Some(p) = weak.upgrade() {
                    p.refresh_preview();
                }
            });
        }
        me
    }

    fn load(self: &Rc<Self>) {
        let cfg = match sched::load(&self.paths) {
            Ok(c) => c,
            Err(e) => {
                self.message.set_text(&format!("{e} Showing the defaults."));
                CleanupConfig::default()
            }
        };
        let state = sched::status(&self.paths, &this_exe());
        self.enable.set_active(matches!(state, UnitState::Installed { .. }));
        self.interval.set_selected(Interval::ALL.iter().position(|i| *i == cfg.interval).unwrap_or(1) as u32);
        for (id, row) in &self.providers {
            row.set_active(cfg.wants(id));
        }
        self.trash_days.set_value(cfg.trash_days.into());
        self.cache_days.set_value(cfg.cache_days.into());
        self.thumb_days.set_value(cfg.thumbnail_days.into());
        self.refresh_preview();
        self.refresh_status();
    }

    fn config(&self) -> CleanupConfig {
        CleanupConfig {
            enabled: self.enable.is_active(),
            interval: Interval::ALL[(self.interval.selected() as usize).min(Interval::ALL.len() - 1)],
            providers: self.providers.iter().filter(|(_, r)| r.is_active()).map(|(id, _)| id.to_string()).collect(),
            trash_days: self.trash_days.value() as u32,
            cache_days: self.cache_days.value() as u32,
            thumbnail_days: self.thumb_days.value() as u32,
        }
    }

    fn refresh_preview(&self) {
        let (svc, tim) = sched::unit_texts(&self.config(), &this_exe());
        self.unit_preview.set_text(&format!(
            "{}/{}\n{svc}\n{}/{}\n{tim}",
            self.paths.unit_dir.display(),
            crate::core::diskmgr::schedule::SERVICE_NAME,
            self.paths.unit_dir.display(),
            crate::core::diskmgr::schedule::TIMER_NAME
        ));
    }

    fn refresh_status(self: &Rc<Self>) {
        self.reinstall.set_visible(false);
        match sched::status(&self.paths, &this_exe()) {
            UnitState::NotInstalled => {
                self.status.set_subtitle("Not installed. Nothing runs in the background.");
                self.apply.set_sensitive(true);
            }
            UnitState::Foreign(name) => {
                self.status.set_subtitle(&format!(
                    "A unit named {name} already exists that Luma did not make, so it will not be changed."
                ));
                self.apply.set_sensitive(false);
                self.enable.set_sensitive(false);
            }
            UnitState::Installed { stale_exe, .. } => {
                self.apply.set_sensitive(true);
                if let Some(old) = stale_exe {
                    self.status.set_subtitle(&format!(
                        "Installed, but it starts {old}, not this copy of Luma. Reinstall to fix it."
                    ));
                    self.reinstall.set_visible(true);
                    return;
                }
                self.status.set_subtitle("Installed.");
                let (w, tools) = (Rc::downgrade(self), self.tools.clone());
                glib::spawn_future_local(async move {
                    let info = gio::spawn_blocking(move || sched::timer_info(&StdRunner::default(), &tools)).await;
                    let (Some(p), Ok(info)) = (w.upgrade(), info) else { return };
                    let text = match (info.active, info.next) {
                        (true, Some(n)) => format!("Installed. Next run: {n}."),
                        (true, None) => "Installed.".into(),
                        (false, _) => "Installed, but the timer is not running. Press Apply to start it.".into(),
                    };
                    p.status.set_subtitle(&text);
                });
            }
        }
    }

    fn set_busy(&self, busy: bool, text: &str) {
        self.busy.set(busy);
        self.apply.set_sensitive(!busy);
        self.try_now.set_sensitive(!busy);
        self.reinstall.set_sensitive(!busy);
        self.message.set_text(text);
    }

    /// Save the choices, then install or remove the timer to match the switch.
    fn apply(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let cfg = self.config();
        if let Err(e) = cfg.validate() {
            self.message.set_text(&e);
            return;
        }
        if cfg.enabled && cfg.providers.is_empty() {
            self.message.set_text("Pick at least one thing to clean, or turn automatic cleanup off.");
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(p) = w.upgrade() else { return };
            if cfg.enabled {
                let what: Vec<_> =
                    SCHEDULABLE.iter().filter(|(id, _)| cfg.wants(id)).map(|(_, l)| l.to_lowercase()).collect();
                let body = format!(
                    "Luma will add a timer to your account that runs {}. It cleans {} and runs at low priority.\n\nNothing runs as administrator.",
                    cfg.interval.label().to_lowercase(),
                    what.join(", ")
                );
                if !dialogs::confirm(&p.dialog, "Turn on automatic cleanup?", &body, "Turn On", false).await {
                    return;
                }
            }
            p.set_busy(true, if cfg.enabled { "Installing the timer…" } else { "Removing the timer…" });
            let (paths, tools, exe, c2) = (p.paths.clone(), p.tools.clone(), this_exe(), cfg.clone());
            let res = gio::spawn_blocking(move || {
                let r = StdRunner::default();
                if c2.enabled {
                    sched::install(&paths, &r, &tools, &c2, &exe)
                } else {
                    sched::save(&paths, &c2).and_then(|_| sched::remove(&paths, &r, &tools))
                }
            })
            .await;
            let Some(p) = w.upgrade() else { return };
            match res {
                Ok(Ok(())) => p.set_busy(
                    false,
                    if cfg.enabled { "Automatic cleanup is on." } else { "Automatic cleanup is off." },
                ),
                Ok(Err(e)) => p.set_busy(false, &e),
                Err(_) => p.set_busy(false, "Something went wrong."),
            }
            p.refresh_status();
        });
    }

    /// Show what a run would do right now. Changes nothing.
    fn preview_run(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true, "Looking…");
        let cfg = self.config();
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || {
                let runner = StdRunner::default();
                let mut env = Env::detect(&runner);
                headless::run_unattended(&mut env, &cfg, true, &AtomicBool::new(false)).summary()
            })
            .await;
            let Some(p) = w.upgrade() else { return };
            p.set_busy(false, "");
            if let Ok(text) = res {
                dialogs::error(&p.parent, "What a run would do now", &text);
            }
        });
    }
}
