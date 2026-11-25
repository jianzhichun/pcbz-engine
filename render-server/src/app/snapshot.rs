//! Snapshot mode (M1): no host. Opens the project, draws N frames of the full editor offscreen and
//! writes each as a PNG, with `frames.json` (adapter, sizes, per-frame timing, the project's
//! revision). No window is created and no OS dialog can open. Shaped after SolidZ's render server
//! `app/snapshot.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::editor_host::EditorHost;
use std::path::PathBuf;

use crate::adapters::encode;
use crate::adapters::gpu::{self, Gpu, Policy};
use crate::adapters::painter::Painted;
use crate::domain::geometry::FrameSize;

pub const EXIT_OK: i32 = 0;
pub const EXIT_SETUP: i32 = 2;
pub const EXIT_DEVICE_LOST: i32 = 3;
pub const EXIT_OPEN: i32 = 4;

pub struct SnapshotArgs {
    pub dir: PathBuf,
    pub frames: u32,
    pub warmup: u32,
    pub gap: Duration,
    pub dark: bool,
    pub size: FrameSize,
    pub adapter: Policy,
    pub project: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
}

/// Longest wait for the project to load before giving up.
const OPEN_WAIT: Duration = Duration::from_secs(120);
/// Pause between passes while the project loads.
const OPEN_POLL: Duration = Duration::from_millis(10);

pub fn run(options: SnapshotArgs) -> i32 {
    if let Err(e) = std::fs::create_dir_all(&options.dir) {
        eprintln!("render-server: cannot make {}: {e}", options.dir.display());
        return EXIT_SETUP;
    }
    let size = options.size;
    let started = Instant::now();
    let gpu = match gpu::open(options.adapter) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("render-server: {e}");
            return EXIT_SETUP;
        }
    };
    let device_ms = started.elapsed().as_secs_f32() * 1000.0;
    eprintln!("graphics: {} {} ({}, software {})", gpu.backend, gpu.adapter, gpu.device_type, gpu.software);
    let started = Instant::now();
    let recovery = options.config_dir.as_ref().map(|d| d.join("recovery"));
    let host = EditorHost::new(egui::Context::default(), &gpu, options.dark, size, options.project.clone(), recovery);
    let mut run = Snapshot { gpu, host };
    run.go(&options, device_ms, started)
}

struct Snapshot {
    gpu: Gpu,
    host: EditorHost,
}

impl Snapshot {
    fn go(&mut self, options: &SnapshotArgs, device_ms: f32, started: Instant) -> i32 {
        let mut open_ms = None;
        if options.project.is_some() {
            let began = Instant::now();
            let until = began + OPEN_WAIT;
            loop {
                self.host.pass(Vec::new());
                if !self.host.app.is_busy() {
                    break;
                }
                if Instant::now() > until {
                    eprintln!("render-server: the project did not finish opening in {} s", OPEN_WAIT.as_secs());
                    return EXIT_OPEN;
                }
                std::thread::sleep(OPEN_POLL);
            }
            open_ms = Some(began.elapsed().as_secs_f32() * 1000.0);
            if self.host.app.open_revision().is_none() {
                eprintln!("render-server: the project did not open: {}", self.host.app.status_text().unwrap_or("no reason given"));
                return EXIT_OPEN;
            }
        }
        // Passes only, nothing painted: a software adapter (WARP) draws a frame slower than a pass
        // takes, and the first read-back would otherwise wait for every frame queued before it.
        // The texture changes the passes make are kept and applied by the first painted frame.
        for _ in 0..options.warmup {
            self.host.pass(Vec::new());
            std::thread::sleep(options.gap);
        }
        let mut rows = Vec::new();
        for n in 1..=options.frames {
            let (pass_ms, paint_ms) = match self.frame() {
                Ok(t) => t,
                Err(()) => return EXIT_DEVICE_LOST,
            };
            let t = Instant::now();
            let mut rgba = Vec::new();
            let written = self.host.read_frame(&self.gpu, &mut rgba).and_then(|()| {
                let readback_ms = t.elapsed().as_secs_f32() * 1000.0;
                let t = Instant::now();
                let size = self.host.size();
                let png = encode::png(&rgba, size.px_w, size.px_h)?;
                let name = format!("frame-{n:04}.png");
                std::fs::write(options.dir.join(&name), png).map_err(|e| e.to_string())?;
                Ok((name, readback_ms, t.elapsed().as_secs_f32() * 1000.0))
            });
            match written {
                Ok((file, readback, encode)) => rows.push(json!({
                    "n": n,
                    "file": file,
                    "timing": {"pass": pass_ms, "paint": paint_ms, "readback": readback, "encode": encode},
                    "revision": self.host.app.open_revision(),
                })),
                Err(e) => {
                    eprintln!("render-server: frame {n}: {e}");
                    // what was learned so far, for a run nobody watched
                    let _ = self.write_report(&options.dir, options, rows, device_ms, open_ms, started, Some(&e));
                    return EXIT_SETUP;
                }
            }
            std::thread::sleep(options.gap);
        }
        if let Err(e) = self.write_report(&options.dir, options, rows, device_ms, open_ms, started, None) {
            eprintln!("render-server: frames.json: {e}");
            return EXIT_SETUP;
        }
        eprintln!("frames done");
        EXIT_OK
    }

    /// One pass and its paint (screenshots answered): the pass's and the paint's milliseconds.
    fn frame(&mut self) -> Result<(f32, f32), ()> {
        let pass = self.host.pass(Vec::new());
        let (painted, paint_ms) = self.host.paint(&self.gpu, pass.shapes, pass.pixels_per_point);
        if let Painted::Lost(why) = painted {
            eprintln!("render-server: the graphics device was lost: {why}");
            return Err(());
        }
        self.host.answer_screenshots(&self.gpu, pass.screenshots);
        Ok((pass.ms, paint_ms))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_report(
        &self,
        dir: &Path,
        options: &SnapshotArgs,
        frames: Vec<Value>,
        device_ms: f32,
        open_ms: Option<f32>,
        started: Instant,
        error: Option<&str>,
    ) -> Result<(), String> {
        let size = self.host.size();
        let report = json!({
            "adapter": self.gpu.adapter,
            "backend": self.gpu.backend,
            "deviceType": self.gpu.device_type,
            "software": self.gpu.software,
            "size": {"w": size.css_w, "h": size.css_h, "dpr": size.dpr, "pxW": size.px_w, "pxH": size.px_h},
            "project": options.project.as_ref().map(|p| p.display().to_string()),
            "projectOpened": self.host.app.open_revision().is_some(),
            "status": self.host.app.status_text(),
            "error": error,
            "timing": {"deviceMs": device_ms, "openMs": open_ms, "editorAndFramesMs": started.elapsed().as_secs_f32() * 1000.0},
            "frames": frames,
        });
        let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("frames.json"), text).map_err(|e| e.to_string())
    }
}
