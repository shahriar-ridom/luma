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
    // `luma --cleanup-run` is what the systemd timer starts. It must work with
    // no display, so it is handled before GTK or logging are touched.
    let args: Vec<String> = std::env::args().collect();
    match core::diskmgr::headless::parse_args(&args) {
        Ok(Some(a)) => std::process::exit(core::diskmgr::headless::main(a)),
        Ok(None) => {}
        Err(e) => {
            eprintln!("luma: {e}\nUsage: luma --cleanup-run [--dry-run] [--config FILE]");
            std::process::exit(2);
        }
    }
    // `RUST_LOG=debug luma` for diagnostics; warnings only by default.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    application::app::run()
}
