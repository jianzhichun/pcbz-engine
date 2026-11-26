#![forbid(unsafe_code)]
//! pcbcraft-render-server: the editing UI with no window. Each frame is drawn offscreen and
//! streamed to the embedding host that started this process; the host sends the user's pointer,
//! keyboard and IME input back; the model drives the same session over upstream's own control
//! channel (`pcbcraft-cli mcp --connect`). Protocol: PROTOCOL.md beside this crate. The host loop
//! (device, egui pass, offscreen paint, read-back, codecs, pacing, input mapping) is SolidZ's
//! render server's.
//!
//! Layers: `domain` (rules, no I/O), `adapters` (GPU, sockets, encoders, logging), `app` (the UI
//! host and the two modes), and this composition root.

mod adapters;
mod app;
mod cli;
mod domain;

use app::snapshot;
use app::stream::{self, StreamArgs};
use cli::{Mode, Parsed};
use domain::geometry::FrameSize;

fn main() {
    adapters::logging::install();
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(o)) => *o,
        Ok(Parsed::Help) => {
            eprintln!("{}", cli::USAGE);
            return;
        }
        Ok(Parsed::Version) => {
            eprintln!("pcbcraft-render-server {} (protocol {})", env!("CARGO_PKG_VERSION"), domain::wire::PROTOCOL_VERSION);
            return;
        }
        Err(e) => {
            eprintln!("pcbcraft-render-server: {e}\n{}", cli::USAGE);
            std::process::exit(stream::EXIT_SETUP);
        }
    };
    if let Some(session) = &options.session {
        log::info!("session {session}");
    }
    let size = FrameSize::new(options.css.0, options.css.1, options.dpr);
    let code = match options.mode {
        Mode::Stream(addr) => stream::run(StreamArgs {
            addr,
            dark: options.dark,
            lang: options.lang.clone(),
            size,
            adapter: options.adapter,
            project: options.project,
            config_dir: options.config_dir,
        }),
        Mode::Snapshot { dir, frames, warmup, gap } => snapshot::run(snapshot::SnapshotArgs {
            dir,
            frames,
            warmup,
            gap,
            dark: options.dark,
            lang: options.lang.clone(),
            size,
            adapter: options.adapter,
            project: options.project,
            config_dir: options.config_dir,
        }),
    };
    std::process::exit(code);
}
