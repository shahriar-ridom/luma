//! Manager for custom actions and plugins.

use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc};

use crate::application::state::AppState;
use crate::database::CustomActionRow;
use crate::integrations::plugins::{parse_command, user_plugin_dir};
use crate::settings::persistence;

/// Add or edit a custom action; re-asks until the command parses.
async fn edit_action(parent: &gtk::Window, existing: Option<CustomActionRow>) -> Option<CustomActionRow> {
    let mut cur = existing.clone().unwrap_or(CustomActionRow {
        id: 0,
        name: String::new(),
        command: String::new(),
        patterns: "any".into(),
        wait: false,
    });
    let mut error: Option<String> = None;
    loop {
        let help = "Placeholders: %f first file · %F all files · %d folder · %n name · %b name without extension · %u URI.\nApplies to: files, folders, any, or patterns like *.png, image/*.";
        let body = error.clone().map(|e| format!("{e}\n\n{help}")).unwrap_or_else(|| help.to_string());
        let d = adw::AlertDialog::new(Some(if existing.is_some() { "Edit Action" } else { "New Action" }), Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("ok", "Save")]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_close_response("cancel");
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("Name, e.g. Open in VS Code"));
        name.set_text(&cur.name);
        let cmd = gtk::Entry::new();
        cmd.set_placeholder_text(Some("code %F"));
        cmd.set_text(&cur.command);
        let pat = gtk::Entry::new();
        pat.set_placeholder_text(Some("any"));
        pat.set_text(&cur.patterns);
        let wait = gtk::CheckButton::with_label("Wait for it to finish and report the result");
        wait.set_active(cur.wait);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        for w in [name.upcast_ref::<gtk::Widget>(), cmd.upcast_ref(), pat.upcast_ref(), wait.upcast_ref()] {
            col.append(w);
        }
        d.set_extra_child(Some(&col));
        if d.choose_future(Some(parent)).await != "ok" {
            return None;
        }
        cur = CustomActionRow {
            id: cur.id,
            name: name.text().trim().to_string(),
            command: cmd.text().trim().to_string(),
            patterns: pat.text().trim().to_string(),
            wait: wait.is_active(),
        };
        if cur.name.is_empty() {
            error = Some("Give the action a name.".into());
        } else if let Err(e) = parse_command(&cur.command) {
            error = Some(e);
        } else {
            return Some(cur);
        }
    }
}

pub fn show(parent: &gtk::Window, state: Rc<AppState>, on_change: Rc<dyn Fn()>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Custom Actions & Plugins");

    // ---- custom actions page ----
    let actions_page = adw::PreferencesPage::new();
    actions_page.set_title("Actions");
    actions_page.set_icon_name(Some("fm-bolt-symbolic"));
    let actions_group = adw::PreferencesGroup::new();
    actions_group.set_title("Your actions");
    actions_group.set_description(Some("They appear under “Actions” in the right-click menu for matching files. Commands run directly, without a shell."));
    let add = gtk::Button::with_label("Add Action");
    add.add_css_class("suggested-action");
    actions_group.set_header_suffix(Some(&add));
    actions_page.add(&actions_group);
    dialog.add(&actions_page);

    // ---- plugins page ----
    let plugins_page = adw::PreferencesPage::new();
    plugins_page.set_title("Plugins");
    plugins_page.set_icon_name(Some("fm-options-symbolic"));
    let info = adw::PreferencesGroup::new();
    info.set_description(Some(&format!(
        "A plugin is a folder containing plugin.toml. Install one into:\n{}\n\nPlugins run external programs only when you use one of their actions, or open Properties for a matching file. New plugins start switched off — read what they run before enabling.",
        user_plugin_dir().display()
    )));
    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btns.set_halign(gtk::Align::Start);
    let open_dir = gtk::Button::with_label("Open Plugins Folder");
    let reload = gtk::Button::with_label("Reload");
    btns.append(&open_dir);
    btns.append(&reload);
    info.add(&btns);
    plugins_page.add(&info);
    let plugin_group = adw::PreferencesGroup::new();
    plugins_page.add(&plugin_group);
    let problem_group = adw::PreferencesGroup::new();
    plugins_page.add(&problem_group);
    dialog.add(&plugins_page);

    let rows: Rc<RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>> = Rc::default();
    let refresh: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();

    let rebuild = {
        let (state, rows, on_change, refresh, parent) =
            (state.clone(), rows.clone(), on_change.clone(), refresh.clone(), parent.clone());
        let (actions_group, plugin_group, problem_group) =
            (actions_group.clone(), plugin_group.clone(), problem_group.clone());
        Rc::new(move || {
            state.reload_extensions();
            on_change();
            for (g, w) in rows.borrow_mut().drain(..) {
                g.remove(&w);
            }
            let again = refresh.clone();
            let redo = Rc::new(move || {
                if let Some(f) = again.borrow().clone() {
                    f();
                }
            });

            for a in state.db.custom_actions().unwrap_or_default() {
                let row = adw::ActionRow::new();
                row.set_use_markup(false);
                row.set_title(&a.name);
                row.set_subtitle(&format!(
                    "{}\nApplies to: {}{}",
                    a.command,
                    if a.patterns.is_empty() { "any" } else { &a.patterns },
                    if a.wait { " · waits for result" } else { "" }
                ));
                let edit = gtk::Button::from_icon_name("fm-edit-symbolic");
                let del = gtk::Button::from_icon_name("fm-trash-symbolic");
                for b in [&edit, &del] {
                    b.add_css_class("flat");
                    b.set_valign(gtk::Align::Center);
                }
                row.add_suffix(&edit);
                row.add_suffix(&del);
                let (st, p, r, a2) = (state.clone(), parent.clone(), redo.clone(), a.clone());
                edit.connect_clicked(move |_| {
                    let (st, p, r, a2) = (st.clone(), p.clone(), r.clone(), a2.clone());
                    glib::spawn_future_local(async move {
                        if let Some(n) = edit_action(&p, Some(a2)).await {
                            let _ = st.db.save_custom_action(&n);
                            r();
                        }
                    });
                });
                let (st, r, id) = (state.clone(), redo.clone(), a.id);
                del.connect_clicked(move |_| {
                    let _ = st.db.delete_custom_action(id);
                    r();
                });
                actions_group.add(&row);
                rows.borrow_mut().push((actions_group.clone(), row.upcast()));
            }

            let ext = state.ext.borrow();
            if ext.plugins.is_empty() {
                let r = adw::ActionRow::new();
                r.set_title("No plugins installed");
                plugin_group.add(&r);
                rows.borrow_mut().push((plugin_group.clone(), r.upcast()));
            }
            for p in &ext.plugins {
                let row = adw::SwitchRow::new();
                row.set_use_markup(false);
                row.set_title(&format!("{} {}", p.name, p.version));
                row.set_subtitle(&format!(
                    "{}\n{} action{}, {} metadata provider{} · {}",
                    p.description,
                    p.actions,
                    if p.actions == 1 { "" } else { "s" },
                    p.metadata,
                    if p.metadata == 1 { "" } else { "s" },
                    p.dir.display()
                ));
                row.set_active(p.enabled);
                let (st, r, id) = (state.clone(), redo.clone(), p.id.clone());
                row.connect_active_notify(move |sw| {
                    if st.prefs.plugin_enabled(&id) != sw.is_active() {
                        persistence::save_plugin_enabled(&st.db, &st.prefs, &id, sw.is_active());
                        r();
                    }
                });
                plugin_group.add(&row);
                rows.borrow_mut().push((plugin_group.clone(), row.upcast()));
            }
            for (dir, why) in &ext.problems {
                let r = adw::ActionRow::new();
                r.set_use_markup(false);
                r.set_title(&format!(
                    "Couldn’t load {}",
                    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ));
                r.set_subtitle(why);
                r.add_css_class("error");
                problem_group.add(&r);
                rows.borrow_mut().push((problem_group.clone(), r.upcast()));
            }
        })
    };
    let rb = rebuild.clone();
    *refresh.borrow_mut() = Some(Rc::new(move || rb()));
    rebuild();

    let (st, p, r) = (state.clone(), parent.clone(), rebuild.clone());
    add.connect_clicked(move |_| {
        let (st, p, r) = (st.clone(), p.clone(), r.clone());
        glib::spawn_future_local(async move {
            if let Some(n) = edit_action(&p, None).await {
                let _ = st.db.save_custom_action(&n);
                r();
            }
        });
    });
    let r = rebuild;
    reload.connect_clicked(move |_| r());
    open_dir.connect_clicked(move |_| {
        let dir = user_plugin_dir();
        let _ = std::fs::create_dir_all(&dir);
        gtk::FileLauncher::new(Some(&gio::File::for_path(&dir))).launch(
            None::<&gtk::Window>,
            gio::Cancellable::NONE,
            |_| {},
        );
    });
    dialog.present(Some(parent));
}
