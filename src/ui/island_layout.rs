//! Places the activity island over the header bar and makes room for it.
//!
//! The island floats in an overlay, centred on the window and vertically on
//! the header. When it needs more room than the free space between the
//! header's left and right controls, the controls make room in steps:
//! 1. every control shrinks a little about its centre and slides toward its
//!    window edge, closing up the spacing;
//! 2. the filter field also scales away to nothing;
//! 3. (an opened card needs the most room) controls shrink further and the
//!    view switcher scales away too, until the card closes.
//!
//! Smoothness: nothing is re-laid out while it moves. Making room is done
//! with CSS transforms (translate + scale), which GTK interpolates at
//! sub-pixel precision on the frame clock, instead of changing sizes or
//! margins (which re-lays out the header every frame and rounds each button
//! to whole pixels, so the icons wobble). The transforms are computed from
//! the controls' real widths, so the room each step frees is known exactly
//! and the step is chosen at once, with no measuring mid-animation. The
//! island's own width and position follow a critically damped spring timed
//! like the CSS transition, so the pill and the controls move together.

use gtk::{gdk, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    fmt::Write,
    rc::Rc,
};

use crate::ui::island::Island;

/// Space kept clear on each side of the island.
const MARGIN: f64 = 10.0;
/// Extra room needed before relaxing a step, so it doesn't flicker.
const HYSTERESIS: f64 = 16.0;
const MAX_TIER: usize = 3;
/// Scale of the controls at each step.
const SHRINK: [f64; MAX_TIER + 1] = [1.0, 0.86, 0.86, 0.76];
/// Share of the spacing between controls closed up at each step.
const CLOSE_UP: [f64; MAX_TIER + 1] = [0.0, 0.6, 0.6, 1.0];
/// Spring stiffness (rad/s); critically damped, settles in ~280 ms, like
/// the CSS transition on the controls.
const OMEGA: f64 = 20.0;

#[derive(Default)]
struct Spring {
    pos: Cell<f64>,
    vel: Cell<f64>,
    target: Cell<f64>,
}

impl Spring {
    fn snap(&self, v: f64) {
        self.pos.set(v);
        self.vel.set(0.0);
        self.target.set(v);
    }
    /// Advance by `dt`; returns true while still moving.
    fn step(&self, dt: f64) -> bool {
        // Critically damped: x'' = ω²(t − x) − 2ω x'. Substeps keep it stable.
        let n = (dt / 0.004).ceil().max(1.0) as usize;
        let h = dt / n as f64;
        let (mut x, mut v, t) = (self.pos.get(), self.vel.get(), self.target.get());
        for _ in 0..n {
            let a = OMEGA * OMEGA * (t - x) - 2.0 * OMEGA * v;
            v += a * h;
            x += v * h;
        }
        let moving = (t - x).abs() > 0.2 || v.abs() > 1.5;
        if moving {
            self.pos.set(x);
            self.vel.set(v);
        } else {
            self.snap(t);
        }
        moving
    }
}

/// One side of the header: its container and its controls.
struct Side {
    container: gtk::Widget,
    widgets: Vec<gtk::Widget>,
    /// -1 moves toward the left edge, +1 toward the right.
    sign: f64,
}

/// Where each control goes at one step, and the room that frees.
#[derive(Default, Clone, PartialEq)]
struct Plan {
    moves: Vec<(String, f64, f64)>,
    gain_left: f64,
    gain_right: f64,
}

struct Fit {
    layer: gtk::Overlay,
    island: Rc<Island>,
    header: gtk::Widget,
    left: Side,
    right: Side,
    /// Controls that scale away entirely, and from which step.
    collapse: Vec<(gtk::Widget, usize)>,
    tier: Cell<usize>,
    plans: RefCell<Vec<Plan>>,
    css: gtk::CssProvider,
    css_text: RefCell<String>,
    x: Spring,
    w: Spring,
    placed: Cell<bool>,
    last_frame: Cell<i64>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    layer_width: Cell<i32>,
    decide_queued: Cell<bool>,
    /// Natural width the targets were computed for. Style changes (a bolder
    /// font for a new state) only show up in later measurements, so a
    /// mismatch at allocation time triggers a fresh decision.
    planned_for: Cell<i32>,
}

fn spacing(container: &gtk::Widget) -> f64 {
    container.downcast_ref::<gtk::Box>().map(|b| b.spacing() as f64).unwrap_or(6.0)
}

impl Fit {
    /// Controls of one side that take part, from the window edge inward.
    fn ordered(&self, side: &Side) -> Vec<gtk::Widget> {
        let mut v: Vec<gtk::Widget> =
            side.widgets.iter().filter(|w| w.is_visible() && w.width() > 0).cloned().collect();
        // Position in the container; the sort only needs the order, which
        // the transforms never change.
        let x = |w: &gtk::Widget| {
            w.compute_point(&side.container, &gtk::graphene::Point::zero()).map(|p| p.x()).unwrap_or(0.0)
        };
        v.sort_by(|a, b| if side.sign < 0.0 { x(a).total_cmp(&x(b)) } else { x(b).total_cmp(&x(a)) });
        v
    }

    /// Transforms for `tier` on one side; returns the room gained.
    fn plan_side(&self, side: &Side, tier: usize, moves: &mut Vec<(String, f64, f64)>) -> f64 {
        let gap = spacing(&side.container);
        let mut cum = 0.0;
        for (i, w) in self.ordered(side).iter().enumerate() {
            let width = w.width() as f64;
            let collapse = self.collapse.iter().any(|(c, from)| c == w && tier >= *from);
            let s = if collapse { 0.0 } else { SHRINK[tier] };
            let cut = match (i, collapse) {
                (0, _) => 0.0,
                (_, true) => gap,
                _ => gap * CLOSE_UP[tier],
            };
            let shift = cum + cut + width * (1.0 - s) / 2.0;
            cum += cut + width * (1.0 - s);
            moves.push((w.widget_name().to_string(), side.sign * shift, s));
        }
        cum
    }

    fn plan(&self, tier: usize) -> Plan {
        let mut moves = Vec::new();
        let gain_left = self.plan_side(&self.left, tier, &mut moves);
        let gain_right = self.plan_side(&self.right, tier, &mut moves);
        Plan { moves, gain_left, gain_right }
    }

    /// Recompute the plans for the current widths; reload the CSS only
    /// when they actually changed (rare: first layout, breakpoints).
    fn replan(&self) {
        let plans: Vec<Plan> = (0..=MAX_TIER).map(|t| self.plan(t)).collect();
        let mut css = String::new();
        for (t, p) in plans.iter().enumerate().skip(1) {
            for (name, dx, s) in &p.moves {
                let fade = if *s == 0.0 { " opacity: 0;" } else { "" };
                let _ = writeln!(
                    css,
                    "headerbar.fm-tier{t} #{name} {{ transform: translateX({dx:.2}px) scale({s:.3});{fade} }}"
                );
            }
        }
        if *self.css_text.borrow() != css {
            self.css.load_from_string(&css);
            *self.css_text.borrow_mut() = css;
        }
        *self.plans.borrow_mut() = plans;
    }

    /// Free space at `tier`: the untransformed gap plus what the step frees.
    fn gap(&self, tier: usize) -> Option<(f64, f64)> {
        let l = self.left.container.compute_bounds(&self.layer)?;
        let r = self.right.container.compute_bounds(&self.layer)?;
        let (l, r) = ((l.x() + l.width()) as f64, r.x() as f64);
        let plans = self.plans.borrow();
        let p = plans.get(tier)?;
        Some((l - p.gain_left + MARGIN, r + p.gain_right - MARGIN))
    }

    fn apply(&self, tier: usize) {
        self.tier.set(tier);
        for t in 1..=MAX_TIER {
            let class = format!("fm-tier{t}");
            if t == tier {
                self.header.add_css_class(&class);
            } else {
                self.header.remove_css_class(&class);
            }
        }
        // Scaled-away controls shouldn't take clicks or focus.
        for (w, from) in &self.collapse {
            let gone = tier >= *from;
            w.set_can_target(!gone);
            w.set_can_focus(!gone);
        }
    }

    /// Choose the step for the island's current size and aim the springs.
    /// Runs synchronously so the controls start moving in the same frame as
    /// the island.
    fn decide(self: &Rc<Self>) {
        self.decide_queued.set(false);
        if self.layer.width() <= 0 {
            return;
        }
        self.replan();
        let want = self.island.natural_width();
        self.planned_for.set(want);
        let want = want as f64;
        let t = self.tier.get();
        let mut pick = MAX_TIER;
        for i in 0..=MAX_TIER {
            let Some((l, r)) = self.gap(i) else { continue };
            let need = if i < t { want + HYSTERESIS } else { want };
            if r - l >= need {
                pick = i;
                break;
            }
        }
        if pick != t {
            self.apply(pick);
        }
        self.retarget();
    }

    /// Point the springs at where the island will end up.
    fn retarget(self: &Rc<Self>) {
        let lw = self.layer.width() as f64;
        let Some((l, r)) = self.gap(self.tier.get()) else { return };
        let want = self.island.natural_width() as f64;
        let expanded = self.island.is_expanded();
        let w = if expanded { want.min(lw - 16.0) } else { want.min((r - l).max(40.0)) };
        let centred = (lw - w) / 2.0;
        // Inside the free space whenever it fits (as close to the centre as
        // that allows); only a card wider than any step's space may overlap.
        let x = if w <= r - l { centred.clamp(l, r - w) } else { centred.clamp(8.0, (lw - w - 8.0).max(8.0)) };
        self.x.target.set(x);
        self.w.target.set(w);
        if !self.placed.replace(true) {
            self.x.snap(x);
            self.w.snap(w);
        }
        self.animate();
    }

    fn animate(self: &Rc<Self>) {
        if self.tick.borrow().is_some() {
            return;
        }
        self.last_frame.set(0);
        let me = Rc::downgrade(self);
        let id = self.layer.add_tick_callback(move |layer, clock| {
            let Some(f) = me.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let dt = match f.last_frame.replace(now) {
                0 => 1.0 / 60.0,
                prev => ((now - prev) as f64 / 1e6).clamp(0.0, 0.05),
            };
            let moving = f.x.step(dt) | f.w.step(dt);
            layer.queue_allocate();
            if !moving {
                f.tick.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(id);
    }

    fn queue_decide(self: &Rc<Self>) {
        if self.decide_queued.replace(true) {
            return;
        }
        let me = self.clone();
        glib::idle_add_local_once(move || me.decide());
    }

    fn rect(&self, child: &gtk::Widget) -> gdk::Rectangle {
        let (min_w, _, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
        // Even widths keep centred content on whole pixels, so text inside
        // doesn't shimmer by half a pixel as the width changes.
        let w = ((self.w.pos.get() / 2.0).round() as i32 * 2).max(min_w).max(2);
        // Hold the centre steady; derive the left edge from it.
        let cx = self.x.pos.get() + self.w.pos.get() / 2.0;
        let x = (cx - w as f64 / 2.0).round() as i32;
        let h = child.measure(gtk::Orientation::Vertical, w).1;
        // Centre the compact pill on the header; an opened card keeps its
        // top row there and grows downward.
        let y = self
            .header
            .compute_bounds(&self.layer)
            .map(|b| (b.y() + (b.height() - self.island.pill_height() as f32) / 2.0).round() as i32)
            .unwrap_or(8);
        gdk::Rectangle::new(x, y.max(0), w, h)
    }
}

/// Put `island` over `content` (which holds `header`) and keep the header's
/// controls out of its way. `left`/`right` are the controls packed at each
/// end of the header; `collapse` lists those that may scale away entirely,
/// with the step from which they do.
pub fn install(
    content: &impl IsA<gtk::Widget>,
    island: &Rc<Island>,
    header: &impl IsA<gtk::Widget>,
    left: Vec<gtk::Widget>,
    right: Vec<gtk::Widget>,
    collapse: Vec<(gtk::Widget, usize)>,
) -> gtk::Overlay {
    let layer = gtk::Overlay::new();
    layer.set_child(Some(content));
    layer.add_overlay(&island.widget);

    // Unique names so the generated CSS can address each control; a shared
    // class carries the transition.
    for (i, w) in left.iter().chain(right.iter()).enumerate() {
        w.set_widget_name(&format!("fm-hdr-{i}"));
        w.add_css_class("fm-hdr-item");
    }
    let container = |ws: &[gtk::Widget]| ws.first().and_then(|w| w.parent()).expect("header control is packed");
    let left = Side { container: container(&left), widgets: left, sign: -1.0 };
    let right = Side { container: container(&right), widgets: right, sign: 1.0 };

    let css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &WidgetExt::display(&layer),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    let fit = Rc::new(Fit {
        layer: layer.clone(),
        island: island.clone(),
        header: header.clone().upcast(),
        left,
        right,
        collapse,
        tier: Cell::new(0),
        plans: RefCell::default(),
        css,
        css_text: RefCell::default(),
        x: Spring::default(),
        w: Spring::default(),
        placed: Cell::new(false),
        last_frame: Cell::new(0),
        tick: RefCell::default(),
        layer_width: Cell::new(-1),
        decide_queued: Cell::new(false),
        planned_for: Cell::new(-1),
    });
    let f = fit.clone();
    layer.connect_get_child_position(move |layer, child| {
        if child != &f.island.widget {
            return None;
        }
        if f.layer_width.replace(layer.width()) != layer.width() {
            // Window resize: follow at once (no spring lag), then re-decide.
            f.placed.set(false);
            f.queue_decide();
        } else if f.island.natural_width() != f.planned_for.get() {
            f.queue_decide();
        }
        Some(f.rect(child))
    });
    let f = fit.clone();
    island.set_on_resize(move || f.decide());
    layer
}
