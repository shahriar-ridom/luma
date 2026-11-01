//! Custom actions and plugin actions in the context menus.

use super::*;
use crate::integrations::plugins::{Item, expand, run_command, spawn_detached};

impl Pane {
    /// What an action would apply to: the selection, or this folder if nothing is selected.
    fn ext_targets(&self) -> (Vec<Item>, Vec<PathBuf>) {
        let Some(dir) = self.cur_dir() else { return (vec![], vec![]) };
        let infos = self.list.selected_infos();
        if infos.is_empty() {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
            return (vec![Item { name, is_dir: true, mime: "inode/directory".into() }], vec![dir]);
        }
        let items = infos
            .iter()
            .map(|i| Item {
                name: i.name().to_string_lossy().into_owned(),
                is_dir: filesystem::is_dir(i),
                mime: i.content_type().map(|c| c.to_string()).unwrap_or_default(),
            })
            .collect();
        (items, infos.iter().map(|i| dir.join(i.name())).collect())
    }

    /// "Actions" submenu for the current target, grouped by origin.
    pub(super) fn ext_menu(&self) -> Option<gio::Menu> {
        let (items, _) = self.ext_targets();
        let ext = self.env.state.ext.borrow();
        let matching = ext.matching(&items);
        if matching.is_empty() {
            return None;
        }
        let menu = gio::Menu::new();
        let mut groups: Vec<(&str, gio::Menu)> = Vec::new();
        for a in matching {
            let m = match groups.iter().position(|(g, _)| *g == a.group) {
                Some(i) => &groups[i].1,
                None => {
                    groups.push((a.group.as_str(), gio::Menu::new()));
                    &groups.last().unwrap().1
                }
            };
            let it = gio::MenuItem::new(Some(&a.name), None);
            it.set_action_and_target_value(Some("win.ext-action"), Some(&a.key.to_variant()));
            m.append_item(&it);
        }
        for (g, m) in groups {
            menu.append_section(Some(g), &m);
        }
        Some(menu)
    }

    pub fn run_ext(self: &Rc<Self>, key: &str) {
        let (items, paths) = self.ext_targets();
        let Some(dir) = self.cur_dir() else { return };
        let Some(action) = self.env.state.ext.borrow().find(key).cloned() else { return };
        if !action.matcher.matches(&items) {
            self.env.ops.toast("That action doesn't apply to this selection");
            return;
        }
        // Run inside the folder being shown (for a folder target, inside it).
        let cwd = if paths.len() == 1 && paths[0].is_dir() && self.list.selected_count() == 0 {
            paths[0].clone()
        } else {
            dir.clone()
        };
        let argv = expand(&action.argv, &paths, &cwd);
        if action.wait {
            let (name, t) = (action.name.clone(), self.clone());
            self.env.ops.run_job(
                &action.name,
                move |cancel| run_command(&argv, &cwd, &cancel),
                move |r| match r {
                    Ok(last) => t.env.ops.toast(&if last.is_empty() {
                        format!("{name} finished")
                    } else {
                        format!("{name}: {last}")
                    }),
                    Err(e) if e == "Cancelled" => t.env.ops.toast("Cancelled"),
                    Err(e) => t.env.ops.toast(&format!("{name} failed: {e}")),
                },
            );
        } else if let Err(e) = spawn_detached(&argv, &cwd) {
            self.env.ops.toast(&format!("{}: {e}", action.name));
        }
    }

    pub fn open_with(&self) {
        let paths = self.selected_paths();
        let Some(first) = paths.into_iter().find(|p| !p.is_dir()) else { return };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&first)));
        launcher.set_always_ask(true);
        launcher.launch(self.window().as_ref(), gio::Cancellable::NONE, |_| {});
    }
}
