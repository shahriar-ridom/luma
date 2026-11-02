pub mod archive;
pub mod batch_rename;
pub mod cloud;
pub mod compare;
pub mod connect;
pub mod duplicates;
pub mod extensions;
pub mod open_with;
pub mod properties;
pub mod search;
pub mod shortcuts;
pub mod storage;
pub mod tags;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::core::filesystem::human_size;
use crate::core::operations::Resolution;

async fn describe(src: &gio::File, dst: &gio::File) -> String {
    async fn line(label: &str, f: &gio::File) -> (String, i64, Option<glib::DateTime>) {
        match f
            .query_info_future("standard::size,time::modified", gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
            .await
        {
            Ok(i) => {
                let m = i.modification_date_time();
                let when = m
                    .as_ref()
                    .and_then(|d| d.to_local().ok())
                    .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown date".into());
                (format!("{label}: {} · modified {when}", human_size(i.size())), i.size(), m)
            }
            Err(_) => (format!("{label}: unavailable"), -1, None),
        }
    }
    let (a, asz, am) = line("New", src).await;
    let (b, bsz, bm) = line("Existing", dst).await;
    let verdict = match (am, bm) {
        (Some(x), Some(y)) if x == y && asz == bsz => "Same size and modification time — probably identical.",
        (Some(x), Some(y)) if x > y => "The new file is more recent.",
        (Some(x), Some(y)) if x < y => "The existing file is more recent.",
        _ => "",
    };
    format!("{a}\n{b}\n\n{verdict}")
}

/// Ask what to do about an existing destination file.
pub async fn ask_conflict(parent: Option<&gtk::Window>, src_uri: &str, dst_uri: &str) -> Resolution {
    let (src, dst) = (gio::File::for_uri(src_uri), gio::File::for_uri(dst_uri));
    let name = dst.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut detailed = false;
    loop {
        let body = if detailed {
            describe(&src, &dst).await
        } else {
            format!("A file named “{name}” already exists in the destination.")
        };
        let d = adw::AlertDialog::new(Some("Replace existing file?"), Some(&body));
        d.add_responses(&[
            ("cancel", "Cancel"),
            ("compare", "Compare"),
            ("skip", "Skip"),
            ("skip_all", "Skip All"),
            ("rename", "Keep Both"),
            ("replace", "Replace"),
            ("replace_all", "Replace All"),
        ]);
        d.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        d.set_response_appearance("replace_all", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("skip"));
        d.set_close_response("cancel");
        let r = d.choose_future(parent).await;
        return match r.as_str() {
            "compare" => {
                detailed = true;
                continue;
            }
            "skip" => Resolution::Skip,
            "skip_all" => Resolution::SkipAll,
            "rename" => Resolution::Rename,
            "replace" => Resolution::Replace,
            "replace_all" => Resolution::ReplaceAll,
            _ => Resolution::Cancel,
        };
    }
}

pub async fn confirm_delete(parent: &gtk::Window, count: usize, first_name: &str) -> bool {
    let body = if count == 1 {
        format!("“{first_name}” will be deleted permanently. This cannot be undone.")
    } else {
        format!("{count} items will be deleted permanently. This cannot be undone.")
    };
    let d = adw::AlertDialog::new(Some("Delete permanently?"), Some(&body));
    d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "delete"
}

/// Why `name` can't be used as a file name, if it can't.
pub fn invalid_name_reason(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("The name can’t be empty.")
    } else if name == "." || name == ".." {
        Some("“.” and “..” are reserved names.")
    } else if name.contains('/') {
        Some("Names can’t contain “/”.")
    } else if name.contains('\0') {
        Some("Names can’t contain null characters.")
    } else if name.len() > 255 {
        Some("The name is too long.")
    } else {
        None
    }
}

/// Prompt for a file name. Returns `None` if cancelled. An invalid name
/// re-opens the prompt with the reason instead of being dropped silently.
pub async fn prompt_name(parent: &gtk::Window, heading: &str, action: &str, initial: &str) -> Option<String> {
    let mut text = initial.to_string();
    let mut problem: Option<&str> = None;
    loop {
        let d = adw::AlertDialog::new(Some(heading), problem);
        d.add_responses(&[("cancel", "Cancel"), ("ok", action)]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        let entry = gtk::Entry::new();
        entry.set_text(&text);
        entry.set_activates_default(true);
        if problem.is_some() {
            entry.add_css_class("error");
        }
        let stem_len = match text.rfind('.') {
            Some(i) if i > 0 && problem.is_none() => text[..i].chars().count() as i32,
            _ => -1,
        };
        d.set_extra_child(Some(&entry));
        let e = entry.clone();
        glib::idle_add_local_once(move || {
            e.grab_focus();
            e.select_region(0, stem_len);
        });
        if d.choose_future(Some(parent)).await != "ok" {
            return None;
        }
        text = entry.text().trim().to_string();
        match invalid_name_reason(&text) {
            None => return Some(text),
            Some(why) => problem = Some(why),
        }
    }
}

pub fn error(parent: &gtk::Window, heading: &str, body: &str) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("ok", "OK");
    d.present(Some(parent));
}

/// Yes/no question. `destructive` styles the confirm button red.
pub async fn confirm(parent: &gtk::Window, heading: &str, body: &str, action: &str, destructive: bool) -> bool {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_responses(&[("cancel", "Cancel"), ("ok", action)]);
    d.set_response_appearance(
        "ok",
        if destructive { adw::ResponseAppearance::Destructive } else { adw::ResponseAppearance::Suggested },
    );
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "ok"
}

/// Multi-line text prompt (commit messages). Returns trimmed non-empty text.
pub async fn prompt_text(parent: &gtk::Window, heading: &str, action: &str, hint: &str) -> Option<String> {
    let d = adw::AlertDialog::new(Some(heading), Some(hint));
    d.add_responses(&[("cancel", "Cancel"), ("ok", action)]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_close_response("cancel");
    let view = gtk::TextView::new();
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.set_top_margin(6);
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&view));
    sw.set_min_content_height(110);
    sw.add_css_class("card");
    d.set_extra_child(Some(&sw));
    let v = view.clone();
    glib::idle_add_local_once(move || {
        v.grab_focus();
    });
    if d.choose_future(Some(parent)).await != "ok" {
        return None;
    }
    let b = view.buffer();
    let t = b.text(&b.start_iter(), &b.end_iter(), false).trim().to_string();
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::invalid_name_reason;

    #[test]
    fn name_validation() {
        assert!(invalid_name_reason("report.txt").is_none());
        assert!(invalid_name_reason(".hidden").is_none());
        for bad in ["", ".", "..", "a/b", &"x".repeat(256)] {
            assert!(invalid_name_reason(bad).is_some(), "{bad:?} should be rejected");
        }
    }
}
