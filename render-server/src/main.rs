#![forbid(unsafe_code)]
//! pcbcraft-render-server: the editing UI with no window. M1 is the snapshot mode: open a project,
//! draw N frames offscreen on the headless graphics device, write each as a PNG. The host loop
//! (device, egui pass, offscreen paint, read-back) is SolidZ's render server's.
//!
//! Layers: `domain` (rules, no I/O), `adapters` (GPU, encoder, logging), `app` (the UI host and
//! the mode), and this composition root.

mod adapters;
mod app;
mod cli;
mod domain;

use app::snapshot::{self, EXIT_SETUP};
use cli::Parsed;

fn main() {
    adapters::logging::install();
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(o)) => *o,
        Ok(Parsed::Help) => {
            eprintln!("{}", cli::USAGE);
            return;
        }
        Ok(Parsed::Version) => {
            eprintln!("pcbcraft-render-server {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Err(e) => {
            eprintln!("pcbcraft-render-server: {e}\n{}", cli::USAGE);
            std::process::exit(EXIT_SETUP);
        }
    };
    std::process::exit(snapshot::run(options));
}
