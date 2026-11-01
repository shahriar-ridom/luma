use adw::prelude::*;
use gtk::glib;

use crate::integrations::network;

pub struct ConnectChoice {
    pub uri: String,
    pub name: String,
    pub remember: bool,
}

/// Ask for a server address; re-asks (with the reason) until it is valid.
pub async fn ask(parent: &gtk::Window) -> Option<ConnectChoice> {
    let mut error: Option<String> = None;
    let mut last = String::new();
    loop {
        let hint: Vec<String> = network::PROTOCOLS.iter().map(|(p, l)| format!("{p}:// — {l}")).collect();
        let body = match &error {
            Some(e) => format!("{e}\n\n{}", hint.join("\n")),
            None => hint.join("\n"),
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
        let remember = gtk::CheckButton::with_label("Remember this server");
        remember.set_active(true);
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
        match network::normalize(&last) {
            Ok(uri) => {
                return Some(ConnectChoice {
                    uri,
                    name: name.text().trim().to_string(),
                    remember: remember.is_active(),
                });
            }
            Err(e) => error = Some(e),
        }
    }
}
