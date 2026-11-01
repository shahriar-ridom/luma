// GTK signal handlers are naturally `Rc<dyn Fn(..)>`-shaped; aliasing each adds noise.
#![allow(clippy::type_complexity)]

mod application;
mod core;
mod database;
mod integrations;
mod services;
mod settings;
mod ui;

use gtk::glib;

fn main() -> glib::ExitCode {
    // `RUST_LOG=debug luma` for diagnostics; warnings only by default.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    application::app::run()
}
