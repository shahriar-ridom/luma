//! Keyboard shortcut editor.

use adw::prelude::*;
use gtk::gdk;
use std::{cell::RefCell, rc::Rc};

use crate::application::{actions::ACTIONS, state::AppState};
use crate::settings::persistence;

fn label_for(accel: &str) -> String {
    match gtk::accelerator_parse(accel) {
        Some((key, mods)) if !accel.is_empty() => gtk::accelerator_get_label(key, mods).to_string(),
        _ => "Disabled".into(),
    }
}

pub fn show(parent: &gtk::Window, state: Rc<AppState>, on_change: Rc<dyn Fn()>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Keyboard Shortcuts");
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_description(Some(
        "Click a shortcut, then press the new key combination. Backspace disables it, Esc cancels.",
    ));
    page.add(&group);
    dialog.add(&page);

    let buttons: Rc<RefCell<Vec<(&'static str, gtk::Button, gtk::Button)>>> = Rc::default();
    let capturing: Rc<RefCell<Option<&'static str>>> = Rc::default();

    let refresh = {
        let (buttons, state, capturing) = (buttons.clone(), state.clone(), capturing.clone());
        Rc::new(move || {
            for (id, b, reset) in buttons.borrow().iter() {
                let waiting = *capturing.borrow() == Some(*id);
                b.set_label(&if waiting { "Press keys…".to_string() } else { label_for(&state.prefs.accel(id)) });
                reset.set_visible(state.prefs.is_overridden(id));
            }
        })
    };

    for def in ACTIONS.iter().filter(|a| a.configurable) {
        let row = adw::ActionRow::new();
        row.set_title(def.label);
        let btn = gtk::Button::new();
        btn.set_valign(gtk::Align::Center);
        let reset = gtk::Button::from_icon_name("fm-undo-symbolic");
        reset.set_tooltip_text(Some("Reset to default"));
        reset.set_valign(gtk::Align::Center);
        reset.add_css_class("flat");
        row.add_suffix(&reset);
        row.add_suffix(&btn);
        group.add(&row);
        buttons.borrow_mut().push((def.id, btn.clone(), reset.clone()));

        let (cap, rf) = (capturing.clone(), refresh.clone());
        btn.connect_clicked(move |b| {
            *cap.borrow_mut() = Some(def.id);
            rf();
            b.grab_focus();
        });
        let (st, ch, rf) = (state.clone(), on_change.clone(), refresh.clone());
        reset.connect_clicked(move |_| {
            persistence::reset_shortcut(&st.db, &st.prefs, def.id);
            ch();
            rf();
        });
    }

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (cap, st, ch, rf) = (capturing, state, on_change, refresh.clone());
    key.connect_key_pressed(move |_, keyval, _, mods| {
        let Some(id) = *cap.borrow() else { return gtk::glib::Propagation::Proceed };
        let finish = |accel: Option<String>| {
            if let Some(accel) = accel {
                if let Some(other) = st.prefs.conflict(id, &accel) {
                    persistence::save_shortcut(&st.db, &st.prefs, other, "");
                }
                persistence::save_shortcut(&st.db, &st.prefs, id, &accel);
                ch();
            }
            *cap.borrow_mut() = None;
            rf();
        };
        match keyval {
            gdk::Key::Escape => finish(None),
            gdk::Key::BackSpace => finish(Some(String::new())),
            _ => {
                let mods = mods & gtk::accelerator_get_default_mod_mask();
                let k = keyval.to_lower();
                if gtk::accelerator_valid(k, mods) {
                    finish(Some(gtk::accelerator_name(k, mods).to_string()));
                }
            }
        }
        gtk::glib::Propagation::Stop
    });
    dialog.add_controller(key);
    refresh();
    dialog.present(Some(parent));
}
