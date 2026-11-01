//! Operation manager UI: progress popover, conflict prompts, toasts, retry.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use crate::application::state::AppState;
use crate::core::operations::undo::UndoAction;
use crate::core::operations::{self, Control, Event, OpKind, OpRequest, Progress, Resolution, Summary};
use crate::ui::dialogs;

struct PendingConflict {
    src: String,
    dst: String,
    reply: mpsc::Sender<Resolution>,
}

struct Inner {
    state: Rc<AppState>,
    window: glib::WeakRef<gtk::Window>,
    toasts: adw::ToastOverlay,
    list: gtk::ListBox,
    button: gtk::MenuButton,
    conflicts: RefCell<VecDeque<PendingConflict>>,
    showing: Cell<bool>,
}

#[derive(Clone)]
pub struct OpsUi(Rc<Inner>);

fn plural(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

impl OpsUi {
    pub fn new(window: &gtk::Window, state: Rc<AppState>) -> Self {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&list));
        sw.set_propagate_natural_height(true);
        sw.set_max_content_height(360);
        sw.set_min_content_width(340);
        let pop = gtk::Popover::new();
        pop.set_child(Some(&sw));
        let button = gtk::MenuButton::new();
        button.set_icon_name("luma-activity-symbolic");
        button.set_tooltip_text(Some("Operations"));
        button.set_popover(Some(&pop));
        button.set_visible(false);
        Self(Rc::new(Inner {
            state,
            window: window.downgrade(),
            toasts: adw::ToastOverlay::new(),
            list,
            button,
            conflicts: RefCell::default(),
            showing: Cell::new(false),
        }))
    }

    pub fn toasts(&self) -> &adw::ToastOverlay {
        &self.0.toasts
    }
    pub fn button(&self) -> &gtk::MenuButton {
        &self.0.button
    }
    pub fn toast(&self, msg: &str) {
        self.0.toasts.add_toast(adw::Toast::new(msg));
    }

    fn refresh_visibility(&self) {
        self.0.button.set_visible(self.0.list.first_child().is_some());
    }

    fn pump_conflicts(&self) {
        if self.0.showing.get() {
            return;
        }
        let Some(p) = self.0.conflicts.borrow_mut().pop_front() else { return };
        self.0.showing.set(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let win = this.0.window.upgrade();
            let r = dialogs::ask_conflict(win.as_ref(), &p.src, &p.dst).await;
            let _ = p.reply.send(r);
            this.0.showing.set(false);
            this.pump_conflicts();
        });
    }

    /// Run a blocking job (archive, checksum, …) off the main thread with a
    /// pulsing row and a cancel button. `done` runs on the main thread.
    pub fn run_job<T: Send + 'static, E: Send + 'static>(
        &self,
        title: &str,
        job: impl FnOnce(Arc<AtomicBool>) -> Result<T, E> + Send + 'static,
        done: impl FnOnce(Result<T, E>) + 'static,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("heading");
        let stop = gtk::Button::from_icon_name("luma-close-symbolic");
        stop.add_css_class("flat");
        stop.set_tooltip_text(Some("Cancel"));
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        top.append(&label);
        top.append(&stop);
        let bar = gtk::ProgressBar::new();
        let v = gtk::Box::new(gtk::Orientation::Vertical, 6);
        v.set_margin_top(10);
        v.set_margin_bottom(10);
        v.set_margin_start(12);
        v.set_margin_end(12);
        v.append(&top);
        v.append(&bar);
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_selectable(false);
        row.set_child(Some(&v));
        self.0.list.append(&row);
        self.refresh_visibility();
        let c = cancel.clone();
        stop.connect_clicked(move |_| c.store(true, Ordering::Relaxed));

        let this = self.clone();
        glib::spawn_future_local(async move {
            let pulse = glib::timeout_add_local(Duration::from_millis(140), {
                let bar = bar.clone();
                move || {
                    bar.pulse();
                    glib::ControlFlow::Continue
                }
            });
            let res = gio::spawn_blocking(move || job(cancel)).await;
            pulse.remove();
            this.0.list.remove(&row);
            this.refresh_visibility();
            match res {
                Ok(r) => done(r),
                Err(_) => this.toast("A background task crashed"),
            }
        });
    }

    pub fn submit(&self, req: OpRequest) {
        let started = std::time::Instant::now();
        let (ctl, rx) = operations::spawn(req.clone());
        let row = OpRow::new(&req, &ctl);
        self.0.list.append(&row.row);
        self.refresh_visibility();
        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                match ev {
                    Event::Progress(p) => row.update(&p),
                    Event::Conflict { src, dst, reply } => {
                        this.0.conflicts.borrow_mut().push_back(PendingConflict { src, dst, reply });
                        this.pump_conflicts();
                    }
                    Event::Finished(s) => {
                        this.finished(&row, &req, s, started.elapsed());
                        break;
                    }
                }
            }
        });
    }

    /// Keep the undo journal and file tags in step with what actually happened.
    fn record(&self, req: &OpRequest, s: &Summary) {
        let st = &self.0.state;
        let path = |u: &str| gio::File::for_uri(u).path();
        match req.kind {
            OpKind::Copy => {
                let created: Vec<_> = s.done.iter().filter_map(|(_, d)| path(d)).collect();
                if !s.undoable {
                    st.undo.clear(); // something was replaced/merged: older undos may no longer be valid
                } else if !created.is_empty() {
                    st.undo.push("Copy", UndoAction::TrashCreated(created));
                }
            }
            OpKind::Move => {
                let pairs: Vec<_> =
                    s.done.iter().filter_map(|(a, b)| Some((path(a)?, path(b)?))).filter(|(a, b)| a != b).collect();
                for (old, new) in &pairs {
                    let _ = st.db.move_tags(old, new);
                }
                if !s.undoable {
                    st.undo.clear();
                } else if !pairs.is_empty() {
                    st.undo
                        .push("Move", UndoAction::MoveBack(pairs.into_iter().map(|(old, new)| (new, old)).collect()));
                }
            }
            OpKind::Trash => {
                let origs: Vec<_> = s.done.iter().filter_map(|(a, _)| path(a)).collect();
                if !origs.is_empty() {
                    st.undo.push("Move to Trash", UndoAction::Restore(origs));
                }
            }
            OpKind::Delete => {
                for u in req.sources.iter().filter(|u| !s.failed_sources.contains(u)) {
                    if let Some(p) = path(u) {
                        let _ = st.db.remove_tags_under(&p);
                    }
                }
            }
        }
    }

    /// Desktop notification for slow operations that finish while the window is in the background.
    fn notify(&self, req: &OpRequest, s: &Summary, elapsed: Duration) {
        let Some(win) = self.0.window.upgrade() else { return };
        if elapsed < Duration::from_secs(5) || win.is_active() {
            return;
        }
        let Some(app) = win.application() else { return };
        let ok = req.sources.len() - s.failed_sources.len().min(req.sources.len());
        let n = gio::Notification::new(&if s.errors.is_empty() {
            format!("{} {}", req.kind.past(), plural(ok))
        } else {
            format!("{} finished with errors", req.kind.verb())
        });
        n.set_body(Some(&match s.errors.first() {
            Some(e) => format!("{} error(s). First: {}", s.errors.len(), e.message),
            None => "The operation completed.".to_string(),
        }));
        n.set_priority(if s.errors.is_empty() {
            gio::NotificationPriority::Normal
        } else {
            gio::NotificationPriority::High
        });
        app.send_notification(None, &n);
    }

    fn finished(&self, row: &OpRow, req: &OpRequest, s: Summary, elapsed: Duration) {
        self.record(req, &s);
        self.notify(req, &s, elapsed);
        let ok = req.sources.len() - s.failed_sources.len().min(req.sources.len());
        if s.errors.is_empty() {
            self.0.list.remove(&row.row);
            self.refresh_visibility();
            self.toast(&if s.cancelled {
                "Operation cancelled".to_string()
            } else {
                format!("{} {}", req.kind.past(), plural(ok))
            });
            return;
        }
        let first = &s.errors[0];
        let name =
            gio::File::for_uri(&first.uri).basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut msg = format!("{name}: {}", first.message);
        if s.errors.len() > 1 {
            msg.push_str(&format!("\n…and {} more error(s)", s.errors.len() - 1));
        }
        let this = self.clone();
        let retry_req = (!s.failed_sources.is_empty() && !s.cancelled)
            .then(|| OpRequest { sources: s.failed_sources.clone(), ..req.clone() });
        let r2 = row.row.clone();
        row.show_failure(
            &format!("{} finished with {} error(s)", req.kind.verb(), s.errors.len()),
            &msg,
            retry_req.is_some(),
            {
                let this = this.clone();
                move || {
                    this.0.list.remove(&r2);
                    this.refresh_visibility();
                }
            },
            {
                let this = this.clone();
                let r3 = row.row.clone();
                move || {
                    this.0.list.remove(&r3);
                    if let Some(r) = retry_req.clone() {
                        this.submit(r);
                    }
                }
            },
        );
        self.toast(&format!("{} error(s) — see Operations", s.errors.len()));
        self.0.button.popup();
    }
}

struct OpRow {
    row: gtk::ListBoxRow,
    title: gtk::Label,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    pause: gtk::ToggleButton,
    cancel: gtk::Button,
    retry: gtk::Button,
}

impl OpRow {
    fn new(req: &OpRequest, ctl: &Control) -> Self {
        let mut t = format!("{} {}", req.kind.verb(), plural(req.sources.len()));
        if let (OpKind::Copy | OpKind::Move, Some(d)) = (req.kind, &req.dest) {
            let n = gio::File::for_uri(d).basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            t.push_str(&format!(" to “{n}”"));
        }
        let title = gtk::Label::new(Some(&t));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("heading");
        let pause = gtk::ToggleButton::new();
        pause.set_icon_name("luma-pause-symbolic");
        pause.set_tooltip_text(Some("Pause / resume"));
        pause.add_css_class("flat");
        pause.set_visible(matches!(req.kind, OpKind::Copy | OpKind::Move | OpKind::Delete));
        let cancel = gtk::Button::from_icon_name("luma-close-symbolic");
        cancel.set_tooltip_text(Some("Cancel"));
        cancel.add_css_class("flat");
        let retry = gtk::Button::with_label("Retry");
        retry.set_visible(false);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        top.append(&title);
        top.append(&retry);
        top.append(&pause);
        top.append(&cancel);
        let bar = gtk::ProgressBar::new();
        let status = gtk::Label::new(Some("Preparing…"));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_max_width_chars(48);
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        let v = gtk::Box::new(gtk::Orientation::Vertical, 6);
        {
            let m = v.upcast_ref::<gtk::Widget>();
            m.set_margin_top(10);
            m.set_margin_bottom(10);
            m.set_margin_start(12);
            m.set_margin_end(12);
        }
        v.append(&top);
        v.append(&bar);
        v.append(&status);
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_selectable(false);
        row.set_child(Some(&v));

        let c = ctl.clone();
        pause.connect_toggled(move |b| c.set_paused(b.is_active()));
        let c = ctl.clone();
        cancel.connect_clicked(move |_| c.cancel());
        Self { row, title, bar, status, pause, cancel, retry }
    }

    fn update(&self, p: &Progress) {
        self.bar.set_fraction(p.fraction());
        let mut s = String::new();
        if p.total_bytes > 0 {
            s = format!(
                "{} of {}",
                crate::core::filesystem::human_size(p.done_bytes as i64),
                crate::core::filesystem::human_size(p.total_bytes as i64)
            );
        } else if p.items_total > 0 {
            s = format!("{} of {}", p.items_done, plural(p.items_total as usize));
        }
        if !p.current.is_empty() {
            s = format!("{s} · {}", p.current);
        }
        self.status.set_text(&s);
    }

    fn show_failure(
        &self,
        title: &str,
        msg: &str,
        can_retry: bool,
        dismiss: impl Fn() + 'static,
        retry: impl Fn() + 'static,
    ) {
        self.title.set_text(title);
        self.bar.set_visible(false);
        self.pause.set_visible(false);
        self.status.set_text(msg);
        self.status.remove_css_class("dim-label");
        self.status.add_css_class("error");
        self.cancel.set_icon_name("luma-close-symbolic");
        self.cancel.set_tooltip_text(Some("Dismiss"));
        self.retry.set_visible(can_retry);
        self.cancel.connect_clicked(move |_| dismiss());
        self.retry.connect_clicked(move |_| retry());
    }
}
