//! macOS-style Quick Look: a bezel-less floating window previewing the selection.
//! Space, Esc or a click outside dismisses it; arrow keys move the selection underneath while
//! it stays open; Return opens the file.
//!
//! The content fills the whole window and the window takes the content's shape:
//!   image  the picture, edge to edge, over a blurred copy of itself
//!   video  the video, edge to edge on black, sized to its aspect ratio
//!   audio  a compact player with the cover art (see `audio_player`)
//!   text   a readable document column
//!   archive its contents as a tree, with file and folder totals
//!   other  the file's icon or thumbnail with its name, kind and size
//! A floating bar with the name, Open and Close sits on top; over images and
//! video it fades out while the pointer rests and comes back when it moves.
//! F or a double-click plays a video full screen.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::application::icons::themed;
use crate::core::filesystem::{human_size, is_dir};
use crate::services::thumbnail;
use crate::ui::archive_preview::ArchivePreview;
use crate::ui::audio_player::AudioPlayer;
use crate::ui::preview::{Target, count_children, is_textual, read_head, render_markdown};

type StepFn = Rc<dyn Fn(i32)>;
type OpenFn = Rc<dyn Fn()>;

const TEXT_CAP: i32 = 64 * 1024;
const IMAGE_PX: i32 = 2048;
/// The bar hides after this long without pointer movement (images and video).
const BAR_IDLE: Duration = Duration::from_millis(1800);
/// Largest window Quick Look opens for media, and the smallest.
const MAX_W: i32 = 1100;
const MAX_H: i32 = 740;
const MIN_W: i32 = 360;
const MIN_H: i32 = 260;
/// Longest Quick Look waits for media to report its size before it opens anyway.
const SETTLE_WAIT: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Image,
    Video,
    Audio,
    Text,
    Archive,
    Info,
}

impl Kind {
    fn page(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Text => "text",
            Kind::Archive => "archive",
            Kind::Info => "info",
        }
    }

    /// Media fills the window, so the bar gets out of the way.
    fn immersive(self) -> bool {
        matches!(self, Kind::Image | Kind::Video)
    }
}

/// Fit `(w, h)` inside the media bounds, keeping its aspect ratio.
fn fit(w: i32, h: i32) -> (i32, i32) {
    if w <= 0 || h <= 0 {
        return (720, 520);
    }
    let s = (MAX_W as f64 / w as f64).min(MAX_H as f64 / h as f64).min(1.0);
    let (w, h) = ((w as f64 * s).round() as i32, (h as f64 * s).round() as i32);
    (w.max(MIN_W), h.max(MIN_H))
}

pub struct QuickLook {
    window: adw::Window,
    root: gtk::Overlay,
    pages: gtk::Stack,
    // image
    picture: gtk::Picture,
    ambient: gtk::Picture,
    // video
    video: gtk::Video,
    // audio
    audio: Rc<AudioPlayer>,
    // text
    text: gtk::TextView,
    // info
    info_icon: gtk::Image,
    info_thumb: gtk::Picture,
    info_icon_stack: gtk::Stack,
    info_name: gtk::Label,
    info_kind: gtk::Label,
    info_detail: gtk::Label,
    archive: Rc<ArchivePreview>,
    // chrome
    bar: gtk::Revealer,
    title: gtk::Label,
    subtitle: gtk::Label,
    bar_timer: RefCell<Option<glib::SourceId>>,
    kind: Cell<Kind>,
    /// The window should open as soon as the content has its size.
    pending_present: Cell<bool>,
    /// The content's size is known (and requested) for the current file.
    settled: Cell<bool>,
    current: RefCell<Option<PathBuf>>,
    token: Cell<u64>,
    on_step: RefCell<Option<StepFn>>,
    on_open: RefCell<Option<OpenFn>>,
    /// When a click outside last closed the window.
    blurred_at: Cell<Option<Instant>>,
}

impl QuickLook {
    pub fn new(parent: &gtk::Window) -> Rc<Self> {
        // Image: the picture over a blurred, enlarged copy of itself.
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        let ambient = gtk::Picture::new();
        ambient.set_content_fit(gtk::ContentFit::Cover);
        ambient.set_can_shrink(true);
        ambient.add_css_class("fm-ql-ambient");
        let image_page = gtk::Overlay::new();
        image_page.set_child(Some(&ambient));
        image_page.add_overlay(&picture);
        image_page.set_overflow(gtk::Overflow::Hidden);
        image_page.add_css_class("fm-ql-image");

        let video = gtk::Video::new();
        video.set_autoplay(true);
        video.add_css_class("fm-ql-video");

        let audio = AudioPlayer::new(240);

        let text = gtk::TextView::new();
        text.set_editable(false);
        text.set_cursor_visible(false);
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.set_left_margin(32);
        text.set_right_margin(32);
        text.set_top_margin(64);
        text.set_bottom_margin(28);
        text.set_pixels_below_lines(2);
        text.add_css_class("fm-ql-text");
        // A readable measure however wide the window is.
        let text_col = adw::Clamp::builder().maximum_size(760).tightening_threshold(600).child(&text).build();
        let text_sw = gtk::ScrolledWindow::new();
        text_sw.set_child(Some(&text_col));
        text_sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

        let info_icon = gtk::Image::new();
        info_icon.set_pixel_size(128);
        info_icon.add_css_class("fm-ql-icon");
        let info_thumb = gtk::Picture::new();
        info_thumb.set_can_shrink(true);
        info_thumb.set_content_fit(gtk::ContentFit::Contain);
        info_thumb.set_size_request(220, 220);
        info_thumb.add_css_class("fm-thumb");
        let info_icon_stack = gtk::Stack::new();
        info_icon_stack.add_named(&info_icon, Some("icon"));
        info_icon_stack.add_named(&info_thumb, Some("thumb"));
        let info_name = gtk::Label::new(None);
        info_name.add_css_class("fm-ql-name");
        info_name.set_wrap(true);
        info_name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        info_name.set_justify(gtk::Justification::Center);
        info_name.set_max_width_chars(36);
        let info_kind = gtk::Label::new(None);
        info_kind.add_css_class("fm-ql-meta");
        let info_detail = gtk::Label::new(None);
        info_detail.add_css_class("fm-ql-meta");
        let info_page = gtk::Box::new(gtk::Orientation::Vertical, 6);
        info_page.set_valign(gtk::Align::Center);
        info_page.set_halign(gtk::Align::Center);
        info_page.set_margin_top(56);
        info_page.set_margin_bottom(32);
        info_page.set_margin_start(32);
        info_page.set_margin_end(32);
        info_icon_stack.set_margin_bottom(12);
        info_page.append(&info_icon_stack);
        info_page.append(&info_name);
        info_page.append(&info_kind);
        info_page.append(&info_detail);

        let pages = gtk::Stack::new();
        pages.set_transition_type(gtk::StackTransitionType::Crossfade);
        pages.set_transition_duration(160);
        pages.add_named(&image_page, Some("image"));
        pages.add_named(&video, Some("video"));
        pages.add_named(&audio.widget, Some("audio"));
        pages.add_named(&text_sw, Some("text"));
        pages.add_named(&info_page, Some("info"));
        let archive = ArchivePreview::new();
        pages.add_named(&archive.widget, Some("archive"));
        pages.set_hexpand(true);
        pages.set_vexpand(true);

        // The floating bar.
        let close = gtk::Button::from_icon_name(themed("luma-close-symbolic"));
        close.add_css_class("flat");
        close.add_css_class("circular");
        close.set_tooltip_text(Some("Close (Space)"));
        let open = gtk::Button::from_icon_name(themed("luma-open-external-symbolic"));
        open.add_css_class("flat");
        open.add_css_class("circular");
        open.set_tooltip_text(Some("Open (Return)"));
        let title = gtk::Label::new(None);
        title.add_css_class("fm-ql-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_max_width_chars(48);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("fm-ql-sub");
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.append(&title);
        titles.append(&subtitle);
        let bar_box = gtk::CenterBox::new();
        bar_box.add_css_class("fm-ql-bar");
        bar_box.set_start_widget(Some(&close));
        bar_box.set_center_widget(Some(&titles));
        bar_box.set_end_widget(Some(&open));
        let bar = gtk::Revealer::new();
        bar.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        bar.set_transition_duration(220);
        bar.set_child(Some(&bar_box));
        bar.set_valign(gtk::Align::Start);
        bar.set_reveal_child(true);
        bar.set_can_target(true);

        let root = gtk::Overlay::new();
        root.add_css_class("fm-ql");
        root.set_child(Some(&pages));
        root.add_overlay(&bar);
        // No title bar: the whole surface moves the window.
        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&root));

        let window = adw::Window::builder()
            .transient_for(parent)
            .destroy_with_parent(true)
            .hide_on_close(true)
            .title("Quick Look")
            .default_width(720)
            .default_height(520)
            .content(&handle)
            .build();
        window.add_css_class("fm-quick-look");

        let this = Rc::new(Self {
            window,
            root,
            pages,
            picture,
            ambient,
            video,
            audio,
            text,
            info_icon,
            info_thumb,
            info_icon_stack,
            info_name,
            info_kind,
            info_detail,
            archive,
            bar,
            title,
            subtitle,
            bar_timer: RefCell::default(),
            kind: Cell::new(Kind::Info),
            pending_present: Cell::new(false),
            settled: Cell::new(false),
            current: RefCell::default(),
            token: Cell::new(0),
            on_step: RefCell::default(),
            on_open: RefCell::default(),
            blurred_at: Cell::default(),
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(q) = w.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::space | gdk::Key::Escape => q.hide(),
                gdk::Key::Left | gdk::Key::Up => q.step(-1),
                gdk::Key::Right | gdk::Key::Down => q.step(1),
                gdk::Key::Return | gdk::Key::KP_Enter => q.open(),
                gdk::Key::f | gdk::Key::F if q.kind.get() == Kind::Video => q.toggle_fullscreen(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        this.window.add_controller(keys);

        let w = Rc::downgrade(&this);
        close.connect_clicked(move |_| {
            if let Some(q) = w.upgrade() {
                q.hide();
            }
        });
        let w = Rc::downgrade(&this);
        open.connect_clicked(move |_| {
            if let Some(q) = w.upgrade() {
                q.open();
            }
        });

        // Double-click a video to watch it full screen.
        let dbl = gtk::GestureClick::new();
        let w = Rc::downgrade(&this);
        dbl.connect_pressed(move |g, n, _, _| {
            if let Some(q) = w.upgrade()
                && n == 2
                && q.kind.get() == Kind::Video
            {
                g.set_state(gtk::EventSequenceState::Claimed);
                q.toggle_fullscreen();
            }
        });
        this.video.add_controller(dbl);

        // Pointer movement brings the bar back over media.
        let motion = gtk::EventControllerMotion::new();
        let w = Rc::downgrade(&this);
        motion.connect_motion(move |_, _, _| {
            if let Some(q) = w.upgrade() {
                q.wake_bar();
            }
        });
        this.root.add_controller(motion);

        // A click in the main window (i.e. outside Quick Look) dismisses it.
        // Watching clicks rather than focus: with focus-follows-mouse, merely
        // moving the pointer off the window would otherwise close it.
        let outside = gtk::GestureClick::new();
        outside.set_button(0);
        outside.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(&this);
        outside.connect_pressed(move |_, _, _, _| {
            if let Some(q) = w.upgrade()
                && q.is_visible()
            {
                q.blurred_at.set(Some(Instant::now()));
                q.hide();
            }
        });
        parent.add_controller(outside);
        // Release media decoders as soon as the window goes away, however it
        // was dismissed (Space, Esc, close button, window manager).
        let w = Rc::downgrade(&this);
        this.window.connect_hide(move |_| {
            if let Some(q) = w.upgrade() {
                q.clear();
            }
        });
        this
    }

    pub fn set_callbacks(&self, on_step: impl Fn(i32) + 'static, on_open: impl Fn() + 'static) {
        *self.on_step.borrow_mut() = Some(Rc::new(on_step));
        *self.on_open.borrow_mut() = Some(Rc::new(on_open));
    }

    fn step(&self, d: i32) {
        let cb = self.on_step.borrow().clone();
        if let Some(cb) = cb {
            cb(d);
        }
    }

    fn open(&self) {
        let cb = self.on_open.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    /// True right after a click outside closed the window, so the click that
    /// caused it (e.g. on the Quick Look button) doesn't reopen it.
    pub fn just_dismissed(&self) -> bool {
        self.blurred_at.get().is_some_and(|t| t.elapsed() < Duration::from_millis(400))
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Stop playback, drop decoded content and forget the current file.
    fn clear(&self) {
        self.token.set(self.token.get() + 1);
        self.current.replace(None);
        self.stop_media();
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.ambient.set_paintable(None::<&gdk::Paintable>);
        self.info_thumb.set_paintable(None::<&gdk::Paintable>);
        self.text.buffer().set_text("");
        self.archive.clear();
        if let Some(id) = self.bar_timer.take() {
            id.remove();
        }
    }

    fn stop_media(&self) {
        if self.window.is_fullscreen() {
            self.window.unfullscreen();
        }
        if let Some(ms) = self.video.media_stream() {
            ms.pause();
            if let Some(mf) = ms.downcast_ref::<gtk::MediaFile>() {
                mf.clear();
            }
        }
        self.video.set_media_stream(None::<&gtk::MediaStream>);
        self.video.set_file(None::<&gio::File>);
        self.audio.stop();
    }

    /// Update the content; present the window if `present` is set.
    pub fn show(self: &Rc<Self>, target: Option<Target>, present: bool) {
        let same = match (&target, self.current.borrow().as_ref()) {
            (Some((p, _)), Some(c)) => p == c,
            _ => false,
        };
        if !same {
            self.load(target);
        }
        if !present {
            return;
        }
        // Open at the content's size: a window can't reliably resize itself
        // once mapped (the compositor may hold it), so wait briefly for media
        // to report its size, then open regardless.
        if self.window.is_visible() || self.settled.get() {
            self.reveal();
        } else {
            self.pending_present.set(true);
            let w = Rc::downgrade(self);
            glib::timeout_add_local_once(SETTLE_WAIT, move || {
                if let Some(q) = w.upgrade()
                    && q.pending_present.get()
                {
                    q.reveal();
                }
            });
        }
    }

    fn reveal(self: &Rc<Self>) {
        self.pending_present.set(false);
        self.window.present();
        // Keys are handled on the window itself; a focused Close button would
        // only add a focus ring to every preview.
        GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        self.wake_bar();
    }

    fn load(self: &Rc<Self>, target: Option<Target>) {
        self.clear();
        self.settled.set(false);
        let id = self.token.get();
        let Some((path, info)) = target else {
            self.set_kind(Kind::Info);
            self.title.set_text("Nothing selected");
            self.subtitle.set_text("");
            self.info_icon.set_icon_name(Some(themed("luma-eye-symbolic")));
            self.info_icon.set_pixel_size(64);
            self.info_icon_stack.set_visible_child_name("icon");
            self.info_name.set_text("Nothing selected");
            self.info_kind.set_text("Select a file to preview it");
            self.info_detail.set_text("");
            self.settle(420, 360);
            return;
        };
        *self.current.borrow_mut() = Some(path.clone());
        self.info_icon.set_pixel_size(128);
        let ct = info.content_type().map(|c| c.to_string()).unwrap_or_default();
        let kind_desc = gio::content_type_get_description(&ct).to_string();
        let dir = is_dir(&info);
        let size = (!dir).then(|| human_size(info.size()));
        self.title.set_text(&info.display_name());
        self.subtitle.set_text(&match &size {
            Some(s) => format!("{kind_desc} · {s}"),
            None => kind_desc.clone(),
        });

        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = if dir {
            Kind::Info
        } else if crate::services::archive::is_archive(&file_name) {
            Kind::Archive
        } else if ct.starts_with("image/") {
            Kind::Image
        } else if ct.starts_with("video/") {
            Kind::Video
        } else if ct.starts_with("audio/") {
            Kind::Audio
        } else if is_textual(&ct) {
            Kind::Text
        } else {
            Kind::Info
        };
        self.fill_info(&path, &info, &kind_desc, size.as_deref(), id);
        self.set_kind(kind);

        match kind {
            Kind::Image => {
                // Until it decodes, show the info card rather than an empty frame.
                self.pages.set_visible_child_name("info");
                let this = self.clone();
                glib::spawn_future_local(async move {
                    let loaded = thumbnail::load_texture(path.to_string_lossy().into_owned(), IMAGE_PX).await;
                    if this.token.get() != id {
                        return;
                    }
                    if let Some((tex, (w, h))) = loaded {
                        this.picture.set_paintable(Some(&tex));
                        this.ambient.set_paintable(Some(&tex));
                        this.pages.set_visible_child_name("image");
                        let (w, h) = if w > 0 { (w, h) } else { (tex.width(), tex.height()) };
                        this.subtitle.set_text(&format!("{} · {w} × {h}", this.subtitle.text()));
                        let (w, h) = fit(w, h);
                        this.settle(w, h);
                    } else {
                        this.set_kind(Kind::Info);
                        this.settle(440, 400);
                    }
                });
            }
            Kind::Video => {
                self.video.set_file(Some(&gio::File::for_path(&path)));
                self.resize(720, 405);
                if let Some(ms) = self.video.media_stream() {
                    let w = Rc::downgrade(self);
                    ms.connect_prepared_notify(move |s| {
                        let Some(q) = w.upgrade().filter(|q| q.token.get() == id) else { return };
                        let (vw, vh) = (s.intrinsic_width(), s.intrinsic_height());
                        if vw > 0 && vh > 0 {
                            let (w, h) = fit(vw, vh);
                            q.settle(w, h);
                        }
                    });
                    let w = Rc::downgrade(self);
                    ms.connect_error_notify(move |s| {
                        let Some(q) = w.upgrade().filter(|q| q.token.get() == id) else { return };
                        if let Some(e) = s.error() {
                            q.info_detail.set_text(&format!("Can't play: {}", e.message()));
                            q.set_kind(Kind::Info);
                            q.settle(440, 400);
                        }
                    });
                }
            }
            Kind::Audio => {
                self.audio.load(&path);
                self.settle(380, 580);
            }
            Kind::Text => {
                let is_md = ct == "text/markdown"
                    || matches!(path.extension().and_then(|e| e.to_str()), Some("md" | "markdown"));
                self.settle(760, 640);
                let this = self.clone();
                glib::spawn_future_local(async move {
                    let Some(bytes) = read_head(&path, TEXT_CAP).await else { return };
                    if this.token.get() != id {
                        return;
                    }
                    if bytes.contains(&0) {
                        this.set_kind(Kind::Info);
                        return;
                    }
                    let mut s = String::from_utf8_lossy(&bytes).into_owned();
                    if bytes.len() as i64 >= TEXT_CAP as i64 {
                        s.push_str("\n… (preview truncated)");
                    }
                    let buf = this.text.buffer();
                    if is_md {
                        render_markdown(&buf, &s);
                        this.text.set_monospace(false);
                    } else {
                        buf.set_text(&s);
                        this.text.set_monospace(true);
                    }
                });
            }
            Kind::Archive => {
                self.archive.load(&path, &info.display_name(), &ct, &self.subtitle.text());
                self.settle(600, 640);
            }
            Kind::Info => self.settle(440, 400),
        }
    }

    /// The card shown for folders, documents and anything without a viewer.
    fn fill_info(
        self: &Rc<Self>,
        path: &std::path::Path,
        info: &gio::FileInfo,
        kind: &str,
        size: Option<&str>,
        id: u64,
    ) {
        self.info_name.set_text(&info.display_name());
        self.info_kind.set_text(kind);
        let modified = info
            .modification_date_time()
            .and_then(|d| d.to_local().ok())
            .and_then(|d| d.format("%-d %b %Y, %H:%M").ok());
        let detail = [size.map(str::to_string), modified.map(|m| format!("Modified {m}"))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        self.info_detail.set_text(&detail);
        // The same duotone art the file views use.
        if let Some(icon) = crate::core::filesystem::file_icon(info).or_else(|| info.icon()) {
            self.info_icon.set_from_gicon(&icon);
        }
        self.info_icon_stack.set_visible_child_name("icon");

        if is_dir(info) {
            let (p, this) = (path.to_path_buf(), self.clone());
            glib::spawn_future_local(async move {
                const CAP: usize = 100_000;
                if let Some(n) = count_children(&p, CAP).await
                    && this.token.get() == id
                {
                    let items = match n {
                        n if n >= CAP => format!("{CAP}+ items"),
                        1 => "1 item".into(),
                        n => format!("{n} items"),
                    };
                    this.info_detail.set_text(&format!("{items} · {}", this.info_detail.text()));
                    this.subtitle.set_text(&format!("{} · {items}", this.subtitle.text()));
                }
            });
        } else if let Some(src) = info.attribute_byte_string("thumbnail::path") {
            // PDFs, documents, … : reuse a system-generated thumbnail if one exists.
            let this = self.clone();
            glib::spawn_future_local(async move {
                if let Some((tex, _)) = thumbnail::load_texture(src.to_string(), 512).await
                    && this.token.get() == id
                {
                    this.info_thumb.set_paintable(Some(&tex));
                    this.info_icon_stack.set_visible_child_name("thumb");
                }
            });
        }
    }

    fn set_kind(self: &Rc<Self>, kind: Kind) {
        let prev = self.kind.replace(kind);
        self.root.remove_css_class(prev.page());
        self.root.add_css_class(kind.page());
        self.pages.set_visible_child_name(kind.page());
        // The player and the archive header show the name themselves.
        let own_title = matches!(kind, Kind::Audio | Kind::Archive);
        self.title.set_visible(!own_title);
        self.subtitle.set_visible(!own_title);
        // A preview is the size of its content, which also keeps tiling and
        // floating rules from stretching it; documents stay resizable for reading.
        self.window.set_resizable(kind == Kind::Text);
        if kind != Kind::Video && self.window.is_fullscreen() {
            self.window.unfullscreen();
        }
        self.wake_bar();
    }

    /// Debug: save the window's current rendering as a PNG.
    #[cfg(debug_assertions)]
    pub fn snapshot(&self, to: &std::path::Path) {
        let w = &self.window;
        let p = gtk::WidgetPaintable::new(Some(w));
        let s = gtk::Snapshot::new();
        p.snapshot(&s, w.width() as f64, w.height() as f64);
        if let (Some(node), Some(r)) = (s.to_node(), w.native().and_then(|n| n.renderer())) {
            let _ = r.render_texture(node, None).save_to_png(to);
        }
    }

    fn toggle_fullscreen(&self) {
        if self.window.is_fullscreen() {
            self.window.unfullscreen();
        } else {
            self.window.fullscreen();
        }
    }

    fn resize(&self, w: i32, h: i32) {
        self.window.set_default_size(w, h);
    }

    /// The content's size is known: request it, and open if we were waiting.
    fn settle(self: &Rc<Self>, w: i32, h: i32) {
        self.resize(w, h);
        self.settled.set(true);
        if self.pending_present.get() {
            self.reveal();
        }
    }

    /// Show the bar; over media, hide it again once the pointer rests.
    fn wake_bar(self: &Rc<Self>) {
        self.bar.set_reveal_child(true);
        if let Some(id) = self.bar_timer.take() {
            id.remove();
        }
        if !self.kind.get().immersive() {
            return;
        }
        let w = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(BAR_IDLE, move || {
            if let Some(q) = w.upgrade() {
                q.bar_timer.take();
                q.bar.set_reveal_child(false);
            }
        });
        *self.bar_timer.borrow_mut() = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_keeps_its_aspect_and_fits_the_bounds() {
        assert_eq!(fit(1920, 1080), (1100, 619));
        assert_eq!(fit(4000, 3000), (987, 740));
        assert_eq!(fit(800, 600), (800, 600), "small media is not enlarged");
        assert_eq!(fit(100, 100), (MIN_W, MIN_H), "tiny media still gets a usable window");
        assert_eq!(fit(0, 0), (720, 520));
    }
}
