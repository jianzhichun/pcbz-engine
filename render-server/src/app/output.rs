//! What the UI asks of its platform after a pass (`egui::PlatformOutput`), as wire messages
//! (PROTOCOL.md, "What the render server does with the platform output"): cursor and IME only when
//! they change, copied text to the host (which owns the person's clipboard: PCBZ does not open the
//! system clipboard from here), a paste request when a Paste button was pressed. Links are not
//! opened (the pane is not a browser of its own).

use crate::domain::wire::{ImePurpose, ImeState, ServerMsg};

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
                egui::OutputCommand::CopyText(text) => msgs.push(ServerMsg::Copied { text }),
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
