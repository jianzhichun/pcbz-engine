//! PCBZ render server patch (U8): the `rfd` file dialog, answered by the host.
//!
//! The editor opens a native dialog at 13 places, each on a worker thread of its own
//! (`rfd::FileDialog::new()...pick_file()` and the like). Embedded in a host there is no window to
//! parent a dialog and nobody at the machine's screen to click it, so this crate takes `rfd`'s
//! place (a `[patch.crates-io]` entry, applied by the build): the same builder methods, and the
//! three picks go to whatever [`Host`] the render server installed. With none installed every
//! dialog is cancelled, which is what the editor does when the person closes a dialog.
//!
//! The pick blocks the calling worker thread until the host answers, so the editor's frame loop is
//! never held (every call site is on a worker thread).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// One `add_filter`: a label and the extensions (no dot) it covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// What a dialog asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub title: Option<String>,
    pub directory: Option<PathBuf>,
    pub file_name: Option<String>,
    pub filters: Vec<Filter>,
}

impl Request {
    /// The extensions of every filter, without dots, in order, each once.
    pub fn extensions(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for ext in self.filters.iter().flat_map(|f| &f.extensions) {
            let ext = ext.trim_start_matches('.').to_string();
            if !ext.is_empty() && !out.contains(&ext) {
                out.push(ext);
            }
        }
        out
    }
}

/// The host's answer to one dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// `None`: cancelled.
    pub path: Option<PathBuf>,
    /// The host's number for this ask, 0 when it made none. When non-zero, [`Host::done`] is called
    /// with it once the thread that asked has finished (the editor's worker is done with the
    /// file), so the host knows a save is complete.
    pub ask: u32,
}

/// Who answers the dialogs. Called on the editor's worker threads, and may block them.
pub trait Host: Send + Sync {
    fn pick_file(&self, request: &Request) -> Answer;
    fn pick_folder(&self, request: &Request) -> Answer;
    fn save_file(&self, request: &Request) -> Answer;
    /// The thread that got `ask`'s answer has finished; `ok`: the answer named a path.
    fn done(&self, ask: u32, ok: bool);
}

static HOST: OnceLock<Arc<dyn Host>> = OnceLock::new();

/// Install the host, once per process. `false`: one is installed already.
pub fn install(host: Arc<dyn Host>) -> bool {
    HOST.set(host).is_ok()
}

/// Calls to [`Host::done`] owed by this thread, made when it ends: the editor's save worker has
/// written the file by then and sent its result.
struct Owed(Vec<(Arc<dyn Host>, u32, bool)>);

impl Drop for Owed {
    fn drop(&mut self) {
        for (host, ask, ok) in self.0.drain(..) {
            host.done(ask, ok);
        }
    }
}

thread_local! {
    static OWED: RefCell<Owed> = const { RefCell::new(Owed(Vec::new())) };
}

fn ask(request: Request, pick: impl FnOnce(&dyn Host, &Request) -> Answer) -> Option<PathBuf> {
    let host = HOST.get()?;
    let answer = pick(host.as_ref(), &request);
    if answer.ask != 0 {
        let ok = answer.path.is_some();
        // A thread that is already being torn down cannot be told later: tell the host now.
        if OWED.try_with(|owed| owed.borrow_mut().0.push((Arc::clone(host), answer.ask, ok))).is_err() {
            host.done(answer.ask, ok);
        }
    }
    answer.path
}

/// The builder `rfd` has, with the methods the editor calls.
#[derive(Clone, Debug, Default)]
pub struct FileDialog {
    request: Request,
}

impl FileDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        self.request.filters.push(Filter { name: name.into(), extensions: extensions.iter().map(ToString::to_string).collect() });
        self
    }

    pub fn set_directory<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.request.directory = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.request.file_name = Some(file_name.into());
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.request.title = Some(title.into());
        self
    }

    pub fn pick_file(self) -> Option<PathBuf> {
        ask(self.request, |h, r| h.pick_file(r))
    }

    pub fn pick_folder(self) -> Option<PathBuf> {
        ask(self.request, |h, r| h.pick_folder(r))
    }

    pub fn save_file(self) -> Option<PathBuf> {
        ask(self.request, |h, r| h.save_file(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builder_collects_what_the_editor_asks() {
        let dialog = FileDialog::new().add_filter("PCBZ project", &["pc", ".json"]).add_filter("Zip", &["zip", "pc"]).set_title("t").set_file_name(String::from("a.pc"));
        assert_eq!(dialog.request.extensions(), ["pc", "json", "zip"]);
        assert_eq!((dialog.request.title.as_deref(), dialog.request.file_name.as_deref()), (Some("t"), Some("a.pc")));
    }

    #[test]
    fn with_no_host_every_dialog_is_cancelled() {
        // (this test binary installs none)
        assert_eq!(FileDialog::new().pick_file(), None);
        assert_eq!(FileDialog::new().save_file(), None);
    }
}
