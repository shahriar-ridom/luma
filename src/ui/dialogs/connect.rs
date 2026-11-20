use adw::prelude::*;
use gtk::glib;

use crate::integrations::network;

pub struct ConnectChoice {
    pub uri: String,
    pub name: String,
    pub remember: bool,
}

const NOTE: &str = "Passwords are never saved with the address. You are asked for one when connecting.";

/// Ask for a server address; re-asks (with the reason) until it is valid.
pub async fn ask(parent: &gtk::Window) -> Option<ConnectChoice> {
    let mut error: Option<String> = None;
    // What was typed survives a rejected address.
    let (mut last, mut last_name, mut last_remember) = (String::new(), String::new(), true);
    loop {
        let hint: Vec<String> = network::PROTOCOLS.iter().map(|(p, l)| format!("{p}:// — {l}")).collect();
        let body = match &error {
            Some(e) => format!("{e}\n\n{}\n\n{NOTE}", hint.join("\n")),
            None => format!("{}\n\n{NOTE}", hint.join("\n")),
        };
        let d = adw::AlertDialog::new(Some("Connect to Server"), Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("ok", "Connect")]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");

        let addr = gtk::Entry::new();
        addr.set_placeholder_text(Some("sftp://user@host/folder"));
        addr.set_text(&last);
        addr.set_activates_default(true);
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("Name (optional)"));
        name.set_text(&last_name);
        let remember = gtk::CheckButton::with_label("Remember this server");
        remember.set_active(last_remember);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        col.append(&addr);
        col.append(&name);
        col.append(&remember);
        d.set_extra_child(Some(&col));
        let a = addr.clone();
        glib::idle_add_local_once(move || {
            a.grab_focus();
        });

        if d.choose_future(Some(parent)).await != "ok" {
            return None;
        }
        last = addr.text().to_string();
        last_name = name.text().to_string();
        last_remember = remember.is_active();
        match network::normalize(&last) {
            Ok(uri) => {
                return Some(ConnectChoice { uri, name: last_name.trim().to_string(), remember: last_remember });
            }
            Err(e) => error = Some(e),
        }
    }
}
