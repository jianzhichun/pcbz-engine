//! The command line. M1 has the snapshot mode only; `--stream` (leg A to the host) arrives with M2.

use std::path::PathBuf;
use std::time::Duration;

use crate::adapters::gpu::Policy;

pub const USAGE: &str = "usage:
  pcbcraft-render-server --snapshot DIR [--frames N] [--size WxH] [--adapter auto|hardware|software] [--project FILE] [--dpr N] [--theme dark|light] [--warmup N] [--gap-ms N]
environment: PCBZ_RENDER_LOG=error|warn|info|debug";

#[derive(Debug, PartialEq)]
pub struct Options {
    /// `frame-0001.png` ... and `frames.json` go here.
    pub dir: PathBuf,
    pub frames: u32,
    /// The editor pane, CSS px.
    pub css: (u32, u32),
    pub dpr: f32,
    pub dark: bool,
    pub adapter: Policy,
    /// A `.pc` archive, a project folder or its `project.json`, opened before the first frame.
    pub project: Option<PathBuf>,
    /// Passes run after the project opened and before frame 1 (layout, fit-to-view, fonts).
    pub warmup: u32,
    /// Pause between passes, so animations and timers see time pass.
    pub gap: Duration,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Box<Options>),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    let mut dir = None;
    let mut o = Options { dir: PathBuf::new(), frames: 1, css: (1280, 800), dpr: 1.0, dark: true, adapter: Policy::Auto, project: None, warmup: 10, gap: Duration::from_millis(30) };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            "--snapshot" => dir = Some(PathBuf::from(value()?)),
            "--frames" => o.frames = value()?.parse::<u32>().ok().filter(|n| *n > 0).ok_or("--frames: a positive count")?,
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
            "--project" => o.project = Some(PathBuf::from(value()?)),
            "--warmup" => o.warmup = value()?.parse().map_err(|_| "--warmup: a number of passes")?,
            "--gap-ms" => o.gap = Duration::from_millis(value()?.parse().map_err(|_| "--gap-ms: milliseconds")?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    o.dir = dir.ok_or("--snapshot DIR is required")?;
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
        assert_eq!((o.frames, o.css, o.adapter, o.project.as_deref()), (3, (640, 400), Policy::Software, Some(std::path::Path::new("a.pc"))));
    }

    #[test]
    fn refusals() {
        assert!(run("--frames 3").is_err_and(|e| e.contains("required")));
        assert!(run("--snapshot out --size 0x10").is_err());
        assert!(run("--snapshot out --adapter cpu").is_err());
        assert!(run("--snapshot out --frames 0").is_err());
    }
}
