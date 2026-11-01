use adw::prelude::*;
use gtk::glib;

use crate::integrations::cloud;

pub fn explain_missing_rclone(parent: &gtk::Window) {
    super::error(
        parent,
        "rclone is needed for Google Drive",
        "Google Drive is connected through rclone and FUSE, so Drive appears as a normal folder.\n\n\
         Install them, then try again:\n  Arch / CachyOS: sudo pacman -S rclone fuse3\n  Debian / Ubuntu: sudo apt install rclone fuse3\n  Fedora: sudo dnf install rclone fuse3\n\n\
         If you use GNOME with an online account set up, you can instead connect with the address google-drive://you@gmail.com/ using “Connect to Server”.",
    );
}

/// Ask for an account name and access level.
pub async fn ask_new(parent: &gtk::Window, taken: &[String]) -> Option<(String, bool)> {
    let mut error: Option<String> = None;
    let default = (1..)
        .map(|i| if i == 1 { "gdrive".to_string() } else { format!("gdrive{i}") })
        .find(|n| !taken.contains(n))
        .unwrap();
    let mut last = default;
    loop {
        let body = error.clone().unwrap_or_else(|| "Your browser opens so you can sign in to Google. The app never sees your password; rclone stores the access token in its own config.".into());
        let d = adw::AlertDialog::new(Some("Add Google Drive"), Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("ok", "Sign In")]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        let name = gtk::Entry::new();
        name.set_text(&last);
        name.set_activates_default(true);
        let access = gtk::DropDown::from_strings(&["Full access (read and write)", "Read-only"]);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        col.append(&gtk::Label::builder().label("Name for this drive").xalign(0.0).build());
        col.append(&name);
        col.append(&access);
        d.set_extra_child(Some(&col));
        let n = name.clone();
        glib::idle_add_local_once(move || {
            n.grab_focus();
        });
        if d.choose_future(Some(parent)).await != "ok" {
            return None;
        }
        last = name.text().trim().to_string();
        if !cloud::valid_name(&last) {
            error = Some("Use letters, numbers, - and _ only.".into());
        } else if taken.contains(&last) {
            error = Some(format!("A drive called “{last}” already exists."));
        } else {
            return Some((last, access.selected() == 1));
        }
    }
}

pub async fn confirm_remove(parent: &gtk::Window, name: &str) -> bool {
    let d = adw::AlertDialog::new(
        Some("Remove this Google Drive?"),
        Some(&format!(
            "“{name}” is signed out and its saved token is deleted. Your files on Google Drive are not touched."
        )),
    );
    d.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    d.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "remove"
}
