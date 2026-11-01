//! Properties: general info, async folder sizes, checksums, and a
//! permissions editor. Everything runs as the current user; if the OS
//! refuses a change, the error is shown rather than retried with privileges.

use adw::prelude::*;
use gtk::glib;
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::application::state::AppState;
use crate::core::filesystem::human_size;
use crate::core::metadata::{self, Algo, DirStats};
use crate::core::permissions::{self, SETGID, SETUID, STICKY, Scope};

fn row(title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_title(title);
    r.set_subtitle(value);
    r.set_subtitle_selectable(true);
    r.add_css_class("property");
    r
}

fn fmt_time(t: std::io::Result<std::time::SystemTime>) -> String {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| glib::DateTime::from_unix_local(d.as_secs() as i64).ok())
        .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Unknown".into())
}

fn kind_of(p: &Path, md: &std::fs::Metadata) -> String {
    if md.file_type().is_symlink() {
        return "Symbolic link".into();
    }
    let ct = if md.is_dir() {
        "inode/directory".to_string()
    } else {
        gtk::gio::content_type_guess(p.file_name().and_then(|n| n.to_str()), None).0.to_string()
    };
    format!("{} ({ct})", gtk::gio::content_type_get_description(&ct))
}

fn size_text(bytes: u64) -> String {
    format!("{} ({} bytes)", human_size(bytes as i64), bytes)
}

pub fn show(parent: &gtk::Window, targets: Vec<PathBuf>, state: Rc<AppState>) {
    if targets.is_empty() {
        return;
    }
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Properties");
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    dialog.connect_closed(move |_| c.store(true, Ordering::Relaxed));

    let page = adw::PreferencesPage::new();
    page.set_title("General");
    page.set_icon_name(Some("luma-documents-symbolic"));
    let group = adw::PreferencesGroup::new();
    page.add(&group);
    dialog.add(&page);

    if targets.len() == 1 {
        single(&dialog, &page, &group, &targets[0], &cancel, &state);
    } else {
        multiple(&group, &targets, &cancel);
    }
    dialog.present(Some(parent));
}

fn spawn_size(path: PathBuf, cancel: Arc<AtomicBool>, target: adw::ActionRow, with_counts: bool) {
    target.set_subtitle("Calculating…");
    glib::spawn_future_local(async move {
        let p = path.clone();
        let c = cancel.clone();
        let r = gtk::gio::spawn_blocking(move || {
            let md = std::fs::symlink_metadata(&p).ok();
            match md {
                Some(m) if m.is_dir() => metadata::dir_stats(&p, &c),
                Some(m) => DirStats { bytes: m.len(), files: 1, dirs: 0 },
                None => DirStats::default(),
            }
        })
        .await;
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(s) = r {
            let extra = if with_counts { format!(" · {} files, {} folders", s.files, s.dirs) } else { String::new() };
            target.set_subtitle(&format!("{}{extra}", size_text(s.bytes)));
        }
    });
}

fn single(
    dialog: &adw::PreferencesDialog,
    page: &adw::PreferencesPage,
    group: &adw::PreferencesGroup,
    path: &Path,
    cancel: &Arc<AtomicBool>,
    state: &Rc<AppState>,
) {
    let Ok(md) = std::fs::symlink_metadata(path) else {
        group.add(&row("Error", "This item can no longer be read."));
        return;
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    group.add(&row("Name", &name));
    group.add(&row("Kind", &kind_of(path, &md)));
    group.add(&row("Location", &path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())));
    if md.file_type().is_symlink() {
        let t = std::fs::read_link(path).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
        group.add(&row("Points to", &t));
    }
    let size = row("Size", "");
    group.add(&size);
    spawn_size(path.to_path_buf(), cancel.clone(), size, md.is_dir());

    let times = adw::PreferencesGroup::new();
    times.add(&row("Modified", &fmt_time(md.modified())));
    times.add(&row("Accessed", &fmt_time(md.accessed())));
    times.add(&row("Created", &fmt_time(md.created())));
    page.add(&times);

    let own = adw::PreferencesGroup::new();
    own.add(&row("Owner", &permissions::owner_name(md.uid())));
    own.add(&row("Group", &permissions::group_name(md.gid())));
    own.add(&row(
        "Permissions",
        &format!("{:03o} · {}", md.mode() & 0o7777, permissions::symbolic(md.mode() & 0o7777)),
    ));
    page.add(&own);

    if md.is_file() {
        details_groups(page, path, cancel, state);
        checksum_group(page, path, cancel);
    }
    if !md.file_type().is_symlink() {
        permissions_page(dialog, path, &md, cancel);
    }
}

/// Photo / audio tags, PDF and media info, and plugin-provided metadata.
fn details_groups(page: &adw::PreferencesPage, path: &Path, cancel: &Arc<AtomicBool>, state: &Rc<AppState>) {
    let placeholder = adw::PreferencesGroup::new();
    placeholder.set_title("Details");
    let loading = adw::ActionRow::new();
    loading.set_title("Reading details…");
    placeholder.add(&loading);
    page.add(&placeholder);

    let providers = state.ext.borrow().providers.clone();
    let (p, c, page, ph) = (path.to_path_buf(), cancel.clone(), page.clone(), placeholder);
    glib::spawn_future_local(async move {
        let (p2, c2) = (p.clone(), c.clone());
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mime = gtk::gio::content_type_guess(Some(&name), None).0.to_string();
        let groups = gtk::gio::spawn_blocking(move || {
            crate::integrations::plugins::collect_metadata(&p2, &mime, &providers, &c2)
        })
        .await;
        if c.load(Ordering::Relaxed) {
            return;
        }
        page.remove(&ph);
        for g in groups.unwrap_or_default() {
            let grp = adw::PreferencesGroup::new();
            grp.set_title(&g.title);
            for it in g.items {
                grp.add(&row(&it.key, &it.value));
            }
            page.add(&grp);
        }
    });
}

fn multiple(group: &adw::PreferencesGroup, targets: &[PathBuf], cancel: &Arc<AtomicBool>) {
    group.add(&row("Selection", &format!("{} items", targets.len())));
    let parents: std::collections::HashSet<_> = targets.iter().filter_map(|t| t.parent()).collect();
    if let (1, Some(p)) = (parents.len(), parents.iter().next()) {
        group.add(&row("Location", &p.to_string_lossy()));
    }
    let size = row("Total size", "Calculating…");
    group.add(&size);
    let (targets, cancel) = (targets.to_vec(), cancel.clone());
    glib::spawn_future_local(async move {
        let c = cancel.clone();
        let r = gtk::gio::spawn_blocking(move || {
            let mut t = DirStats::default();
            for p in &targets {
                if let Ok(m) = std::fs::symlink_metadata(p) {
                    if m.is_dir() {
                        let s = metadata::dir_stats(p, &c);
                        t.bytes += s.bytes;
                        t.files += s.files;
                        t.dirs += s.dirs + 1;
                    } else {
                        t.bytes += m.len();
                        t.files += 1;
                    }
                }
            }
            t
        })
        .await;
        if !cancel.load(Ordering::Relaxed)
            && let Ok(s) = r
        {
            size.set_subtitle(&format!("{} · {} files, {} folders", size_text(s.bytes), s.files, s.dirs));
        }
    });
}

fn checksum_group(page: &adw::PreferencesPage, path: &Path, cancel: &Arc<AtomicBool>) {
    let g = adw::PreferencesGroup::new();
    g.set_title("Checksum");
    let labels: Vec<&str> = Algo::ALL.iter().map(|a| a.label()).collect();
    let combo = adw::ComboRow::new();
    combo.set_title("Algorithm");
    combo.set_model(Some(&gtk::StringList::new(&labels)));
    let result = row("Digest", "Not calculated");
    let btn = gtk::Button::with_label("Calculate");
    btn.set_valign(gtk::Align::Center);
    result.add_suffix(&btn);
    g.add(&combo);
    g.add(&result);
    page.add(&g);

    let (path, cancel) = (path.to_path_buf(), cancel.clone());
    btn.connect_clicked(move |b| {
        let algo = Algo::ALL[combo.selected() as usize];
        b.set_sensitive(false);
        result.set_subtitle("Calculating…");
        let (p, c, result, b) = (path.clone(), cancel.clone(), result.clone(), b.clone());
        glib::spawn_future_local(async move {
            let c2 = c.clone();
            let r = gtk::gio::spawn_blocking(move || metadata::checksum(&p, algo, &c2)).await;
            b.set_sensitive(true);
            if c.load(Ordering::Relaxed) {
                return;
            }
            result.set_subtitle(&match r {
                Ok(Ok(Some(s))) => s,
                Ok(Ok(None)) => "Cancelled".into(),
                Ok(Err(e)) => format!("Could not read file: {e}"),
                Err(_) => "Failed".into(),
            });
        });
    });
}

fn permissions_page(dialog: &adw::PreferencesDialog, path: &Path, md: &std::fs::Metadata, cancel: &Arc<AtomicBool>) {
    let page = adw::PreferencesPage::new();
    page.set_title("Permissions");
    page.set_icon_name(Some("luma-lock-symbolic"));
    let mode0 = md.mode() & 0o7777;
    let editable = permissions::can_chmod(md);

    let access = adw::PreferencesGroup::new();
    access.set_title("Access");
    if !editable {
        access.set_description(Some("Only the owner can change permissions."));
    }
    let mut checks: Vec<gtk::CheckButton> = Vec::new();
    for (label, shift) in [("Owner", 6u32), ("Group", 3), ("Others", 0)] {
        let r = adw::ActionRow::new();
        r.set_title(label);
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.set_valign(gtk::Align::Center);
        for (name, bit) in [("Read", 4u32), ("Write", 2), ("Execute", 1)] {
            let c = gtk::CheckButton::with_label(name);
            c.set_active(mode0 & (bit << shift) != 0);
            c.set_sensitive(editable);
            b.append(&c);
            checks.push(c);
        }
        r.add_suffix(&b);
        access.add(&r);
    }
    page.add(&access);

    let special = adw::PreferencesGroup::new();
    special.set_title("Special bits");
    let mut switches = Vec::new();
    for (title, sub, bit) in [
        ("Set user ID", "Run as the file’s owner", SETUID),
        ("Set group ID", "Run as the file’s group (folders: new files inherit the group)", SETGID),
        ("Sticky", "Only owners can delete entries in this folder", STICKY),
    ] {
        let s = adw::SwitchRow::new();
        s.set_title(title);
        s.set_subtitle(sub);
        s.set_active(mode0 & bit != 0);
        s.set_sensitive(editable);
        special.add(&s);
        switches.push((s, bit));
    }
    page.add(&special);

    let mode_row = row("Mode", "");
    let (checks, switches) = (Rc::new(checks), Rc::new(switches));
    let compute = {
        let (checks, switches) = (checks.clone(), switches.clone());
        Rc::new(move || -> u32 {
            let mut m = 0u32;
            for (i, c) in checks.iter().enumerate() {
                if c.is_active() {
                    m |= (4 >> (i % 3)) << (6 - 3 * (i as u32 / 3));
                }
            }
            for (s, bit) in switches.iter() {
                if s.is_active() {
                    m |= bit;
                }
            }
            m
        })
    };
    let refresh = {
        let (compute, mode_row) = (compute.clone(), mode_row.clone());
        Rc::new(move || {
            let m = compute();
            mode_row.set_subtitle(&format!("{m:03o} · {}", permissions::symbolic(m)));
        })
    };
    refresh();
    for c in checks.iter() {
        let r = refresh.clone();
        c.connect_toggled(move |_| r());
    }
    for (s, _) in switches.iter() {
        let r = refresh.clone();
        s.connect_active_notify(move |_| r());
    }

    let owner_group = adw::PreferencesGroup::new();
    owner_group.set_title("Ownership");
    owner_group.add(&row(
        "Owner",
        &format!("{} (changing the owner needs administrator rights)", permissions::owner_name(md.uid())),
    ));
    let mut groups = permissions::assignable_groups();
    if !groups.iter().any(|(g, _)| *g == md.gid()) {
        groups.push((md.gid(), permissions::group_name(md.gid())));
    }
    let group_combo = adw::ComboRow::new();
    group_combo.set_title("Group");
    let gnames: Vec<&str> = groups.iter().map(|(_, n)| n.as_str()).collect();
    group_combo.set_model(Some(&gtk::StringList::new(&gnames)));
    group_combo.set_selected(groups.iter().position(|(g, _)| *g == md.gid()).unwrap_or(0) as u32);
    group_combo.set_sensitive(editable);
    owner_group.add(&group_combo);
    page.add(&owner_group);

    let apply_group = adw::PreferencesGroup::new();
    let scope_combo = adw::ComboRow::new();
    scope_combo.set_title("Apply to");
    scope_combo.set_model(Some(&gtk::StringList::new(&[
        "This item only",
        "Everything inside",
        "Folders inside only",
        "Files inside only",
    ])));
    scope_combo.set_visible(md.is_dir());
    scope_combo.set_sensitive(editable);
    apply_group.add(&scope_combo);
    let apply_row = adw::ActionRow::new();
    let apply = gtk::Button::with_label("Apply Changes");
    apply.add_css_class("suggested-action");
    apply.set_valign(gtk::Align::Center);
    apply.set_sensitive(editable);
    apply_row.add_suffix(&apply);
    apply_row.set_title("Changes affect the real files immediately.");
    apply_group.add(&apply_row);
    page.add(&apply_group);
    page.add(&{
        let g = adw::PreferencesGroup::new();
        g.add(&mode_row);
        g
    });
    dialog.add(&page);

    let (path, cancel, dialog) = (path.to_path_buf(), cancel.clone(), dialog.clone());
    let gid0 = md.gid();
    apply.connect_clicked(move |btn| {
        let mode = compute();
        let scope = match scope_combo.selected() {
            1 => Scope::All,
            2 => Scope::FoldersOnly,
            3 => Scope::FilesOnly,
            _ => Scope::ItemOnly,
        };
        let gid = groups.get(group_combo.selected() as usize).map(|(g, _)| *g).unwrap_or(gid0);
        btn.set_sensitive(false);
        let (p, c, dialog, btn) = (path.clone(), cancel.clone(), dialog.clone(), btn.clone());
        glib::spawn_future_local(async move {
            let r = gtk::gio::spawn_blocking(move || {
                let mut errors: Vec<String> = Vec::new();
                let mut changed = 0;
                if gid != gid0 {
                    match permissions::set_group(&p, gid) {
                        Ok(()) => changed += 1,
                        Err(e) => errors.push(format!("group: {e}")),
                    }
                }
                if mode != mode0 || scope != Scope::ItemOnly {
                    let rep = permissions::apply(&p, mode, scope, &c);
                    changed += rep.changed;
                    errors.extend(rep.errors.into_iter().take(3).map(|(p, e)| format!("{}: {e}", p.display())));
                }
                (changed, errors)
            })
            .await;
            btn.set_sensitive(true);
            let msg = match r {
                Ok((n, e)) if e.is_empty() => format!("Updated {n} item(s)"),
                Ok((n, e)) => format!("Updated {n}; {}", e.join("; ")),
                Err(_) => "Update failed".into(),
            };
            dialog.add_toast(adw::Toast::new(&msg));
        });
    });
}
