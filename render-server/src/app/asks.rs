//! The editor's file dialogs, answered by the host (PROTOCOL.md `ask_open`, `ask_save`, `ask_done`,
//! `answer`). The editor opens every dialog on a worker thread through the `rfd` shim
//! (`shims/rfd`, which installs this as its [`Host`]); the worker blocks here until the host's
//! `answer` comes, so the frame loop never waits. Folder picks, and saves that name no file type
//! (a new project or library *folder*), are cancelled with a `notice`: the host's pane can offer
//! files only. With no host (snapshot mode) every dialog is cancelled.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use rfd::{Answer, Host, Request};

use crate::adapters::link::Outbox;
use crate::domain::wire::{NoticeLevel, ServerMsg};

/// Shown when a dialog asks for something the pane cannot offer.
pub const NO_FOLDERS: &str = "这里只能选择文件，不能选择或新建文件夹";

#[derive(Default)]
struct Table {
    next: u32,
    /// Asks the host has not answered: `None` waiting, `Some(path)` answered (`None` path: cancel).
    asks: HashMap<u32, Option<Option<PathBuf>>>,
    /// The process is ending: every waiting worker is released with a cancel.
    closed: bool,
}

/// The asks of this process. Cheap to clone (the shim holds one, the loop another).
#[derive(Clone)]
pub struct Asks {
    shared: Arc<Shared>,
}

struct Shared {
    outbox: Outbox,
    table: Mutex<Table>,
    answered: Condvar,
}

impl Asks {
    pub fn new(outbox: Outbox) -> Self {
        Asks { shared: Arc::new(Shared { outbox, table: Mutex::new(Table::default()), answered: Condvar::new() }) }
    }

    /// The host answered `ask` (`None`: cancelled). An answer to an ask that is not waiting is ignored.
    pub fn answer(&self, ask: u32, path: Option<String>) {
        let mut table = self.lock();
        if let Some(slot) = table.asks.get_mut(&ask).filter(|s| s.is_none()) {
            *slot = Some(path.map(PathBuf::from));
            self.shared.answered.notify_all();
        }
    }

    /// End of the process: release every worker waiting for the host.
    pub fn close(&self) {
        self.lock().closed = true;
        self.shared.answered.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.shared.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notice(&self, text: &str) {
        self.shared.outbox.send(&ServerMsg::Notice { level: NoticeLevel::Warn, text: text.to_string() });
    }

    /// Send the ask `msg` makes and wait for its answer.
    fn ask(&self, msg: impl FnOnce(u32) -> ServerMsg) -> Answer {
        let ask = {
            let mut table = self.lock();
            if table.closed {
                return Answer::default();
            }
            table.next += 1;
            let ask = table.next;
            table.asks.insert(ask, None);
            ask
        };
        self.shared.outbox.send(&msg(ask));
        let mut table = self.lock();
        loop {
            if let Some(Some(path)) = table.asks.get(&ask) {
                let path = path.clone();
                table.asks.remove(&ask);
                return Answer { path, ask };
            }
            if table.closed {
                table.asks.remove(&ask);
                return Answer::default();
            }
            table = self.shared.answered.wait(table).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Host for Asks {
    fn pick_file(&self, request: &Request) -> Answer {
        let exts = request.extensions();
        self.ask(|ask| ServerMsg::AskOpen { ask, exts })
    }

    fn pick_folder(&self, _request: &Request) -> Answer {
        self.notice(NO_FOLDERS);
        Answer::default()
    }

    fn save_file(&self, request: &Request) -> Answer {
        let exts = request.extensions();
        // A save with no file type names a new folder (a project folder, a library): not offered.
        if exts.is_empty() {
            self.notice(NO_FOLDERS);
            return Answer::default();
        }
        let name = request.file_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| "project".to_string());
        self.ask(|ask| ServerMsg::AskSave { ask, name, exts })
    }

    fn done(&self, ask: u32, ok: bool) {
        self.shared.outbox.send(&ServerMsg::AskDone { ask, ok });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::mpsc::Receiver;

    /// The next message the outbox was given, as JSON.
    fn next(rx: &Receiver<Option<Vec<u8>>>) -> Value {
        let bytes = rx.recv_timeout(std::time::Duration::from_secs(5)).ok().flatten().unwrap_or_default();
        // u32 envelope length, u32 header length, header.
        let header_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        serde_json::from_slice(&bytes[8..8 + header_len]).unwrap_or(Value::Null)
    }

    fn request(file_name: &str, filters: &[(&str, &[&str])]) -> Request {
        Request {
            file_name: Some(file_name.into()),
            filters: filters.iter().map(|(n, e)| rfd::Filter { name: (*n).into(), extensions: e.iter().map(|x| x.to_string()).collect() }).collect(),
            ..Request::default()
        }
    }

    /// A save asks the host, blocks its worker until the answer, and the worker's end says `ask_done`.
    #[test]
    fn a_save_waits_for_the_hosts_answer() {
        let (outbox, rx) = Outbox::detached();
        let asks = Asks::new(outbox);
        let worker = asks.clone();
        let handle = std::thread::spawn(move || worker.save_file(&request("a.pc", &[("PCBZ", &["pc"])])));
        let ask = next(&rx);
        assert_eq!((ask["k"].as_str(), ask["name"].as_str(), ask["exts"][0].as_str()), (Some("ask_save"), Some("a.pc"), Some("pc")));
        asks.answer(ask["ask"].as_u64().unwrap_or(0) as u32, Some("/staging/a.pc".into()));
        let answer = handle.join().unwrap_or_default();
        assert_eq!((answer.path, answer.ask), (Some(PathBuf::from("/staging/a.pc")), 1));
        asks.done(1, true);
        assert_eq!(next(&rx), serde_json::json!({"k": "ask_done", "ask": 1, "ok": true}));
    }

    /// Folders and saves of no file type are cancelled at once with a notice; closing releases a waiting worker.
    #[test]
    fn folders_are_refused_and_close_releases() {
        let (outbox, rx) = Outbox::detached();
        let asks = Asks::new(outbox);
        assert_eq!(asks.pick_folder(&Request::default()).path, None);
        assert_eq!(next(&rx)["k"], "notice");
        assert_eq!(asks.save_file(&request("lib", &[])).path, None);
        assert_eq!(next(&rx)["text"], NO_FOLDERS);
        let worker = asks.clone();
        let handle = std::thread::spawn(move || worker.pick_file(&request("", &[("x", &["pc"])])));
        assert_eq!(next(&rx)["k"], "ask_open");
        asks.close();
        assert_eq!(handle.join().unwrap_or_default().path, None);
    }
}
