//! Stream mode: the frame loop with a host on leg A (PROTOCOL.md, "Starting (leg A)" and
//! "Sequence"). Render on demand: the loop sleeps on its channel until input, a host message, the
//! UI's own repaint request (upstream's live control endpoint wakes it too) or the idle refinement
//! is due; passes keep running while the pane is hidden (control requests, timers, background
//! jobs), frames do not. Ported from SolidZ's render server `app/stream.rs`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::asks::Asks;
use super::editor_host::EditorHost;
use super::events::{self, LoopEvent};
use super::frames::FrameStream;
use super::output::OutputRelay;
use crate::adapters::gpu::{self, Gpu, Policy};
use crate::adapters::link::{self, Incoming, Outbox, Writer};
use crate::adapters::painter::Painted;
use crate::domain::geometry::FrameSize;
use crate::domain::input::{self, InputEvent, Translator};
use crate::domain::pacing::Schedule;
use crate::domain::wire::{ByeWhy, HostMsg, PROTOCOL_VERSION, ServerMsg};

/// Exit codes the host may read (it acts on `bye`; these are for logs).
pub const EXIT_OK: i32 = 0;
pub const EXIT_SETUP: i32 = 1;
pub const EXIT_PROTOCOL: i32 = 2;
pub const EXIT_DEVICE_LOST: i32 = 3;
pub const EXIT_PANIC: i32 = 4;
pub const EXIT_OPEN: i32 = 5;

/// Longest the startup project open may take before `ready` (the host waits 60 s after `hello`).
const OPEN_WAIT: Duration = Duration::from_secs(50);
/// Pause between the passes that wait for the open.
const OPEN_POLL: Duration = Duration::from_millis(10);

pub struct StreamArgs {
    pub addr: SocketAddr,
    pub dark: bool,
    pub size: FrameSize,
    pub adapter: Policy,
    pub project: Option<PathBuf>,
    /// Recovery snapshots go to `<config_dir>/recovery`.
    pub config_dir: Option<PathBuf>,
}

/// How the loop ends: the exit code and the last message to send.
struct End {
    code: i32,
    last: Option<ServerMsg>,
}

impl End {
    fn bye(code: i32, why: ByeWhy, detail: Option<String>) -> Self {
        End { code, last: Some(ServerMsg::Bye { why, detail }) }
    }
}

/// The host's token for this process (`PCBZ_RENDER_TOKEN`, 64 lowercase hex).
fn stream_token() -> Option<String> {
    let t = std::env::var("PCBZ_RENDER_TOKEN").ok()?;
    (t.len() == 64 && t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(t)
}

pub fn run(args: StreamArgs) -> i32 {
    let Some(token) = stream_token() else {
        eprintln!("render-server: PCBZ_RENDER_TOKEN must be 64 lowercase hex characters");
        return EXIT_SETUP;
    };
    let (tx, rx) = events::channel();
    let host_tx = tx.clone();
    let (outbox, writer) = match link::connect(args.addr, move |i| {
        let _ = host_tx.send(LoopEvent::Host(i));
    }) {
        Ok(link) => link,
        Err(e) => {
            eprintln!("render-server: cannot reach the host at {}: {e}", args.addr);
            return EXIT_SETUP;
        }
    };
    let gpu = match gpu::open(args.adapter) {
        Ok(gpu) => gpu,
        Err(e) => {
            // No message at all: the host takes only `hello` first, and an exit before `ready` is
            // what puts its session on the viewer at once (a `bye` would count as a crash and be
            // retried). The reason goes to stderr, which the host keeps in its log.
            log::error!("{e}");
            return finish(&outbox, writer, End { code: EXIT_SETUP, last: None });
        }
    };
    outbox.send(&ServerMsg::Hello {
        v: PROTOCOL_VERSION,
        token,
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        backend: Some(gpu.backend),
        adapter: gpu.adapter.clone(),
        software: gpu.software,
    });
    let ctx = egui::Context::default();
    events::wake_on_repaint(&ctx, tx);
    let asks = Asks::new(outbox.clone());
    if !rfd::install(std::sync::Arc::new(asks.clone())) {
        log::warn!("file dialogs: a host was installed already");
    }
    let recovery = args.config_dir.as_ref().map(|d| d.join("recovery"));
    let mut ui = EditorHost::new(ctx, &gpu, args.dark, args.size, args.project.clone(), recovery);
    // Before `ready`, as a device that does not open: the reason on stderr, no `bye`.
    let (control_port, control_secret) = match ui.start_control() {
        Ok(endpoint) => endpoint,
        Err(e) => {
            log::error!("control channel: {e}");
            return finish(&outbox, writer, End { code: EXIT_SETUP, last: None });
        }
    };
    let mut stream = Stream {
        gpu,
        ui,
        outbox: outbox.clone(),
        asks: asks.clone(),
        frames: FrameStream::default(),
        relay: OutputRelay::default(),
        schedule: Schedule::default(),
        translator: Translator::native(),
        pending: Vec::new(),
        input_seq: 0,
        painted: None,
        visible: true,
        control: (control_port, control_secret),
        ready: false,
        read_only: None,
        revision: None,
        project: args.project,
        stopping: false,
    };
    let end = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stream.run(&rx))) {
        Ok(end) => {
            drop(stream);
            end
        }
        // A panic in the loop (the default hook has printed it on stderr): the host hears `bye
        // error` with it, a crash. Not dropped: a UI that panicked mid-pass may panic again in
        // its destructors, before the host was told.
        Err(panic) => {
            std::mem::forget(stream);
            End::bye(EXIT_PANIC, ByeWhy::Error, Some(panic_text(panic.as_ref())))
        }
    };
    asks.close();
    finish(&outbox, writer, end)
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    let text = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned());
    format!("panic: {}", text.unwrap_or_default())
}

/// Send the last message, let the writer drain, and say the code. (The control endpoint belongs
/// to the UI and stopped with it.)
fn finish(outbox: &Outbox, writer: Writer, end: End) -> i32 {
    if let Some(last) = &end.last {
        outbox.send(last);
    }
    outbox.close();
    writer.join();
    end.code
}

/// The pass the texture was last painted by: its size (the texture's until the next paint,
/// even after a resize), input sequence and timings, for a frame held for lack of a credit.
#[derive(Clone, Copy)]
struct PaintedPass {
    size: FrameSize,
    input_seq: u64,
    pass_ms: f32,
    paint_ms: f32,
}

struct Stream {
    gpu: Gpu,
    ui: EditorHost,
    outbox: Outbox,
    asks: Asks,
    frames: FrameStream,
    relay: OutputRelay,
    schedule: Schedule,
    translator: Translator,
    /// Input for the next pass, in order.
    pending: Vec<egui::Event>,
    /// The last `input.seq` read (echoed as `frame.inputSeq` by the pass that reads it).
    input_seq: u64,
    painted: Option<PaintedPass>,
    visible: bool,
    /// The control channel's port and secret, for `ready`.
    control: (u16, String),
    /// `ready` has been sent.
    ready: bool,
    /// The host's read-only reason, said again once the startup open is over (opening a project
    /// replaces the status line the reason was written on).
    read_only: Option<String>,
    /// The project revision last announced with `design_changed`.
    revision: Option<u64>,
    /// The project the process was started on, if any.
    project: Option<PathBuf>,
    stopping: bool,
}

impl Stream {
    fn run(&mut self, rx: &Receiver<LoopEvent>) -> End {
        // What the host sent while the device was being made (`policy`, `resize`, `visible`),
        // and the startup project opening: passes without paint until it is in (or has failed).
        let until = Instant::now() + OPEN_WAIT;
        loop {
            while let Ok(event) = rx.try_recv() {
                if let Some(end) = self.handle(event) {
                    return end;
                }
            }
            if self.stopping {
                return End::bye(EXIT_OK, ByeWhy::Stop, None);
            }
            if let Some(end) = self.pass(Mode::Unpainted) {
                return end;
            }
            if !self.ui.app.is_busy() {
                break;
            }
            if Instant::now() > until {
                return End::bye(EXIT_OPEN, ByeWhy::Error, Some(format!("the project did not finish opening in {} s", OPEN_WAIT.as_secs())));
            }
            std::thread::sleep(OPEN_POLL);
        }
        if self.project.is_some() && self.ui.app.open_revision().is_none() {
            let why = self.ui.app.status_text().unwrap_or("no reason given").to_string();
            return End::bye(EXIT_OPEN, ByeWhy::Error, Some(format!("the project did not open: {why}")));
        }
        if let Some(reason) = self.read_only.clone() {
            self.ui.app.set_read_only(Some(&reason));
        }
        if let Some(end) = self.pass(Mode::First) {
            return end;
        }
        loop {
            // Checked before waiting too: a `stop` that came in before the first pass must not
            // wait for some later event to be seen.
            if self.stopping {
                return End::bye(EXIT_OK, ByeWhy::Stop, None);
            }
            if let Some(end) = self.wait(rx) {
                return end;
            }
            if self.stopping {
                return End::bye(EXIT_OK, ByeWhy::Stop, None);
            }
            if self.visible && self.frames.held_and_sendable() {
                self.send_held();
            }
            let now = Instant::now();
            if self.schedule.due(now)
                && let Some(end) = self.pass(Mode::Steady)
            {
                return end;
            }
            if self.visible && self.frames.refine_at().is_some_and(|at| at <= now) {
                match self.frames.refine(&self.ui.size(), self.input_seq) {
                    Ok(Some(out)) => self.outbox.send_with(&out.msg, &out.payload),
                    Ok(None) => {}
                    Err(e) => log::warn!("refinement: {e}"),
                }
            }
        }
    }

    /// Sleep until the next event or deadline (a pass, the refinement), then take every event
    /// that is there.
    fn wait(&mut self, rx: &Receiver<LoopEvent>) -> Option<End> {
        let refine = if self.visible { self.frames.refine_at() } else { None };
        let deadline = [self.schedule.next_pass(), refine].into_iter().flatten().min();
        let first = match deadline {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Some(End { code: EXIT_OK, last: None }),
            },
            None => match rx.recv() {
                Ok(event) => Some(event),
                Err(_) => return Some(End { code: EXIT_OK, last: None }),
            },
        };
        for event in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            if let Some(end) = self.handle(event) {
                return Some(end);
            }
        }
        None
    }

    fn handle(&mut self, event: LoopEvent) -> Option<End> {
        match event {
            LoopEvent::Repaint(delay) => self.schedule.repaint_after(Instant::now(), delay),
            LoopEvent::Host(Incoming::Msg(msg)) => return self.on_host(msg),
            LoopEvent::Host(Incoming::Refused(refusal)) => {
                log::error!("host broke the protocol: {}", refusal.message);
                return Some(End { code: EXIT_PROTOCOL, last: Some(refusal.into_msg()) });
            }
            LoopEvent::Host(Incoming::Closed(why)) => return Some(End::bye(EXIT_OK, ByeWhy::HostGone, Some(why))),
        }
        None
    }

    fn on_host(&mut self, msg: HostMsg) -> Option<End> {
        match msg {
            HostMsg::Input { seq, events } => {
                let (events, dropped) = input::parse_events(&events);
                if dropped > 0 {
                    log::debug!("input {seq}: {dropped} events this version does not read");
                }
                let points_per_css = 1.0 / self.ui.zoom();
                for ev in events {
                    if let InputEvent::Focus { focused } = ev {
                        self.ui.set_focused(focused);
                    }
                    if let Some(e) = self.translator.event(ev, points_per_css) {
                        self.pending.push(e);
                    }
                }
                self.input_seq = seq;
                self.frames.activity(Instant::now());
                self.schedule.want();
            }
            HostMsg::Resize { w, h, dpr } => {
                self.ui.set_size(FrameSize::new(w, h, dpr));
                self.frames.invalidate();
                self.schedule.want();
            }
            HostMsg::Visible { visible } => {
                self.visible = visible;
                if visible {
                    self.frames.invalidate();
                    self.schedule.want();
                }
            }
            HostMsg::Ack { seq, decode_ms } => {
                log::debug!("frame {seq} on screen, decoded in {decode_ms} ms");
                self.frames.ack();
            }
            HostMsg::Policy { read_only } => {
                let reason = read_only.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
                log::info!("policy: read-only {reason:?}");
                self.ui.app.set_read_only(reason.as_deref());
                self.read_only = reason;
                self.schedule.want();
            }
            HostMsg::Answer { ask, path } => {
                self.asks.answer(ask, path);
                self.schedule.want();
            }
            HostMsg::Stop { why } => {
                log::info!("stop: {why}");
                self.stopping = true;
            }
            HostMsg::Error { code, message } => {
                log::error!("host error {code}: {message}");
                return Some(End { code: EXIT_PROTOCOL, last: None });
            }
        }
        None
    }

    /// One UI pass and its outputs; per `mode`, its paint and frame. `First`: `ready` goes out
    /// once the first frame is drawn, before it.
    fn pass(&mut self, mode: Mode) -> Option<End> {
        let input_seq = self.input_seq;
        let pass = self.ui.pass(std::mem::take(&mut self.pending));
        self.schedule.ran(Instant::now(), pass.repaint_delay);
        for msg in self.relay.relay(pass.platform, self.ui.zoom()) {
            self.outbox.send(&msg);
        }
        if pass.paste_requested {
            self.outbox.send(&ServerMsg::PasteRequest);
        }
        self.announce_revision();
        let first = mode == Mode::First;
        if mode == Mode::Unpainted || !(self.visible || first || !pass.screenshots.is_empty()) {
            return None;
        }
        let (painted, paint_ms) = self.ui.paint(&self.gpu, pass.shapes, pass.pixels_per_point);
        if let Painted::Lost(why) = painted {
            return Some(End::bye(EXIT_DEVICE_LOST, ByeWhy::DeviceLost, Some(why)));
        }
        self.ui.answer_screenshots(&self.gpu, pass.screenshots);
        if first {
            let size = self.ui.size();
            self.outbox.send(&ServerMsg::Ready {
                control_port: self.control.0,
                control_secret: self.control.1.clone(),
                w: size.px_w,
                h: size.px_h,
                dpr: size.dpr,
                samples: 1,
            });
            self.ready = true;
            // The project opened before `ready`: its revision is the first thing the host learns.
            self.announce_revision();
        }
        self.painted = Some(PaintedPass { size: self.ui.size(), input_seq, pass_ms: pass.ms, paint_ms });
        if self.visible {
            self.send_held();
        }
        None
    }

    /// `design_changed` when the open project's revision moved since it was last said (an edit,
    /// undo or redo, from the pane or the control channel). Not before `ready`: the host takes
    /// only `hello` first.
    fn announce_revision(&mut self) {
        if !self.ready {
            return;
        }
        if let Some(revision) = self.ui.app.open_revision()
            && self.revision != Some(revision)
        {
            self.revision = Some(revision);
            self.outbox.send(&ServerMsg::DesignChanged { revision });
        }
    }

    /// The frame for the texture last painted, if it changed and a credit is left.
    fn send_held(&mut self) {
        let (Some(p), Some(texture)) = (self.painted, self.ui.texture()) else { return };
        match self.frames.send_painted(&self.gpu.rs, texture, &p.size, p.input_seq, p.pass_ms, p.paint_ms) {
            Ok(Some(out)) => self.outbox.send_with(&out.msg, &out.payload),
            Ok(None) => {}
            Err(e) => log::warn!("frame: {e}"),
        }
    }
}

/// What a pass does after the UI ran.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The startup open is still running: no paint, no `ready` yet.
    Unpainted,
    /// The first pass after the open: always painted, `ready` goes out.
    First,
    /// Any other: painted when the pane is visible.
    Steady,
}
