//! Operation manager: runs file operations and background jobs, reports
//! them in the activity island, asks about conflicts and keeps the undo
//! journal in step. Short messages also go through the island.

use gtk::{gio, glib, prelude::*};
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
use crate::core::operations::{self, Event, OpKind, OpRequest, Progress, Resolution, Summary};
use crate::ui::dialogs;
use crate::ui::island::{Island, Outcome, PauseFn};

struct PendingConflict {
    src: String,
    dst: String,
    reply: mpsc::Sender<Resolution>,
}

struct Inner {
    state: Rc<AppState>,
    window: glib::WeakRef<gtk::Window>,
    island: Rc<Island>,
    conflicts: RefCell<VecDeque<PendingConflict>>,
    showing: Cell<bool>,
    /// Called when an operation or job ends (files and free space changed).
    on_settled: RefCell<Option<Rc<dyn Fn()>>>,
}

#[derive(Clone)]
pub struct OpsUi(Rc<Inner>);

fn plural(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// "Copy 3 items to “Photos”"
fn op_title(req: &OpRequest) -> String {
    let mut t = format!("{} {}", req.kind.verb(), plural(req.sources.len()));
    if let (OpKind::Copy | OpKind::Move, Some(d)) = (req.kind, &req.dest) {
        let n = gio::File::for_uri(d).basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        t.push_str(&format!(" to “{n}”"));
    }
    t
}

/// "1.2 GB of 3.4 GB · photo.jpg"
fn status_text(p: &Progress) -> String {
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
        s = if s.is_empty() { p.current.clone() } else { format!("{s} · {}", p.current) };
    }
    s
}

impl OpsUi {
    pub fn new(window: &gtk::Window, state: Rc<AppState>) -> Self {
        let island = Island::new();
        island.attach(window);
        Self(Rc::new(Inner {
            state,
            window: window.downgrade(),
            island,
            conflicts: RefCell::default(),
            showing: Cell::new(false),
            on_settled: RefCell::default(),
        }))
    }

    pub fn island(&self) -> &Rc<Island> {
        &self.0.island
    }

    /// How many operations and background jobs are still running.
    pub fn active_count(&self) -> usize {
        self.0.island.active_count()
    }

    pub fn cancel_all(&self) {
        self.0.island.cancel_all();
    }

    pub fn set_on_settled(&self, f: impl Fn() + 'static) {
        *self.0.on_settled.borrow_mut() = Some(Rc::new(f));
    }

    fn settled(&self) {
        let cb = self.0.on_settled.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    /// A short message, shown in the island.
    pub fn toast(&self, msg: &str) {
        self.0.island.message(msg);
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

    /// Run a blocking job (archive, checksum, …) off the main thread, shown
    /// in the island with a cancel button. `done` runs on the main thread
    /// and usually posts its own message.
    pub fn run_job<T: Send + 'static, E: Send + 'static>(
        &self,
        title: &str,
        job: impl FnOnce(Arc<AtomicBool>) -> Result<T, E> + Send + 'static,
        done: impl FnOnce(Result<T, E>) + 'static,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let id = self.0.island.add(title, Rc::new(move || c.store(true, Ordering::Relaxed)), None);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let pulse = glib::timeout_add_local(Duration::from_millis(140), {
                let isl = this.0.island.clone();
                move || {
                    isl.progress(id, None, "", None);
                    glib::ControlFlow::Continue
                }
            });
            let res = gio::spawn_blocking(move || job(cancel)).await;
            pulse.remove();
            this.0.island.finish(id, Outcome::Quiet);
            this.settled();
            match res {
                Ok(r) => done(r),
                Err(_) => this.toast("A background task stopped unexpectedly"),
            }
        });
    }

    pub fn submit(&self, req: OpRequest) {
        let started = std::time::Instant::now();
        let (ctl, rx) = operations::spawn(req.clone());
        let c = ctl.clone();
        let pause: Option<PauseFn> = matches!(req.kind, OpKind::Copy | OpKind::Move | OpKind::Delete).then(|| {
            let c = ctl.clone();
            Rc::new(move |on| c.set_paused(on)) as PauseFn
        });
        let id = self.0.island.add(&op_title(&req), Rc::new(move || c.cancel()), pause);
        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                match ev {
                    Event::Progress(p) => {
                        let known = p.total_bytes > 0 || p.items_total > 0;
                        let bytes = (p.total_bytes > 0).then_some((p.done_bytes, p.total_bytes));
                        this.0.island.progress(id, known.then(|| p.fraction()), &status_text(&p), bytes);
                    }
                    Event::Conflict { src, dst, reply } => {
                        this.0.conflicts.borrow_mut().push_back(PendingConflict { src, dst, reply });
                        this.pump_conflicts();
                    }
                    Event::Finished(s) => {
                        this.finished(id, &req, s, started.elapsed());
                        return;
                    }
                }
            }
            // The worker went away without reporting.
            this.0.island.finish(
                id,
                Outcome::Failed {
                    title: format!("{} stopped", req.kind.verb()),
                    detail: "The operation ended unexpectedly.".into(),
                    retry: None,
                },
            );
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

    fn finished(&self, id: u64, req: &OpRequest, s: Summary, elapsed: Duration) {
        self.record(req, &s);
        self.settled();
        self.notify(req, &s, elapsed);
        let ok = req.sources.len() - s.failed_sources.len().min(req.sources.len());
        if s.errors.is_empty() {
            let outcome = if s.cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Done(format!("{} {}", req.kind.past(), plural(ok)))
            };
            self.0.island.finish(id, outcome);
            return;
        }
        let first = &s.errors[0];
        let name =
            gio::File::for_uri(&first.uri).basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut detail = format!("{name}: {}", first.message);
        if s.errors.len() > 1 {
            detail.push_str(&format!("\nand {} more", plural(s.errors.len() - 1)));
        }
        let retry = (!s.failed_sources.is_empty() && !s.cancelled).then(|| {
            let (this, r) = (self.clone(), OpRequest { sources: s.failed_sources.clone(), ..req.clone() });
            Rc::new(move || this.submit(r.clone())) as Rc<dyn Fn()>
        });
        let title = if s.errors.len() == 1 {
            format!("{} failed for 1 item", req.kind.verb())
        } else {
            format!("{} failed for {} items", req.kind.verb(), s.errors.len())
        };
        self.0.island.finish(id, Outcome::Failed { title, detail, retry });
    }
}
