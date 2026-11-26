//! The editor with no window: upstream's `PcbApp` assembled by `PcbApp::new_embedded` (patch 0002),
//! driven one pass at a time by synthesized `RawInput` (`ctx.run_ui(raw, |ui| app.frame(ui))`, the
//! way upstream's own `performance/project.rs` drives it) and painted offscreen. Ported from SolidZ's
//! `app/ui_host.rs`: same `RawInput` assembly, viewport commands, texture deltas, tessellate and
//! screenshot answering; the CAD-specific assembly (kernel session, 3D viewport, control channel)
//! is replaced by the one constructor. The control channel is upstream's own `LiveControl`
//! (`PcbApp::start_control`, patch 0006): its requests run inside the frame, on the same session.

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
    /// Cursor, IME, copied text: what the UI asks of its platform.
    pub platform: egui::PlatformOutput,
    /// When the UI wants the next pass (`Duration::MAX`: not until something happens); the stream
    /// loop paces by it.
    pub repaint_delay: Duration,
    /// A Paste button was pressed: the host is asked for the clipboard's text.
    pub paste_requested: bool,
    /// Screenshot requests: the frame painted after this pass, read back.
    pub screenshots: Vec<egui::ScreenshotCallback>,
    pub ms: f32,
}

/// The host's `lang` as the chrome's language preference. Anything unrecognised (and the
/// default when the host says nothing) is Simplified Chinese; `en` and `he` stay available.
fn chrome_language(lang: &str) -> pcbcraft_app::LanguageChoice {
    use pcbcraft_app::LanguageChoice;
    let tag = lang.trim().replace('_', "-").to_ascii_lowercase();
    match tag.split('-').next().unwrap_or_default() {
        "en" => LanguageChoice::English,
        "he" | "iw" => LanguageChoice::Hebrew,
        "system" => LanguageChoice::System,
        _ => LanguageChoice::Chinese,
    }
}

pub struct EditorHost {
    pub ctx: egui::Context,
    pub app: PcbApp,
    started: Instant,
    size: FrameSize,
    focused: bool,
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
    /// `ctx` is made by the caller so its repaint callback is set before anything can ask for one.
    /// `recovery`: where recovery snapshots go if the person turns them on (none: nowhere).
    pub fn new(
        ctx: egui::Context,
        gpu: &Gpu,
        dark: bool,
        lang: &str,
        size: FrameSize,
        project: Option<PathBuf>,
        recovery: Option<PathBuf>,
    ) -> Self {
        // `new_embedded` also turns on embedded viewports: detached editor windows become windows
        // inside the pane, there is no OS to give them real ones.
        let mut app = PcbApp::new_embedded(&ctx, project);
        // The host's theme and language: zh unless the host asked for another language. Both take
        // effect on the first pass and can change later without touching the document.
        app.set_host_theme(dark);
        app.set_language(chrome_language(lang));
        if let Some(dir) = recovery {
            app.set_recovery_dir(dir);
        }
        EditorHost {
            max_texture_side: gpu.rs.device.limits().max_texture_dimension_2d as usize,
            ctx,
            app,
            started: Instant::now(),
            size,
            focused: true,
            theme: if dark { egui::Theme::Dark } else { egui::Theme::Light },
            deltas: egui::TexturesDelta::default(),
            painter: OffscreenPainter::default(),
            shots: Readback::default(),
        }
    }

    pub fn size(&self) -> FrameSize {
        self.size
    }

    pub fn set_size(&mut self, size: FrameSize) {
        self.size = size;
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Switch light/dark at runtime: the next pass repaints in the new palette. No document,
    /// selection or undo state is touched.
    pub fn set_theme(&mut self, dark: bool) {
        self.theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
        self.app.set_host_theme(dark);
        self.ctx.request_repaint();
    }

    /// Switch the interface language at runtime (`attach.lang`).
    pub fn set_language(&mut self, lang: &str) {
        self.app.set_language(chrome_language(lang));
        self.ctx.request_repaint();
    }

    /// CSS px per UI point: the UI's own zoom (1 unless a preference changes it).
    pub fn zoom(&self) -> f32 {
        self.ctx.zoom_factor()
    }

    /// Start upstream's live control endpoint; `(port, secret)` for `pcbcraft-cli mcp --connect`.
    /// Requests may wait five minutes for the editor, as the runner's longest call (a shorter wait
    /// would drop the reply of a command that outlived it, after its edit applied).
    pub fn start_control(&mut self) -> Result<(u16, String), String> {
        self.app.start_control(&self.ctx, Duration::from_secs(300))
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
            focused: Some(self.focused),
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
            focused: self.focused,
            system_theme: Some(self.theme),
            ..Default::default()
        };
        self.app.prepare_input(&self.ctx, &mut raw);
        let app = &mut self.app;
        let out = self.ctx.run_ui(raw, |ui| app.frame(ui));
        let egui::FullOutput { platform_output, textures_delta, shapes, pixels_per_point, mut viewport_output, .. } = out;
        self.deltas.append(textures_delta);
        let root = viewport_output.remove(&egui::ViewportId::ROOT);
        let repaint_delay = root.as_ref().map_or(Duration::MAX, |v| v.repaint_delay);
        let mut screenshots = Vec::new();
        let mut paste_requested = false;
        for command in root.into_iter().flat_map(|v| v.commands) {
            match command {
                egui::ViewportCommand::Screenshot(callback) => screenshots.push(callback),
                egui::ViewportCommand::RequestPaste => paste_requested = true,
                // The frame is not a window: close, minimise, title, focus, cursor warp, size.
                other => log::debug!("viewport command ignored: {other:?}"),
            }
        }
        Pass { shapes, pixels_per_point, platform: platform_output, repaint_delay, paste_requested, screenshots, ms: t0.elapsed().as_secs_f32() * 1000.0 }
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

    pub fn texture(&self) -> Option<&egui_wgpu::wgpu::Texture> {
        self.painter.texture()
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
