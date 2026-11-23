//! Archive contents for Quick Look: a summary header and the entries as an
//! indented tree. Listing runs bsdtar off the main thread; nothing is
//! extracted. Encrypted archives are not unlocked here — that stays with
//! opening or extracting them, which asks for the password.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::Cell, path::Path, rc::Rc};

use crate::core::filesystem::{human_size, icon_for};
use crate::services::archive::{self, ArchiveError, Summary, TreeRow};

/// Rows beyond this are summarised in a footer instead of listed.
const MAX_ROWS: usize = 5000;
/// Indentation per nesting level, in pixels.
const INDENT: i32 = 18;

pub struct ArchivePreview {
    pub widget: gtk::Box,
    icon: gtk::Image,
    name: gtk::Label,
    kind: gtk::Label,
    summary: gtk::Label,
    body: gtk::Stack,
    store: gio::ListStore,
    message: gtk::Label,
    detail: gtk::Label,
    spinner: gtk::Spinner,
    more: gtk::Label,
    token: Cell<u64>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

fn describe(s: &Summary) -> String {
    let mut parts = vec![plural(s.files, "file", "files")];
    if s.folders > 0 {
        parts.push(plural(s.folders, "folder", "folders"));
    }
    format!("{} · {} uncompressed", parts.join(", "), human_size(s.bytes as i64))
}

fn row_widget() -> gtk::Box {
    let icon = gtk::Image::new();
    icon.set_pixel_size(16);
    icon.add_css_class("fm-ql-icon");
    let name = gtk::Label::new(None);
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let size = gtk::Label::new(None);
    size.add_css_class("fm-ql-meta");
    size.set_xalign(1.0);
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    b.append(&icon);
    b.append(&name);
    b.append(&size);
    b
}

fn bind_row(b: &gtk::Box, r: &TreeRow) {
    let (Some(icon), Some(name), Some(size)) = (
        b.first_child().and_downcast::<gtk::Image>(),
        b.first_child().and_then(|w| w.next_sibling()).and_downcast::<gtk::Label>(),
        b.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    b.set_margin_start(r.depth as i32 * INDENT);
    let mime = if r.is_dir {
        "inode/directory".to_string()
    } else {
        gio::content_type_guess(Some(&r.name), None).0.to_string()
    };
    icon.set_from_gicon(&icon_for(&mime, r.is_dir));
    name.set_text(&r.name);
    size.set_text(&human_size(r.size as i64));
    if r.is_dir {
        name.add_css_class("fm-ql-folder");
    } else {
        name.remove_css_class("fm-ql-folder");
    }
}

impl ArchivePreview {
    pub fn new() -> Rc<Self> {
        let icon = gtk::Image::new();
        icon.set_pixel_size(56);
        icon.add_css_class("fm-ql-icon");
        let name = gtk::Label::new(None);
        name.add_css_class("fm-ql-name");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let kind = gtk::Label::new(None);
        kind.add_css_class("fm-ql-meta");
        kind.set_xalign(0.0);
        let summary = gtk::Label::new(None);
        summary.add_css_class("fm-ql-meta");
        summary.set_xalign(0.0);
        summary.set_wrap(true);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        titles.set_valign(gtk::Align::Center);
        titles.append(&name);
        titles.append(&kind);
        titles.append(&summary);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        header.add_css_class("fm-ql-archive-head");
        header.append(&icon);
        header.append(&titles);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, o| {
            if let Some(item) = o.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&row_widget()));
                item.set_activatable(false);
            }
        });
        factory.connect_bind(|_, o| {
            let Some(item) = o.downcast_ref::<gtk::ListItem>() else { return };
            let (Some(b), Some(obj)) =
                (item.child().and_downcast::<gtk::Box>(), item.item().and_downcast::<glib::BoxedAnyObject>())
            else {
                return;
            };
            bind_row(&b, &obj.borrow::<TreeRow>());
        });
        let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store.clone()))), Some(factory));
        list.add_css_class("fm-ql-archive");
        let more = gtk::Label::new(None);
        more.add_css_class("fm-ql-meta");
        more.set_margin_top(6);
        more.set_margin_bottom(14);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&list);
        content.append(&more);
        let sw = gtk::ScrolledWindow::new();
        sw.set_child(Some(&content));
        sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        sw.set_vexpand(true);

        let spinner = gtk::Spinner::new();
        let message = gtk::Label::new(None);
        message.add_css_class("fm-ql-name");
        message.set_wrap(true);
        message.set_justify(gtk::Justification::Center);
        let detail = gtk::Label::new(None);
        detail.add_css_class("fm-ql-meta");
        detail.set_wrap(true);
        detail.set_justify(gtk::Justification::Center);
        detail.set_max_width_chars(44);
        let status = gtk::Box::new(gtk::Orientation::Vertical, 8);
        status.set_valign(gtk::Align::Center);
        status.set_halign(gtk::Align::Center);
        status.set_margin_start(32);
        status.set_margin_end(32);
        status.append(&spinner);
        status.append(&message);
        status.append(&detail);

        let body = gtk::Stack::new();
        body.set_transition_type(gtk::StackTransitionType::Crossfade);
        body.set_transition_duration(140);
        body.add_named(&sw, Some("list"));
        body.add_named(&status, Some("status"));

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("fm-ql-archive-page");
        widget.append(&header);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&body);

        Rc::new(Self {
            widget,
            icon,
            name,
            kind,
            summary,
            body,
            store,
            message,
            detail,
            spinner,
            more,
            token: Cell::new(0),
        })
    }

    /// Drop the listing and ignore any listing still running.
    pub fn clear(&self) {
        self.token.set(self.token.get() + 1);
        self.store.remove_all();
        self.spinner.stop();
    }

    /// List `path` (never extracting anything) and show its contents.
    /// `kind` is the line under the name, e.g. "Zip archive · 152 kB".
    pub fn load(self: &Rc<Self>, path: &Path, display_name: &str, content_type: &str, kind: &str) {
        self.clear();
        let id = self.token.get();
        self.icon.set_from_gicon(&icon_for(content_type, false));
        self.name.set_text(display_name);
        self.kind.set_text(kind);
        self.summary.set_text("Reading contents…");
        self.status("", "", true);

        let (p, w) = (path.to_path_buf(), Rc::downgrade(self));
        glib::spawn_future_local(async move {
            let listed = gio::spawn_blocking(move || archive::list(&p, None).map(|e| archive::tree(&e))).await;
            let Some(this) = w.upgrade().filter(|t| t.token.get() == id) else { return };
            match listed {
                Ok(Ok((rows, _))) if rows.is_empty() => {
                    this.summary.set_text("Empty archive");
                    this.status("This archive is empty", "", false);
                }
                Ok(Ok((rows, sum))) => this.show_rows(rows, &sum),
                Ok(Err(ArchiveError::NeedsPassphrase)) => {
                    this.summary.set_text("Encrypted");
                    this.status("Password protected", "Open or extract it to enter the password.", false);
                }
                Ok(Err(ArchiveError::Missing)) => {
                    this.summary.set_text("Contents unavailable");
                    this.status("Can't read archives", "Install bsdtar (libarchive) to preview them.", false);
                }
                Ok(Err(e)) => {
                    this.summary.set_text("Contents unavailable");
                    this.status("Can't read this archive", &e.to_string(), false);
                }
                Err(_) => this.status("Can't read this archive", "", false),
            }
        });
    }

    fn show_rows(&self, rows: Vec<TreeRow>, sum: &Summary) {
        self.summary.set_text(&describe(sum));
        let total = rows.len();
        let objs: Vec<glib::BoxedAnyObject> = rows.into_iter().take(MAX_ROWS).map(glib::BoxedAnyObject::new).collect();
        self.store.extend_from_slice(&objs);
        let hidden = total.saturating_sub(MAX_ROWS);
        self.more.set_text(&format!("and {} more", plural(hidden, "item", "items")));
        self.more.set_visible(hidden > 0);
        self.spinner.stop();
        self.body.set_visible_child_name("list");
    }

    fn status(&self, message: &str, detail: &str, busy: bool) {
        self.message.set_text(message);
        self.message.set_visible(!message.is_empty());
        self.detail.set_text(detail);
        self.detail.set_visible(!detail.is_empty());
        self.spinner.set_visible(busy);
        if busy {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }
        self.body.set_visible_child_name("status");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reads_naturally() {
        assert_eq!(describe(&Summary { files: 1, folders: 0, bytes: 0 }), "1 file · 0 bytes uncompressed");
        assert_eq!(
            describe(&Summary { files: 12, folders: 1, bytes: 2048 }),
            "12 files, 1 folder · 2.0\u{a0}kB uncompressed"
        );
    }
}
