//! The command line (PROTOCOL.md, "Starting (leg A)"; the host's copy is its contract's flag list).
//! `--stream` is the product's mode, `--snapshot` the host-less one the M1 smoke runs.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::adapters::gpu::Policy;

pub const USAGE: &str = "usage:
  pcbcraft-render-server --stream 127.0.0.1:PORT [--session ID] [--size WxH] [--dpr N] [--theme dark|light] [--lang zh|en|he|system] [--project FILE] [--config-dir DIR] [--adapter auto|hardware|software]
  pcbcraft-render-server --snapshot DIR [--frames N] [--size WxH] [--adapter auto|hardware|software] [--project FILE] [--dpr N] [--theme dark|light] [--lang zh|en|he|system] [--warmup N] [--gap-ms N] [--config-dir DIR]
environment: PCBZ_RENDER_TOKEN (64 lowercase hex; stream mode), PCBZ_RENDER_LOG=error|warn|info|debug
--config-dir DIR (absolute): recovery snapshots go to DIR/recovery, never the per-user folder
--lang defaults to zh (Simplified Chinese); the host's attach.lang overrides it at runtime";

#[derive(Debug, PartialEq)]
pub enum Mode {
    /// Leg A to the host's listener (loopback only).
    Stream(SocketAddr),
    /// `frame-0001.png` ... and `frames.json` go to `dir`.
    Snapshot { dir: PathBuf, frames: u32, warmup: u32, gap: Duration },
}

#[derive(Debug, PartialEq)]
pub struct Options {
    pub mode: Mode,
    /// For log lines only.
    pub session: Option<String>,
    /// The editor pane, CSS px.
    pub css: (u32, u32),
    pub dpr: f32,
    pub dark: bool,
    /// Interface language (`zh` default; `en`, `he`, `system`). A host's `attach.lang` overrides it.
    pub lang: String,
    pub adapter: Policy,
    /// A `.pc` archive, a project folder or its `project.json`, opened before the first frame.
    pub project: Option<PathBuf>,
    /// The host's folder for the per-user files the editor would otherwise keep in the user's own.
    pub config_dir: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Box<Options>),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    let (mut stream, mut snapshot) = (None, None);
    let (mut frames, mut warmup, mut gap) = (1u32, 10u32, Duration::from_millis(30));
    let mut o = Options {
        mode: Mode::Stream(SocketAddr::from(([127, 0, 0, 1], 0))),
        session: None,
        css: (1280, 800),
        dpr: 1.0,
        dark: true,
        lang: "zh".to_owned(),
        adapter: Policy::Auto,
        project: None,
        config_dir: None,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            "--stream" => {
                let addr: SocketAddr = value()?.parse().map_err(|e| format!("--stream: {e}"))?;
                if !addr.ip().is_loopback() {
                    return Err(format!("--stream: {addr} is not a loopback address"));
                }
                stream = Some(addr);
            }
            "--snapshot" => snapshot = Some(PathBuf::from(value()?)),
            "--session" => o.session = Some(value()?),
            "--frames" => frames = value()?.parse::<u32>().ok().filter(|n| *n > 0).ok_or("--frames: a positive count")?,
            "--size" => o.css = size(&value()?)?,
            "--dpr" => o.dpr = value()?.parse::<f32>().ok().filter(|d| d.is_finite() && *d > 0.0).ok_or("--dpr: a positive number")?,
            "--theme" => {
                o.dark = match value()?.as_str() {
                    "dark" => true,
                    "light" => false,
                    other => return Err(format!("--theme: dark or light, not {other}")),
                }
            }
            "--adapter" => o.adapter = Policy::parse(&value()?).ok_or("--adapter: auto, hardware or software")?,
            "--lang" => {
                let lang = value()?;
                let tag = lang.trim().replace('_', "-").to_ascii_lowercase();
                match tag.split('-').next().unwrap_or_default() {
                    "zh" | "en" | "he" | "iw" | "system" => o.lang = tag,
                    other => return Err(format!("--lang: zh, en, he or system, not {other}")),
                }
            }
            "--project" => o.project = Some(PathBuf::from(value()?)),
            "--config-dir" => {
                let dir = PathBuf::from(value()?);
                if !dir.is_absolute() {
                    return Err(format!("--config-dir: {} is not an absolute path", dir.display()));
                }
                o.config_dir = Some(dir);
            }
            "--warmup" => warmup = value()?.parse().map_err(|_| "--warmup: a number of passes")?,
            "--gap-ms" => gap = Duration::from_millis(value()?.parse().map_err(|_| "--gap-ms: milliseconds")?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    o.mode = match (stream, snapshot) {
        (Some(_), Some(_)) => return Err("--stream and --snapshot are two modes: give one".into()),
        (Some(addr), None) => Mode::Stream(addr),
        (None, Some(dir)) => Mode::Snapshot { dir, frames, warmup, gap },
        (None, None) => return Err("--stream ADDR or --snapshot DIR is required".into()),
    };
    Ok(Parsed::Run(Box::new(o)))
}

/// `WxH` in CSS px.
fn size(s: &str) -> Result<(u32, u32), String> {
    let bad = || format!("--size: WxH, not {s}");
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
    let (w, h) = (w.trim().parse::<u32>().map_err(|_| bad())?, h.trim().parse::<u32>().map_err(|_| bad())?);
    if w == 0 || h == 0 {
        return Err(bad());
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &str) -> Result<Options, String> {
        match parse(args.split_whitespace().map(String::from))? {
            Parsed::Run(o) => Ok(*o),
            other => Err(format!("{other:?}")),
        }
    }

    #[test]
    fn the_smoke_command_line() {
        let o = run("--snapshot out --frames 3 --size 640x400 --adapter software --project a.pc").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(o.mode, Mode::Snapshot { frames: 3, .. }));
        assert_eq!((o.css, o.adapter, o.project.as_deref()), ((640, 400), Policy::Software, Some(std::path::Path::new("a.pc"))));
    }

    #[test]
    fn the_stream_command_line() {
        let o = run("--stream 127.0.0.1:4001 --session s1 --size 800x500 --dpr 2 --theme light --config-dir /tmp/c").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(o.mode, Mode::Stream(SocketAddr::from(([127, 0, 0, 1], 4001))));
        assert_eq!((o.css, o.dpr, o.dark, o.session.as_deref()), ((800, 500), 2.0, false, Some("s1")));
    }

    #[test]
    fn refusals() {
        assert!(run("--frames 3").is_err_and(|e| e.contains("required")));
        assert!(run("--snapshot out --size 0x10").is_err());
        assert!(run("--snapshot out --adapter cpu").is_err());
        assert!(run("--snapshot out --frames 0").is_err());
        assert!(run("--stream 10.0.0.2:4000").is_err_and(|e| e.contains("loopback")));
        assert!(run("--stream 127.0.0.1:1 --snapshot out").is_err());
        assert!(run("--stream 127.0.0.1:1 --config-dir rel").is_err_and(|e| e.contains("absolute")));
    }
}
