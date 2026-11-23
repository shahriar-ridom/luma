//! A small audio player for previews: cover art over a blurred copy of itself,
//! title and artist, a seek bar and play controls.
//!
//! Playback runs on GStreamer's `playbin` (decodebin2), not on GTK's media
//! backend. GTK plays through `playbin3`, and with GStreamer 1.28 its
//! decodebin3 aborts the whole process (`mq_slot_handle_stream_start:
//! assertion failed: (collection)`) when an MP3 stream is seeked or restarted.
//! A plain `playbin` never reaches that code.
//!
//! Everything runs on the main thread: the bus is watched from the main
//! context and the position is polled with a timeout. Tags and the cover are
//! read off-thread. Every `load` bumps a token so late results for an earlier
//! file are dropped.

use adw::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
    time::Duration,
};

use crate::application::icons::themed;
use crate::core::metadata::advanced::{AudioInfo, audio_info};
use crate::services::thumbnail;

/// How far the skip buttons jump.
const SKIP: Duration = Duration::from_secs(10);
/// Position refresh while playing.
const TICK: Duration = Duration::from_millis(200);

pub struct AudioPlayer {
    pub widget: gtk::Overlay,
    ambient: gtk::Picture,
    cover: gtk::Picture,
    cover_stack: gtk::Stack,
    title: gtk::Label,
    subtitle: gtk::Label,
    scale: gtk::Scale,
    elapsed: gtk::Label,
    remaining: gtk::Label,
    play: gtk::Button,
    controls: gtk::Box,
    cover_px: i32,
    pipeline: RefCell<Option<gst::Element>>,
    bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    tick: RefCell<Option<glib::SourceId>>,
    token: Cell<u64>,
    duration: Cell<Option<gst::ClockTime>>,
    playing: Cell<bool>,
}

fn fmt_time(t: gst::ClockTime) -> String {
    let s = t.seconds();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn icon_button(icon: &'static str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(themed(icon));
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.add_css_class("fm-player-skip");
    b.set_tooltip_text(Some(tip));
    b.set_valign(gtk::Align::Center);
    b
}

impl AudioPlayer {
    /// `cover_px`: edge of the cover square (Quick Look uses a large one, the
    /// side preview a small one).
    pub fn new(cover_px: i32) -> Rc<Self> {
        // Ambient backdrop: the cover again, blurred and scaled past the edges.
        let ambient = gtk::Picture::new();
        ambient.set_content_fit(gtk::ContentFit::Cover);
        ambient.set_can_shrink(true);
        ambient.add_css_class("fm-player-ambient");
        let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scrim.add_css_class("fm-player-scrim");
        let backdrop = gtk::Overlay::new();
        backdrop.set_child(Some(&ambient));
        backdrop.add_overlay(&scrim);
        backdrop.set_overflow(gtk::Overflow::Hidden);

        let cover = gtk::Picture::new();
        cover.set_content_fit(gtk::ContentFit::Cover);
        cover.set_can_shrink(true);
        let placeholder = gtk::Image::from_icon_name(themed("luma-music-symbolic"));
        placeholder.set_pixel_size(cover_px / 3);
        placeholder.add_css_class("fm-player-glyph");
        let cover_stack = gtk::Stack::new();
        cover_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        cover_stack.set_transition_duration(180);
        cover_stack.add_named(&placeholder, Some("none"));
        cover_stack.add_named(&cover, Some("art"));
        cover_stack.set_overflow(gtk::Overflow::Hidden);
        cover_stack.add_css_class("fm-player-cover");
        // Always a square of at most `cover_px`, whatever the art's shape.
        let square = gtk::AspectFrame::new(0.5, 0.5, 1.0, false);
        square.set_child(Some(&cover_stack));
        let cover_box =
            adw::Clamp::builder().maximum_size(cover_px).tightening_threshold(cover_px).child(&square).build();

        let title = gtk::Label::new(None);
        title.add_css_class("fm-player-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(28);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("fm-player-sub");
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.set_max_width_chars(34);

        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
        scale.set_draw_value(false);
        scale.set_hexpand(true);
        scale.add_css_class("fm-player-seek");
        scale.update_property(&[gtk::accessible::Property::Label("Playback position")]);
        let elapsed = gtk::Label::new(Some("0:00"));
        let remaining = gtk::Label::new(Some("0:00"));
        for l in [&elapsed, &remaining] {
            l.add_css_class("fm-player-time");
        }
        elapsed.set_halign(gtk::Align::Start);
        elapsed.set_hexpand(true);
        remaining.set_halign(gtk::Align::End);
        let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        times.append(&elapsed);
        times.append(&remaining);
        let seek = gtk::Box::new(gtk::Orientation::Vertical, 0);
        seek.append(&scale);
        seek.append(&times);

        let back = icon_button("luma-seek-backward-symbolic", "Back 10 seconds");
        let fwd = icon_button("luma-seek-forward-symbolic", "Forward 10 seconds");
        let play = gtk::Button::from_icon_name(themed("luma-play-symbolic"));
        play.add_css_class("circular");
        play.add_css_class("fm-player-play");
        play.set_tooltip_text(Some("Play"));
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        controls.set_halign(gtk::Align::Center);
        controls.append(&back);
        controls.append(&play);
        controls.append(&fwd);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.append(&title);
        text.append(&subtitle);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
        column.set_valign(gtk::Align::Center);
        column.set_margin_top(28);
        column.set_margin_bottom(24);
        column.set_margin_start(28);
        column.set_margin_end(28);
        column.set_width_request(cover_px.min(300));
        column.append(&cover_box);
        column.append(&text);
        column.append(&seek);
        column.append(&controls);

        let widget = gtk::Overlay::new();
        widget.add_css_class("fm-player");
        widget.set_child(Some(&backdrop));
        widget.add_overlay(&column);
        widget.set_measure_overlay(&column, true);

        let this = Rc::new(Self {
            widget,
            ambient,
            cover,
            cover_stack,
            title,
            subtitle,
            scale,
            elapsed,
            remaining,
            play,
            controls,
            cover_px,
            pipeline: RefCell::default(),
            bus_watch: RefCell::default(),
            tick: RefCell::default(),
            token: Cell::new(0),
            duration: Cell::new(None),
            playing: Cell::new(false),
        });

        let w = Rc::downgrade(&this);
        this.play.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.toggle();
            }
        });
        let w = Rc::downgrade(&this);
        back.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.skip(false);
            }
        });
        let w = Rc::downgrade(&this);
        fwd.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.skip(true);
            }
        });
        // Only user changes seek; the position poll sets the value directly.
        let w = Rc::downgrade(&this);
        this.scale.connect_change_value(move |_, _, v| {
            if let Some(p) = w.upgrade() {
                p.seek_to(gst::ClockTime::from_nseconds((v.max(0.0) * 1e9) as u64));
            }
            glib::Propagation::Proceed
        });
        this
    }

    /// Show `path` and start playing it.
    pub fn load(self: &Rc<Self>, path: &Path) {
        self.stop();
        let id = self.token.get() + 1;
        self.token.set(id);
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.show_info(&AudioInfo::default(), &name);
        self.set_cover(None);

        // Tags and cover off the main thread.
        let (p, w, px) = (path.to_path_buf(), Rc::downgrade(self), self.cover_px * 2);
        glib::spawn_future_local(async move {
            let info = gio::spawn_blocking(move || audio_info(&p)).await.ok().flatten();
            let Some(this) = w.upgrade().filter(|t| t.token.get() == id) else { return };
            let Some(info) = info else { return };
            this.show_info(&info, &name);
            if this.duration.get().is_none() && !info.duration.is_zero() {
                this.set_duration(gst::ClockTime::from_nseconds(info.duration.as_nanos() as u64));
            }
            if let Some(bytes) = info.cover {
                let tex = thumbnail::texture_from_bytes(bytes, px).await;
                if let Some(this) = w.upgrade().filter(|t| t.token.get() == id) {
                    this.set_cover(tex.as_ref());
                }
            }
        });

        if let Err(e) = self.start(path, id) {
            tracing::warn!("audio preview: {e}");
            self.fail("Can't play this file");
        }
    }

    fn start(self: &Rc<Self>, path: &Path, id: u64) -> anyhow::Result<()> {
        gst::init()?;
        let uri = gio::File::for_path(path).uri();
        let pipeline = gst::ElementFactory::make("playbin").property("uri", uri.as_str()).build()?;
        // Audio only: no video sink or visualisation for the cover-art stream.
        pipeline.set_property_from_str("flags", "audio+soft-volume");
        let bus = pipeline.bus().ok_or_else(|| anyhow::anyhow!("pipeline has no bus"))?;
        let w = Rc::downgrade(self);
        let watch = bus.add_watch_local(move |_, msg| {
            let Some(p) = w.upgrade().filter(|p| p.token.get() == id) else { return glib::ControlFlow::Break };
            match msg.view() {
                gst::MessageView::Eos(_) => p.finished(),
                gst::MessageView::Error(e) => {
                    tracing::warn!("audio preview: {} ({:?})", e.error(), e.debug());
                    p.fail("Can't play this file");
                }
                gst::MessageView::DurationChanged(_) => p.refresh_duration(),
                gst::MessageView::AsyncDone(_) => p.refresh_duration(),
                _ => {}
            }
            glib::ControlFlow::Continue
        })?;
        *self.bus_watch.borrow_mut() = Some(watch);
        pipeline.set_state(gst::State::Playing)?;
        *self.pipeline.borrow_mut() = Some(pipeline);
        self.controls.set_sensitive(true);
        self.scale.set_sensitive(true);
        self.set_playing(true);
        Ok(())
    }

    /// Stop playback and release the pipeline.
    pub fn stop(&self) {
        self.token.set(self.token.get() + 1);
        if let Some(id) = self.tick.take() {
            id.remove();
        }
        self.bus_watch.take();
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.duration.set(None);
        self.playing.set(false);
        self.scale.set_range(0.0, 1.0);
        self.scale.set_value(0.0);
        self.elapsed.set_text("0:00");
        self.remaining.set_text("0:00");
    }

    pub fn toggle(self: &Rc<Self>) {
        let Some(p) = self.pipeline.borrow().clone() else { return };
        let next = !self.playing.get();
        let _ = p.set_state(if next { gst::State::Playing } else { gst::State::Paused });
        self.set_playing(next);
    }

    fn skip(&self, forward: bool) {
        let Some(p) = self.pipeline.borrow().clone() else { return };
        let Some(pos) = p.query_position::<gst::ClockTime>() else { return };
        let step = gst::ClockTime::from_nseconds(SKIP.as_nanos() as u64);
        let to = if forward { pos + step } else { pos.saturating_sub(step) };
        let to = self.duration.get().map_or(to, |d| to.min(d.saturating_sub(gst::ClockTime::from_mseconds(250))));
        self.seek_to(to);
    }

    fn seek_to(&self, t: gst::ClockTime) {
        let Some(p) = self.pipeline.borrow().clone() else { return };
        let _ = p.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, t);
        self.show_position(t);
    }

    fn set_playing(self: &Rc<Self>, playing: bool) {
        self.playing.set(playing);
        let (icon, tip) = if playing { ("luma-pause-symbolic", "Pause") } else { ("luma-play-symbolic", "Play") };
        self.play.set_icon_name(themed(icon));
        self.play.set_tooltip_text(Some(tip));
        if playing && self.tick.borrow().is_none() {
            let w = Rc::downgrade(self);
            let id = glib::timeout_add_local(TICK, move || {
                let Some(p) = w.upgrade() else { return glib::ControlFlow::Break };
                let pos = p.pipeline.borrow().as_ref().and_then(|pl| pl.query_position::<gst::ClockTime>());
                if let Some(pos) = pos {
                    p.show_position(pos);
                }
                glib::ControlFlow::Continue
            });
            *self.tick.borrow_mut() = Some(id);
        } else if !playing && let Some(id) = self.tick.take() {
            id.remove();
        }
    }

    fn refresh_duration(&self) {
        let d = self.pipeline.borrow().as_ref().and_then(|p| p.query_duration::<gst::ClockTime>());
        if let Some(d) = d.filter(|d| d.nseconds() > 0) {
            self.set_duration(d);
        }
    }

    fn set_duration(&self, d: gst::ClockTime) {
        self.duration.set(Some(d));
        self.scale.set_range(0.0, d.nseconds() as f64 / 1e9);
        self.show_position(gst::ClockTime::from_nseconds((self.scale.value() * 1e9) as u64));
    }

    fn show_position(&self, pos: gst::ClockTime) {
        self.scale.set_value(pos.nseconds() as f64 / 1e9);
        self.elapsed.set_text(&fmt_time(pos));
        if let Some(d) = self.duration.get() {
            self.remaining.set_text(&format!("−{}", fmt_time(d.saturating_sub(pos))));
        }
    }

    /// End of track: back to the start, paused, ready to play again.
    fn finished(self: &Rc<Self>) {
        if let Some(p) = self.pipeline.borrow().clone() {
            let _ = p.set_state(gst::State::Paused);
            let _ = p.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, gst::ClockTime::ZERO);
        }
        self.set_playing(false);
        self.show_position(gst::ClockTime::ZERO);
    }

    fn fail(&self, why: &str) {
        if let Some(id) = self.tick.take() {
            id.remove();
        }
        self.bus_watch.take();
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.subtitle.set_text(why);
        self.controls.set_sensitive(false);
        self.scale.set_sensitive(false);
    }

    fn show_info(&self, info: &AudioInfo, file_name: &str) {
        self.title.set_text(info.title.as_deref().unwrap_or(file_name));
        let sub = match (&info.artist, &info.album) {
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => a.clone(),
            (Some(a), Some(b)) => format!("{a} — {b}"),
            (Some(a), None) => a.clone(),
            (None, Some(b)) => b.clone(),
            (None, None) => String::new(),
        };
        self.subtitle.set_text(&sub);
        self.subtitle.set_visible(!sub.is_empty());
    }

    fn set_cover(&self, tex: Option<&gtk::gdk::Texture>) {
        self.cover.set_paintable(tex);
        self.ambient.set_paintable(tex);
        self.cover_stack.set_visible_child_name(if tex.is_some() { "art" } else { "none" });
        self.widget.remove_css_class("has-art");
        if tex.is_some() {
            self.widget.add_css_class("has-art");
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_format_like_a_player() {
        assert_eq!(fmt_time(gst::ClockTime::from_seconds(0)), "0:00");
        assert_eq!(fmt_time(gst::ClockTime::from_seconds(75)), "1:15");
        assert_eq!(fmt_time(gst::ClockTime::from_seconds(3725)), "1:02:05");
    }
}
