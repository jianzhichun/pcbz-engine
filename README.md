# pcbz-engine

Builds `pcbcraft-cli`, the headless MCP engine of [PCBCraft](https://github.com/tangency-labs/pcbcraft),
from source on GitHub-hosted runners, because upstream publishes no release yet.

- `UPSTREAM_COMMIT`: the upstream commit built.
- `patches/`: changes applied on top, kept as small as possible.
  `0001-headless-snapshot-project.patch` adds a read-only `snapshot_project` tool to headless
  sessions: it returns the open project as the same portable `.pc` archive the desktop app writes
  (base64, with the session revision), so the host that runs the engine can store the project.
  Headless sessions otherwise have no way to save (`file.*` commands need a live editor).
- `.github/workflows/engine.yml`: builds Linux x64, macOS (Apple silicon) and Windows x64,
  runs `scripts/smoke.py` (handshake, snapshot, reopen) and, for a tag
  `engine-<first 9 of UPSTREAM_COMMIT>-<n>`, publishes a release with `SHA256SUMS.txt`.

PCBCraft is MIT OR Apache-2.0; each archive carries its licences, `ATTRIBUTION.md`, the
third-party licences of the crates built in, the patches and `SOURCE.txt`.
The files in this repository are likewise MIT OR Apache-2.0.

## Render server (M1 spike, M2 live editor)

`render-server/` is a new app, `pcbcraft-render-server`, copied into `apps/` of the upstream tree by
`.github/workflows/render-server.yml` (a separate workflow: `engine.yml` and its release path are untouched). It draws
the upstream editor with no window, offscreen on a headless wgpu device.

- `--snapshot DIR --frames N --size WxH --adapter auto|hardware|software --project FILE` (M1) writes `frame-0001.png`..
  and `frames.json`.
- `--stream 127.0.0.1:PORT` (M2) is the product's mode: SolidZ's wire protocol v1 on one loopback connection (frames as dirty
  tiles, raw RGBA small and JPEG stripes large with a lossless refinement, input, clipboard through the host, file dialogs
  asked of the host, `design_changed`, `ready`, `bye`), and upstream's live control endpoint on a port of its own whose secret
  comes in `ready`, so `pcbcraft-cli mcp --connect` drives the same session the pane draws. `render-server/PROTOCOL.md` is
  the protocol and lists every difference from SolidZ's.

The workflow builds the editor and the CLI together for Linux x64 (lavapipe), Windows x64 (WARP, static C runtime) and macOS
(Apple silicon, Metal), runs the snapshot smoke and `scripts/stream_proof.py` (a scripted host: frames, a click, a key and
text, `project.rename`, `snapshot_project`, `edit.undo`, a read-only session that refuses an edit), and uploads the frame PNGs,
the protocol log, the timings and the logs. `workflow_dispatch` takes `targets` to build only some platforms.

Upstream patches added for it (each only adds or guards; `0001` and `0002` are unchanged):

| Patch | What |
|---|---|
| `0002-embedded-host.patch` | build the app without eframe; busy/revision/status queries (M1) |
| `0003-embedded-flag.patch` | `embedded` flag: embedded viewports (no OS windows), a "new project" refusal that does not wait on "unsaved" |
| `0004-recovery-dir.patch` | recovery snapshots go to the folder the host passes, never the per-user folder; no timed save to the project file |
| `0005-session-scope.patch` | `Session::set_scope` and `PcbApp::set_read_only`: a running session turned read-only by the host |
| `0006-live-snapshot-packed.patch` | the live control server answers `project.snapshot_packed` as the headless engine does; `PcbApp::start_control` |
| `0007-trash-guard.patch` | no system trash (no `gio`/`osascript`/`powershell`) from the embedded app |
| `0008-product-name.patch` | the name a user sees is PCBZ (strings only) |
| `0009-cjk-fonts.patch` | a system CJK font behind Plex, so Chinese draws |

`render-server/workspace.patch` and `render-server/Cargo.lock.patch` (applied by the render-server workflow only) replace the `rfd`
crate with `render-server/shims/rfd`, whose `FileDialog` asks the host process instead of the operating system, and carry the lock
entries that follow (the new member, `rfd` as a path crate, `image`'s JPEG codec). They are not in `patches/` because `engine.yml`
builds the CLI with `--locked` from a tree that has no render server.
