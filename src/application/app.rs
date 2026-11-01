use adw::prelude::*;
use gtk::{gio, glib};

use crate::application::state::AppState;
use crate::ui::window::FmWindow;

pub const APP_ID: &str = "dev.filemanager.FileManager";

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    let state = AppState::open();
    app.connect_startup(move |_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("../../data/style.css"));
        if let Some(d) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&d, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            // Our icon family ships inside the binary (and in packages).
            match crate::application::resources::icon_search_path(&glib::user_cache_dir()) {
                Ok(dir) => gtk::IconTheme::for_display(&d).add_search_path(dir),
                Err(e) => tracing::warn!("could not unpack icons: {e}"),
            }
        }
        gtk::Window::set_default_icon_name(APP_ID);
    });
    let activate_state = state.clone();
    app.connect_activate(move |app| {
        // First window restores the previous session; later ones start at home.
        let first = app.windows().is_empty();
        let (paths, active) =
            if first { activate_state.db.load_session().unwrap_or_default() } else { (vec![glib::home_dir()], 0) };
        FmWindow::new(app, activate_state.clone(), first, paths, active).present();
    });
    // `file-manager /some/folder file.txt` and desktop-entry %U launches.
    let st = state.clone();
    app.connect_open(move |app, files, _| {
        let paths: Vec<std::path::PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        let existing = app.windows().into_iter().find_map(|w| w.downcast::<FmWindow>().ok());
        match existing {
            Some(w) => w.open_locations(paths, false),
            None => {
                let (session, active) = st.db.load_session().unwrap_or_default();
                let w = FmWindow::new(app, st.clone(), true, session, active);
                w.open_locations(paths, false);
            }
        }
    });
    own_file_manager_bus_name(&app, state.clone());
    app.run()
}

const FILE_MANAGER1: &str = r#"<node><interface name="org.freedesktop.FileManager1">
  <method name="ShowFolders"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  <method name="ShowItems"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  <method name="ShowItemProperties"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
</interface></node>"#;

/// Answer other apps' "show in folder" requests (org.freedesktop.FileManager1).
/// The name is taken only if nobody else owns it, so another file manager is
/// never displaced.
fn own_file_manager_bus_name(app: &adw::Application, state: std::rc::Rc<crate::application::state::AppState>) {
    let Ok(info) = gio::DBusNodeInfo::for_xml(FILE_MANAGER1) else { return };
    let Some(iface) = info.lookup_interface("org.freedesktop.FileManager1") else { return };
    let app = app.clone();
    gio::bus_own_name(
        gio::BusType::Session,
        "org.freedesktop.FileManager1",
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            let (app, state) = (app.clone(), state.clone());
            let res = conn
                .register_object("/org/freedesktop/FileManager1", &iface)
                .method_call(move |_c, _sender, _path, _iface, method, params, invocation| {
                    let uris: Vec<String> = params.child_value(0).get().unwrap_or_default();
                    let paths: Vec<std::path::PathBuf> =
                        uris.iter().filter_map(|u| gio::File::for_uri(u).path()).collect();
                    let window =
                        app.windows().into_iter().find_map(|w| w.downcast::<FmWindow>().ok()).unwrap_or_else(|| {
                            let (session, active) = state.db.load_session().unwrap_or_default();
                            FmWindow::new(&app, state.clone(), true, session, active)
                        });
                    match method {
                        "ShowFolders" => window.open_locations(paths, false),
                        // Files are selected inside their folder; folders themselves are opened.
                        _ => window.open_locations(paths, method == "ShowItemProperties"),
                    }
                    invocation.return_value(None);
                })
                .build();
            if let Err(e) = res {
                tracing::debug!("FileManager1 registration failed: {e}");
            }
        },
        |_, _| {},
        |_, name| tracing::debug!("{name} not acquired (another file manager owns it)"),
    );
}
