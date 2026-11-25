//! The editor with no window: upstream's `PcbApp` assembled by `PcbApp::new_embedded` (patch 0002),
//! driven one pass at a time by synthesized `RawInput` (`ctx.run_ui(raw, |ui| app.frame(ui))`, the
//! way upstream's own `performance/project.rs` drives it) and painted offscreen. Ported from SolidZ's
//! `app/ui_host.rs`: same `RawInput` assembly, viewport commands, texture deltas, tessellate and
//! screenshot answering; the CAD-specific assembly (kernel session, 3D viewport, control channel)
//! is replaced by the one constructor.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pcbcraft_app::PcbApp;

use crate::adapters::gpu::Gpu;
use crate::adapters::painter::{OffscreenPainter, Painted};
use crate::adapters::readback::Readback;
use crate::domain::geometry::FrameSize;

/// One pass's results the loop acts on.
pub struct Pass {
    pub shapes: Vec<egui::epaint::ClippedShape>,
    pub pixels_per_point: f32,
    /// When the UI wants the next pass (`Duration::MAX`: not until something happens).
    pub repaint_delay: Duration,
    /// Screenshot requests: the frame painted after this pass, read back.
    pub screenshots: Vec<egui::ScreenshotCallback>,
    pub ms: f32,
}

pub struct EditorHost {
    pub ctx: egui::Context,
    pub app: PcbApp,
    started: Instant,
    size: FrameSize,
    theme: egui::Theme,
    max_texture_side: usize,
    /// Texture changes since the last paint: passes that are not painted keep theirs for the
    /// next one, as eframe's `pending_deltas` do.
    deltas: egui::TexturesDelta,
    painter: OffscreenPainter,
    shots: Readback,
}

impl EditorHost {
    /// The editor on `gpu`, opening `project` (a `.pc` archive or project folder) on its first pass.
    pub fn new(gpu: &Gpu, dark: bool, size: FrameSize, project: Option<PathBuf>) -> Self {
        let ctx = egui::Context::default();
        // Detached editor windows become in-window windows: there is no OS to give them windows
        // (egui's default, said out loud).
        ctx.set_embed_viewports(true);
        let app = PcbApp::new_embedded(&ctx, project);
        EditorHost {
            max_texture_side: gpu.rs.device.limits().max_texture_dimension_2d as usize,
            ctx,
            app,
            started: Instant::now(),
            size,
            theme: if dark { egui::Theme::Dark } else { egui::Theme::Light },
            deltas: egui::TexturesDelta::default(),
            painter: OffscreenPainter::default(),
            shots: Readback::default(),
        }
    }

    pub fn size(&self) -> FrameSize {
        self.size
    }

    /// One UI pass over `events` (mirrors eframe: `native/epi_integration.rs`, `run_ui` after
    /// `raw_input_hook`).
    pub fn pass(&mut self, events: Vec<egui::Event>) -> Pass {
        let t0 = Instant::now();
        // CSS px per UI point: the app's own zoom preference (1 unless it changes it).
        let zoom = self.ctx.zoom_factor();
        let points = egui::vec2(self.size.css_w as f32 / zoom, self.size.css_h as f32 / zoom);
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, points);
        // Never minimized or occluded: the model's screenshots must still be answered.
        let info = egui::ViewportInfo {
            native_pixels_per_point: Some(self.size.dpr),
            monitor_size: Some(points),
            inner_rect: Some(rect),
            outer_rect: Some(rect),
            minimized: Some(false),
            maximized: Some(false),
            fullscreen: Some(false),
            focused: Some(true),
            occluded: Some(false),
            ..Default::default()
        };
        let mut raw = egui::RawInput {
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((egui::ViewportId::ROOT, info)).collect(),
            screen_rect: Some(rect),
            max_texture_side: Some(self.max_texture_side),
            time: Some(self.started.elapsed().as_secs_f64()),
            events,
            focused: true,
            system_theme: Some(self.theme),
            ..Default::default()
        };
        self.app.prepare_input(&self.ctx, &mut raw);
        let app = &mut self.app;
        let out = self.ctx.run_ui(raw, |ui| app.frame(ui));
        let egui::FullOutput { textures_delta, shapes, pixels_per_point, mut viewport_output, .. } = out;
        self.deltas.append(textures_delta);
        let root = viewport_output.remove(&egui::ViewportId::ROOT);
        let repaint_delay = root.as_ref().map_or(Duration::MAX, |v| v.repaint_delay);
        let mut screenshots = Vec::new();
        for command in root.into_iter().flat_map(|v| v.commands) {
            match command {
                egui::ViewportCommand::Screenshot(callback) => screenshots.push(callback),
                // The frame is not a window: close, minimise, title, focus, cursor warp, size.
                other => log::debug!("viewport command ignored: {other:?}"),
            }
        }
        Pass { shapes, pixels_per_point, repaint_delay, screenshots, ms: t0.elapsed().as_secs_f32() * 1000.0 }
    }

    /// Tessellate and paint a pass into the offscreen texture; the paint's milliseconds.
    pub fn paint(&mut self, gpu: &Gpu, shapes: Vec<egui::epaint::ClippedShape>, pixels_per_point: f32) -> (Painted, f32) {
        let t0 = Instant::now();
        let prims = self.ctx.tessellate(shapes, pixels_per_point);
        // The theme's panel colour, opaque.
        let mut clear = self.ctx.global_style().visuals.panel_fill.to_normalized_gamma_f32();
        clear[3] = 1.0;
        let painted = self.painter.paint(&gpu.rs, self.size.px(), pixels_per_point, clear, &prims, &mut self.deltas);
        (painted, t0.elapsed().as_secs_f32() * 1000.0)
    }

    /// The texture last painted, as tight opaque RGBA rows (snapshots, screenshots).
    pub fn read_frame(&mut self, gpu: &Gpu, out: &mut Vec<u8>) -> Result<(), String> {
        let texture = self.painter.texture().ok_or("nothing painted yet")?;
        self.shots.read(&gpu.rs, texture, self.size.px(), out)?;
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Ok(())
    }

    /// Answer screenshot requests with the frame just painted. A read that fails drops them.
    pub fn answer_screenshots(&mut self, gpu: &Gpu, callbacks: Vec<egui::ScreenshotCallback>) {
        if callbacks.is_empty() {
            return;
        }
        let mut rgba = Vec::new();
        let read = self.read_frame(gpu, &mut rgba);
        match read {
            Ok(()) => {
                let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied([self.size.px_w as usize, self.size.px_h as usize], &rgba));
                for callback in callbacks {
                    callback.complete(image.clone());
                }
            }
            Err(e) => log::warn!("screenshot: {e}"),
        }
    }
}
