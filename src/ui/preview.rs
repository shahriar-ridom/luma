//! Asynchronous, resource-limited file preview (side panel and gallery).
//!
//! Every `show` bumps a token; async work started for an earlier target
//! checks it before touching the UI, so fast selection changes never flash
//! stale content. Text reads are capped and images are decoded off-thread at
//! a bounded size. Video/audio is only loaded for the current target and
//! released as soon as it changes.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use crate::core::filesystem::{human_size, is_dir};
use crate::services::thumbnail;

const TEXT_CAP: i32 = 64 * 1024;
const COUNT_CAP: usize = 100_000;

pub type Target = (PathBuf, gio::FileInfo);

pub struct Preview {
    pub widget: gtk::Box,
    media: gtk::Stack,
    picture: gtk::Picture,
    text: gtk::TextView,
    video: gtk::Video,
    icon: gtk::Image,
    title: gtk::Label,
    meta: gtk::Box,
    token: Cell<u64>,
    current: RefCell<Option<PathBuf>>,
    image_px: i32,
}

fn is_textual(ct: &str) -> bool {
    gio::content_type_is_a(ct, "text/plain")
        || matches!(
            ct,
            "application/json"
                | "application/xml"
                | "application/x-shellscript"
                | "application/javascript"
                | "application/toml"
                | "application/x-yaml"
                | "application/x-desktop"
                | "application/sql"
        )
}

impl Preview {
    /// `large`: gallery mode — bigger decode size and a compact metadata line.
    pub fn new(large: bool) -> Rc<Self> {
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        picture.set_overflow(gtk::Overflow::Hidden);
        let text = gtk::TextView::new();
        text.set_editable(false);
        text.set_cursor_visible(false);
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.set_left_margin(10);
        text.set_right_margin(10);
        text.set_top_margin(8);
        text.add_css_class("fm-preview-text");
        let text_sw = gtk::ScrolledWindow::new();
        text_sw.set_child(Some(&text));
        let video = gtk::Video::new();
        video.set_autoplay(true);
        let icon = gtk::Image::new();
        icon.set_pixel_size(if large { 192 } else { 128 });
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("fm-eye-symbolic"));
        empty.set_title("No Selection");
        empty.set_description(Some("Select a file to preview it"));

        let media = gtk::Stack::new();
        media.set_transition_type(gtk::StackTransitionType::Crossfade);
        media.set_transition_duration(100);
        media.add_named(&empty, Some("empty"));
        media.add_named(&icon, Some("icon"));
        media.add_named(&picture, Some("image"));
        media.add_named(&text_sw, Some("text"));
        media.add_named(&video, Some("video"));
        media.set_vexpand(true);
        media.set_hexpand(true);
        media.set_size_request(if large { 280 } else { 240 }, if large { 150 } else { 220 });
        media.add_css_class("fm-preview-media");

        let title = gtk::Label::new(None);
        title.add_css_class("title-4");
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_selectable(true);
        title.set_xalign(0.0);
        let meta = gtk::Box::new(gtk::Orientation::Vertical, 4);

        let info = gtk::Box::new(gtk::Orientation::Vertical, 8);
        info.set_margin_top(12);
        info.set_margin_bottom(12);
        info.set_margin_start(14);
        info.set_margin_end(14);
        info.append(&title);
        info.append(&meta);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("fm-preview");
        widget.append(&media);
        if large {
            let sw = gtk::ScrolledWindow::new();
            sw.set_child(Some(&info));
            sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            sw.set_max_content_height(190);
            sw.set_min_content_height(112);
            sw.set_propagate_natural_height(true);
            widget.append(&sw);
        } else {
            let sw = gtk::ScrolledWindow::new();
            sw.set_child(Some(&info));
            sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            sw.set_min_content_height(160);
            widget.append(&sw);
        }
        Rc::new(Self {
            widget,
            media,
            picture,
            text,
            video,
            icon,
            title,
            meta,
            token: Cell::new(0),
            current: RefCell::default(),
            image_px: if large { 2048 } else { 1024 },
        })
    }

    /// For hosts that already show the file name (Quick Look's title bar).
    pub fn hide_title(&self) {
        self.title.set_visible(false);
    }

    fn clear_meta(&self) {
        while let Some(c) = self.meta.first_child() {
            self.meta.remove(&c);
        }
    }

    fn add_row(&self, key: &str, val: &str) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let k = gtk::Label::new(Some(key));
        k.add_css_class("key");
        k.set_xalign(0.0);
        k.set_width_chars(10);
        k.set_valign(gtk::Align::Start);
        let v = gtk::Label::new(Some(val));
        v.set_xalign(0.0);
        v.set_wrap(true);
        v.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        v.set_hexpand(true);
        row.append(&k);
        row.append(&v);
        self.meta.append(&row);
    }

    /// Stop playback and release the decoder. Pausing is not enough: the
    /// stream must be torn down or audio can keep running in the background.
    fn stop_media(&self) {
        if let Some(ms) = self.video.media_stream() {
            ms.pause();
            if let Some(mf) = ms.downcast_ref::<gtk::MediaFile>() {
                mf.clear();
            }
        }
        self.video.set_media_stream(None::<&gtk::MediaStream>);
        self.video.set_file(None::<&gio::File>);
    }

    pub fn show(self: &Rc<Self>, target: Option<Target>) {
        // Repeated updates for the same file (selection/status events) must
        // not restart playback.
        if let Some((p, _)) = &target
            && self.current.borrow().as_deref() == Some(p.as_path())
        {
            return;
        }
        *self.current.borrow_mut() = target.as_ref().map(|(p, _)| p.clone());
        let id = self.token.get() + 1;
        self.token.set(id);
        self.stop_media();
        self.picture.set_paintable(None::<&gtk::gdk::Paintable>);
        self.clear_meta();

        let Some((path, info)) = target else {
            self.title.set_text("");
            self.media.set_visible_child_name("empty");
            return;
        };
        self.title.set_text(&info.display_name());
        let ct = info.content_type().map(|c| c.to_string()).unwrap_or_default();
        let kind = gio::content_type_get_description(&ct);
        self.add_row("Kind", &kind);
        if !is_dir(&info) {
            self.add_row("Size", &format!("{} ({} bytes)", human_size(info.size()), info.size()));
        }
        if let Some(d) =
            info.modification_date_time().and_then(|d| d.to_local().ok()).and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        {
            self.add_row("Modified", &d);
        }
        self.add_row("Where", &path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());

        if let Some(icon) = info.icon() {
            self.icon.set_from_gicon(&icon);
        }
        self.media.set_visible_child_name("icon");

        let this = self.clone();
        if !is_dir(&info) && (ct.starts_with("image/") || ct.starts_with("audio/")) {
            let (p, mime, t) = (path.clone(), ct.clone(), this.clone());
            glib::spawn_future_local(async move {
                let groups = gio::spawn_blocking(move || crate::core::metadata::advanced::extract_builtin(&p, &mime))
                    .await
                    .unwrap_or_default();
                if t.token.get() == id {
                    for it in groups.into_iter().flat_map(|g| g.items).take(8) {
                        t.add_row(&it.key, &it.value);
                    }
                }
            });
        }
        if is_dir(&info) {
            glib::spawn_future_local(async move {
                if let Some(n) = count_children(&path, COUNT_CAP).await
                    && this.token.get() == id
                {
                    let s = if n >= COUNT_CAP {
                        format!("{COUNT_CAP}+ items")
                    } else if n == 1 {
                        "1 item".into()
                    } else {
                        format!("{n} items")
                    };
                    this.add_row("Contains", &s);
                }
            });
        } else if ct.starts_with("image/") {
            let px = self.image_px;
            glib::spawn_future_local(async move {
                if let Some((tex, (w, h))) = thumbnail::load_texture(path.to_string_lossy().into_owned(), px).await
                    && this.token.get() == id
                {
                    this.picture.set_paintable(Some(&tex));
                    this.media.set_visible_child_name("image");
                    if w > 0 {
                        this.add_row("Dimensions", &format!("{w} × {h} px"));
                    }
                }
            });
        } else if ct.starts_with("video/") || ct.starts_with("audio/") {
            this.video.set_file(Some(&gio::File::for_path(&path)));
            this.media.set_visible_child_name("video");
            if let Some(ms) = this.video.media_stream() {
                let weak = Rc::downgrade(&this);
                ms.connect_error_notify(move |s| {
                    let Some(p) = weak.upgrade() else { return };
                    if p.token.get() == id
                        && let Some(e) = s.error()
                    {
                        p.add_row("Playback", &format!("Unavailable: {}", e.message()));
                        p.media.set_visible_child_name("icon");
                    }
                });
            }
        } else if is_textual(&ct) {
            let is_md =
                ct == "text/markdown" || matches!(path.extension().and_then(|e| e.to_str()), Some("md" | "markdown"));
            glib::spawn_future_local(async move {
                let Some(bytes) = read_head(&path, TEXT_CAP).await else { return };
                if this.token.get() != id || bytes.contains(&0) {
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
                this.media.set_visible_child_name("text");
            });
        } else if let Some(src) = info.attribute_byte_string("thumbnail::path") {
            // PDFs, documents, … : reuse a system-generated thumbnail if one exists.
            glib::spawn_future_local(async move {
                if let Some((tex, _)) = thumbnail::load_texture(src.to_string(), 512).await
                    && this.token.get() == id
                {
                    this.picture.set_paintable(Some(&tex));
                    this.media.set_visible_child_name("image");
                }
            });
        }
    }
}

async fn read_head(path: &std::path::Path, max: i32) -> Option<Vec<u8>> {
    let f = gio::File::for_path(path);
    let s = f.read_future(glib::Priority::DEFAULT).await.ok()?;
    let b = s.read_bytes_future(max as usize, glib::Priority::DEFAULT).await.ok()?;
    Some(b.to_vec())
}

async fn count_children(path: &std::path::Path, cap: usize) -> Option<usize> {
    let en = gio::File::for_path(path)
        .enumerate_children_future("standard::name", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, glib::Priority::LOW)
        .await
        .ok()?;
    let mut n = 0;
    while n < cap {
        let batch = en.next_files_future(500, glib::Priority::LOW).await.ok()?;
        if batch.is_empty() {
            break;
        }
        n += batch.len();
    }
    Some(n)
}

/// Minimal Markdown: headings, bullets, fenced code, `code` and **bold**.
fn render_markdown(buf: &gtk::TextBuffer, src: &str) {
    buf.set_text("");
    let table = buf.tag_table();
    for (name, scale, bold) in [("h1", 1.6, true), ("h2", 1.35, true), ("h3", 1.15, true), ("bold", 1.0, true)] {
        let t = gtk::TextTag::builder().name(name).scale(scale).weight(if bold { 700 } else { 400 }).build();
        table.add(&t);
    }
    let code = gtk::TextTag::builder()
        .name("code")
        .family("monospace")
        .background_rgba(&gtk::gdk::RGBA::new(0.5, 0.5, 0.5, 0.15))
        .build();
    table.add(&code);

    let mut fence = false;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        let mut end = buf.end_iter();
        if fence {
            buf.insert_with_tags_by_name(&mut end, &format!("{line}\n"), &["code"]);
            continue;
        }
        let (text, head): (String, Option<&str>) = if let Some(t) = line.strip_prefix("### ") {
            (t.into(), Some("h3"))
        } else if let Some(t) = line.strip_prefix("## ") {
            (t.into(), Some("h2"))
        } else if let Some(t) = line.strip_prefix("# ") {
            (t.into(), Some("h1"))
        } else if let Some(t) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            (format!("• {t}"), None)
        } else {
            (line.into(), None)
        };
        if let Some(h) = head {
            buf.insert_with_tags_by_name(&mut end, &format!("{text}\n"), &[h]);
            continue;
        }
        // Inline: toggle bold on "**" and code on "`".
        let (mut bold, mut mono) = (false, false);
        let mut cur = String::new();
        let flush = |cur: &mut String, bold: bool, mono: bool| {
            if cur.is_empty() {
                return;
            }
            let mut tags = Vec::new();
            if bold {
                tags.push("bold")
            }
            if mono {
                tags.push("code")
            }
            let mut it = buf.end_iter();
            buf.insert_with_tags_by_name(&mut it, cur, &tags);
            cur.clear();
        };
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '*' && chars.peek() == Some(&'*') {
                chars.next();
                flush(&mut cur, bold, mono);
                bold = !bold;
            } else if c == '`' {
                flush(&mut cur, bold, mono);
                mono = !mono;
            } else {
                cur.push(c);
            }
        }
        cur.push('\n');
        flush(&mut cur, bold, mono);
    }
}
