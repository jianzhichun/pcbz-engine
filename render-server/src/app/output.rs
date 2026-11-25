//! What the UI asks of its platform after a pass (`egui::PlatformOutput`), as wire messages
//! (PROTOCOL.md, "What the render server does with the platform output"): cursor and IME only when
//! they change, copied text to the host (which owns the person's clipboard: PCBZ does not open the
//! system clipboard from here), a paste request when a Paste button was pressed. Links are not
//! opened (the pane is not a browser of its own). A copy too big for one message (the host ends a
//! session whose header passes 1 MiB) goes out as a `notice` instead.

use crate::domain::envelope::MAX_HEADER_BYTES;
use crate::domain::wire::{self, ImePurpose, ImeState, NoticeLevel, ServerMsg};

/// Shown when a copy is too big to hand to the host's clipboard.
pub const TOO_BIG_TO_COPY: &str = "复制的内容太大，没有放进剪贴板";

#[derive(Default)]
pub struct OutputRelay {
    cursor: Option<egui::CursorIcon>,
    ime: Option<Option<ImeState>>,
}

impl OutputRelay {
    /// The messages for `out`. `zoom`: CSS px per UI point (the UI's zoom factor).
    pub fn relay(&mut self, out: egui::PlatformOutput, zoom: f32) -> Vec<ServerMsg> {
        let mut msgs = Vec::new();
        if self.cursor != Some(out.cursor_icon) {
            self.cursor = Some(out.cursor_icon);
            // The variant's name is the wire's (`CursorIcon` derives `Debug`).
            msgs.push(ServerMsg::Cursor { icon: format!("{:?}", out.cursor_icon) });
        }
        let ime = out.ime.map(|o| ime_state(o, zoom));
        if self.ime != Some(ime) {
            self.ime = Some(ime);
            msgs.push(ServerMsg::Ime { ime });
        }
        for command in out.commands {
            match command {
                egui::OutputCommand::CopyText(text) => {
                    let copied = ServerMsg::Copied { text };
                    if wire::header(&copied).len() <= MAX_HEADER_BYTES {
                        msgs.push(copied);
                    } else {
                        log::warn!("copied text not sent: its header passes {MAX_HEADER_BYTES} bytes");
                        msgs.push(ServerMsg::Notice { level: NoticeLevel::Warn, text: TOO_BIG_TO_COPY.to_string() });
                    }
                }
                egui::OutputCommand::CopyImage(_) => log::info!("copied image not sent: the wire carries text"),
                egui::OutputCommand::OpenUrl(url) => log::info!("link not opened: {}", url.url),
                egui::OutputCommand::TextSelectionSettled(_) => {}
            }
        }
        msgs
    }
}

/// `IMEOutput { purpose, rect, cursor_rect, should_interrupt_composition }` in CSS px.
fn ime_state(o: egui::output::IMEOutput, zoom: f32) -> ImeState {
    let css = |r: egui::Rect| [r.min.x * zoom, r.min.y * zoom, r.width() * zoom, r.height() * zoom];
    ImeState {
        purpose: match o.purpose {
            egui::IMEPurpose::Normal => ImePurpose::Normal,
            egui::IMEPurpose::Password => ImePurpose::Password,
            egui::IMEPurpose::Terminal => ImePurpose::Terminal,
        },
        rect: css(o.rect),
        cursor: css(o.cursor_rect),
        interrupt: o.should_interrupt_composition,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy whose message would pass the host's header limit is a notice; a small one is sent.
    #[test]
    fn a_copy_too_big_for_one_header_becomes_a_notice() {
        let mut out = egui::PlatformOutput::default();
        out.commands.push(egui::OutputCommand::CopyText("\"".repeat(MAX_HEADER_BYTES / 2)));
        out.commands.push(egui::OutputCommand::CopyText("small".to_string()));
        let msgs = OutputRelay::default().relay(out, 1.0);
        let kinds: Vec<&str> = msgs
            .iter()
            .map(|m| match m {
                ServerMsg::Notice { .. } => "notice",
                ServerMsg::Copied { text } if text.as_str() == "small" => "copied",
                ServerMsg::Cursor { .. } | ServerMsg::Ime { .. } => "state",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["state", "state", "notice", "copied"]);
    }
}
