# pcbcraft-render-server wire protocol, version 1 (PCBZ)

The render server runs upstream's PCB/schematic editor (`PcbApp`, embedded) with no window, draws each
frame into an offscreen texture, and streams it to an embedding host. The host sends the user's pointer,
keyboard and IME input back as UI input. The project file, the exports and the user's clipboard stay the
host's; the model drives the same session over upstream's own control channel
(`pcbcraft-cli mcp --connect`).

This is SolidZ's `solidcraft-render-server` protocol v1: same envelope, same message kinds, same field
names, same frame rules, so the host and page code written for SolidZ's stream (`StreamSession`,
`StreamHub`, `StreamEngine`, `FrameDrawer`, `InputCapture`) reads it unchanged. `v` stays 1 because every
difference below is an addition an old peer can skip (a kind or field it does not know), except where
marked **changed**. The host keeps the same kinds as TypeScript types in its own `contract.ts`.

## Differences from SolidZ v1

| Where | SolidZ v1 | PCBZ |
|---|---|---|
| program, env | `solidcraft-render-server`, `SOLIDCRAFT_RENDER_TOKEN`, `SOLIDCRAFT_CONTROL_TOKEN`, `SOLIDCRAFT_RENDER_LOG` | `pcbcraft-render-server`, `PCBZ_RENDER_TOKEN`, `PCBZ_RENDER_LOG`. **No control token variable:** the editor makes the control secret and returns it in `ready` |
| flags | `--lang`, `--msaa`, `--open`, `--script`, `--hold-ms` | no `--lang` (the UI is English; the product name is PCBZ), no `--msaa` (the UI is 2D), `--project FILE` (the `.pc` archive or folder to open) in place of `--open`, no `--script`; `--snapshot DIR --frames N --warmup N --gap-ms N` as in M1 |
| `--config-dir DIR` | SolidCraft's per-user folders | recovery snapshots go to `DIR/recovery`; with no flag there is **no** recovery (never the per-user folder); the editor keeps no other file (no settings, no token file, no discovery file) |
| `ready` | `controlPort` | adds `controlSecret: string` (64 hex): give it to `pcbcraft-cli mcp --connect PORT` in `PCBCRAFT_CONTROL_TOKEN`. `samples` is always 1. `ready` comes after the startup project opened |
| `design_changed` (server → host) | none (the host followed `document.revision` itself) | `{revision: u64}`: the open project's revision moved (an edit, undo or redo, from the pane or the control channel); sent after `ready` for the opened project, then once per pass whose revision differs from the last sent. Revisions only grow within a session |
| `notice` (server → host) | host ↔ page only | the server may also send `{level: "info"\|"warn", text}` (a Chinese sentence for the user): today only "folders cannot be picked" |
| `paste_request` (server → host) | none | a Paste button in the editor was pressed: the host answers with `input` `paste {text}` (the clipboard is the host's) |
| `copied` | sent after the render server put the text on the system clipboard (arboard) | sent for every copy, **the render server never touches the system clipboard** (no arboard): the host puts `text` on the user's clipboard |
| `ask_open` / `ask_save` / `ask_done` / `answer` | `Services::pick_*` futures polled by the UI each pass | the `rfd` crate is replaced by a shim: each of the editor's 13 file dialogs blocks its own worker thread until `answer` (the UI loop never waits). `ask_open.exts` is every extension of every filter (no dot); `ask_save.name` is the suggested name, `exts` the filters'. **Folder picks and saves with no filter (a new project or library folder) are cancelled at once with a `notice`**. `ask_done {ask, ok}` is sent when the thread that got the answer ends (the editor's save worker has written the file and reported): `ok` = the answer named a path. A thread that never ends sends none |
| `policy` | host read-only hook + `document.import` backstop | `Session::set_scope` (upstream patch 0005): the engine refuses every command that changes the project, from the pane, the control channel, undo and redo; reading, selecting, exporting stay. The reason is shown on the editor's status bar. `null` makes the session editable again |
| Product name | SolidZ | PCBZ (`PCBCraft`/`PCBcraft` in every string the editor shows: app bar, About, window title, menus, dialogs, settings text, file filter names) |
| Frame codec | raw ≤ 64k px, JPEG stripes, PNG refinement | unchanged; the refinement matters more for thin PCB lines |

## Legs and framing

| Leg | Between | Transport | Framing |
|---|---|---|---|
| A | render server and host process | one loopback TCP connection; the render server connects to `--stream 127.0.0.1:PORT` (a listener the host made for this one process) | `u32 LE length` + envelope, repeated |
| B | host process and the page | one WebSocket (binary messages) on the host's own origin | one WebSocket message = one envelope |

**Envelope** = `u32 LE header_len` + `header_len` bytes of UTF-8 JSON (an object with `"k"`, the kind)
+ payload bytes (the rest of the envelope). Only `frame` has a payload; every other kind has none.

Limits: header at most 1 MiB (`1 << 20`); envelope at most 64 MiB (`64 << 20`). A peer that receives
more, a header that is not a JSON object, a kind it does not know for that direction, or `v` other
than its own sends `error` and closes the connection.

Messages on one leg arrive in the order they were sent. The host relays to the page exactly
`frame`, `cursor`, `ime`, `copied` (unchanged, payload included), and to the render server exactly
the page's `input`, `resize`, `visible`, `ack` (unchanged). Everything else is the host's own.

## Units

- Positions, sizes and rectangles exchanged with the UI: **CSS pixels of the page's pane = egui
  points**. The render server runs the context at zoom 1 with `pixels_per_point = dpr`, so
  `RawInput.screen_rect = Rect(0, 0, w, h)` in points and
  `viewports[ROOT].native_pixels_per_point = Some(dpr)`.
- A frame's `w`, `h` and its rectangles: **device pixels** (`round(w_css * dpr)`), at most 4096 per
  side (a larger pane is drawn at a lower dpr, which `frame.dpr` reports). The pane itself may be up
  to 16384 CSS px a side (the host refuses more); the UI is laid out over all of it.
- Times: milliseconds, as durations only. The two processes' clocks are never compared.

## Starting (leg A)

```
pcbcraft-render-server --stream 127.0.0.1:PORT [--session ID] [--size WxH] [--dpr N] [--theme dark|light] [--project FILE] [--config-dir DIR] [--adapter auto|hardware|software]
```

Loopback only (`--stream` with another address exits 1 before `hello`). `--config-dir DIR` must be absolute (a relative
one exits 1 before `hello`, the reason on stderr): recovery snapshots, which are off until the person turns them
on in Preferences, go to `DIR/recovery`; with no flag there are none. `--project FILE` is a `.pc` archive, a project
folder or its `project.json`, opened before `ready`; one that fails to open ends the process with `bye error` and the
status text.

Environment: `PCBZ_RENDER_TOKEN` (64 lowercase hex, required: sent back in `hello`, proves the connection is the
process the host started), `PCBZ_RENDER_LOG=error|warn|info|debug`. The render server writes **no** token file and **no**
discovery file: a user's own `pcbcraft-cli mcp --connect` can never find it; the control endpoint is on `127.0.0.1:0`
and its secret travels only in `ready`.

M1 mode, no host: `--snapshot DIR [--frames N] [--size WxH] [--dpr N] [--adapter ...] [--project FILE] [--warmup N] [--gap-ms N]`
draws N frames, writes `DIR/frame-0001.png` … and `DIR/frames.json`, and exits 0.

Sequence:

1. render server connects, opens the graphics device (`hello` names it), sends `hello` within 15 s
   of starting. When no device opens it sends nothing, writes the reason on stderr and exits 1:
   the host sees an exit before `ready` (its session goes to the viewer at once), not a crash.
2. host checks `v` and the token (constant time); wrong: `error` and close. Right: the host sends
   `policy`, `resize`, `visible` (current state; `resize` may equal the flags).
3. render server opens the editor and the control channel (`127.0.0.1:0`), opens the startup project, draws the first
   frame, sends `ready` (within 60 s of `hello`), then `design_changed` for the opened project.
4. steady state: `frame` / `cursor` / `ime` / `copied` / `ask_*` up; `input` / `resize` / `visible` /
   `ack` / `policy` / `answer` down.
5. end: host sends `stop` → render server finishes the frame, stops the control channel, sends `bye`,
   exits within 5 s (else the host kills it). The render server also exits, with `bye`
   `host-gone` if it can still write, when leg A closes or errors: it never outlives its host.
   An exit without `bye` is a crash to the host.

## Messages

Field names are exactly as written (JSON). Rust: one enum per direction with
`#[serde(tag = "k", rename_all = "snake_case")]`; struct fields `#[serde(rename_all = "camelCase")]`
(the multi-word fields are `inputSeq`, `controlPort`, `decodeMs`, `readOnly`). Optional fields
(`detail`) are omitted when absent; `null` is written where the type says nullable.

### Render server → host

| `k` | fields | when |
|---|---|---|
| `hello` | `v: u32` (=1), `token: string`, `pid: u32`, `version: string` (the app version), `backend: "metal"\|"dx12"\|"vulkan"\|"gl"\|null`, `adapter: string`, `software: bool` | first message |
| `ready` | `controlPort: u16`, `controlSecret: string` (**PCBZ**), `w: u32`, `h: u32` (device px), `dpr: f32`, `samples: u32` (always 1) | control channel up, project open, first frame drawn |
| `design_changed` | `revision: u64` (**PCBZ**) | the project's revision moved; see Differences |
| `frame` | `seq: u64` (from 1), `inputSeq: u64` (highest `input.seq` the pass read; 0 none), `w: u32`, `h: u32`, `dpr: f32`, `full: bool`, `rects: [FrameRect]`, `timing: FrameTiming`; payload | below |
| `cursor` | `icon: CursorIcon` | when the cursor changes |
| `ime` | `ime: ImeState \| null` | when it changes |
| `copied` | `text: string` | the UI copied text; the host puts it on the user's clipboard |
| `paste_request` | — (**PCBZ**) | a Paste button was pressed; answer with `input` `paste` |
| `notice` | `level: "info"\|"warn"`, `text: string` (**PCBZ**) | something the editor could not do here |
| `ask_save` | `ask: u32`, `name: string`, `exts: [string]` (no dot) | a save dialog with at least one file type was opened |
| `ask_open` | `ask: u32`, `exts: [string]` (empty: any) | an open dialog was opened |
| `ask_done` | `ask: u32`, `ok: bool` | the worker thread that got the answer ended (the file is written) |
| `bye` | `why: "stop"\|"host-gone"\|"device-lost"\|"error"`, `detail?: string` | last message |
| `error` | `code: "version"\|"token"\|"malformed"\|"too-large"\|"unknown-kind"\|"order"`, `message: string` | then close |

### Host → render server

| `k` | fields | effect |
|---|---|---|
| `input` | `seq: u64` (increasing from 1 per page attach; the render server only echoes the last one it read as `frame.inputSeq` and never checks the order), `events: [InputEvent]` | appended to the next pass's `RawInput.events`, in order |
| `resize` | `w: u32`, `h: u32` (CSS px), `dpr: f32` | new target size; the next frame is `full` |
| `visible` | `visible: bool` | false: passes still run (control requests, timers) but nothing is read back or encoded; true: the next frame is `full` |
| `ack` | `seq: u64`, `decodeMs: f32` | one credit back (start: 2 credits) |
| `policy` | `readOnly: string \| null` | read-only on with this reason (`Session::set_scope`), or off |
| `answer` | `ask: u32`, `path: string \| null` | resolves that ask's picker future; null = cancel |
| `stop` | `why: string` | finish and exit |
| `error` | as above | then close |

A save's `answer` is a place of the host's (its staging folder): the UI writes the file there, the
host delivers it after `ask_done` and tells the user where it went. The UI's own status message that
names that place is not shown (an error names the file alone), so the user reads only the host's
location; nothing else on the wire changes.

### Host ↔ page only (the render server never sees these)

`attach {v, session, w, h, dpr, theme, lang}` (page → host, first), `choose_cancel {ask}`,
`restart {}`; `status {state: "starting"|"live"|"fallback"|"stopped", reason?, readOnly}`,
`choose_file {ask, exts}`, `notice {level: "info"|"warn", text}` (host → page). Listed so the kinds
never collide.

## Shared types

**FrameRect** `{x: u32, y: u32, w: u32, h: u32, codec: "jpeg"|"png"|"rgba", ew: u32, eh: u32, offset: u32, length: u32}`:
a changed rectangle in device pixels; `ew`×`eh` is the encoded image's size (v1 always `= w`×`h`; a
later draft mode may send it smaller and the page scales it); `offset`/`length` locate its bytes in
the payload. `rgba`: `ew*eh*4` bytes, rows top to bottom, no padding, straight alpha = 255.
`jpeg`/`png`: a complete file each.

**FrameTiming** `{pass: f32, paint: f32, readback: f32, diff: f32, encode: f32}` ms: the UI pass
(`raw_input_hook` + `logic` + `ui`), tessellate + GPU encode/submit, copy back to the CPU (map
wait included), finding the changed tiles, encoding.

**Frame rules.** The page keeps the last frame and draws each rectangle over it. `full: true` means
the rectangles cover the whole `w`×`h`; it is sent first, after every `resize`, after `visible: true`,
and whenever `w`/`h`/`dpr` change. A frame is produced only when the pass painted something different
and a credit is left; while credits are 0 the changed region keeps growing (it is relative to the last
frame *sent*, so nothing is lost) and the next frame carries all of it. At most 60 frames a second.

**InputEvent**: an object with `"e"`. Rust: `#[serde(tag = "e", rename_all = "snake_case")]`.
Each maps to one `egui::Event` (egui 0.37, `crates/egui/src/data/input/event.rs`):

| `e` | fields | `egui::Event` |
|---|---|---|
| `move` | `x, y: f32` | `PointerMoved(pos2(x, y))` |
| `button` | `x, y: f32`, `button: "primary"\|"secondary"\|"middle"\|"extra1"\|"extra2"`, `pressed: bool`, `mods: Mods` | `PointerButton { pos, button, pressed, modifiers }` |
| `gone` | — | `PointerGone` |
| `wheel` | `dx, dy: f32` (already the UI's sign: −DOM delta), `unit: "point"\|"line"\|"page"`, `mods` | `MouseWheel { unit, delta: vec2(dx, dy), phase: TouchPhase::Move, source: MouseWheelSource::Unknown, modifiers }` |
| `zoom` | `factor: f32` | `Zoom(factor)` |
| `key` | `key: string` (DOM `key`), `physical: string \| null` (DOM `code`), `pressed, repeat: bool`, `mods` | `Key { key: Key::from_name(key).or(Key::from_name(physical)), physical_key: physical.and_then(Key::from_name), pressed, repeat, modifiers }`; dropped when neither name maps |
| `text` | `text` | `Text(text)` |
| `preedit` | `text`, `range: [u32, u32] \| null` (chars) | `Ime(ImeEvent::Preedit { text, active_range_chars: range.map(\|[a, b]\| a..b) })` |
| `commit` | `text` | `Ime(ImeEvent::Commit(text))` |
| `delete_surrounding` | `before, after: u32` | `Ime(ImeEvent::DeleteSurrounding { before_chars, after_chars })` |
| `copy` / `cut` | — | `Copy` / `Cut` |
| `paste` | `text` | `Paste(text)` |
| `focus` | `focused: bool` | `WindowFocused(focused)`, and `RawInput.focused` |
| `mods` | `mods` | `ModifiersChanged(modifiers)` |

**Mods** `{alt, ctrl, shift, meta: bool}` → `egui::Modifiers { alt, ctrl, shift, mac_cmd, command }`
with, on macOS, `mac_cmd = meta, command = meta`, elsewhere `mac_cmd = false, command = ctrl` (what
egui-winit does for a native window; the page cannot know which system the render server runs on).

**CursorIcon**: the `egui::CursorIcon` variant name (`Default`, `None`, `ContextMenu`, `Help`,
`PointingHand`, `Progress`, `Wait`, `Cell`, `Crosshair`, `Text`, `VerticalText`, `Alias`, `Copy`,
`Move`, `NoDrop`, `NotAllowed`, `Grab`, `Grabbing`, `AllScroll`, `ResizeHorizontal`, `ResizeNeSw`,
`ResizeNwSe`, `ResizeVertical`, `ResizeEast`, `ResizeSouthEast`, `ResizeSouth`, `ResizeSouthWest`,
`ResizeWest`, `ResizeNorthWest`, `ResizeNorth`, `ResizeNorthEast`, `ResizeColumn`, `ResizeRow`,
`ZoomIn`, `ZoomOut`), from `PlatformOutput.cursor_icon`.

**ImeState** `{purpose: "normal"|"password"|"terminal", rect: [x, y, w, h], cursor: [x, y, w, h], interrupt: bool}`
from `PlatformOutput.ime` (`IMEOutput { purpose, rect, cursor_rect, should_interrupt_composition }`),
in points. The page puts its hidden text area at `cursor`, so the system's candidate box opens at the
caret.

## Product name

The editor says PCBZ wherever upstream's says PCBCraft or PCBcraft in a string a user sees (patch 0008). Its own
documentation, release and bug-report links are not opened (`OpenUrl` is ignored). Fixed in the render server; there is
no flag for it. The UI is English (upstream's catalog); Chinese text in names draws with the first system CJK font
(patch 0009).

## What the render server does with the platform output

- `cursor_icon` → `cursor` (on change). `ime` → `ime` (on change).
- `OutputCommand::CopyText(t)` → `copied` (always). `CopyImage`, `OpenUrl` and `TextSelectionSettled` → ignored (logged).
- `ViewportCommand::RequestPaste` → `paste_request`.
- Other viewport commands: `Close`, `Minimized`, `Maximized`, `BeginResize`, `StartDrag`, `Title`, `Focus`,
  `CursorPosition` are ignored; the pane is not a window. Detached editor windows are windows inside the pane (egui's
  embedded viewports); they close from the editor's own controls.

## Read-only (`policy`)

`readOnly: "<reason>"` makes the live session read-only (`Session::set_scope(ReadOnly)`): every command that
changes the project is refused with `out_of_scope` ("`project.rename` cannot run in a read-only session"), whoever sends
it (the pane's tools, the control channel's `execute`, undo and redo); opening a project, looking, selecting, measuring and
exporting stay. `project.snapshot_packed` (the bridge's `snapshot_project`) stays: saving what is there is reading.
The reason shows on the editor's status bar. `null` makes the session editable again.

## The control channel (upstream's own)

`ready.controlPort` is upstream's `LiveControl` (loopback, token-authenticated), started by the render server with a request
wait of 300 s. `pcbcraft-cli mcp --connect PORT --wait 300` with `PCBCRAFT_CONTROL_TOKEN=<controlSecret>` serves the MCP
tools against the session the pane draws: one document, one undo history. Requests run inside a frame pass, so frames
keep running while the pane is hidden (`visible: false` stops frames, not passes). The live server answers
`project.snapshot_packed` as the headless engine does (patch 0006), so `snapshot_project` works; its reply is capped at
16 MiB with base64 (about 12 MiB of archive).
