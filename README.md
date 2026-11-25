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

## Render server (M1 spike)

`render-server/` is a new app, `pcbcraft-render-server`, copied into `apps/` of the upstream tree by
`.github/workflows/render-server.yml` (a separate workflow: `engine.yml` and its release path are untouched). It draws
the upstream editor with no window, offscreen on a headless wgpu device, and `--snapshot DIR --frames N --size WxH
--adapter auto|hardware|software --project FILE` writes `frame-0001.png`.. and `frames.json`. The workflow builds it for
Linux x64 (lavapipe), Windows x64 (WARP, static C runtime) and macOS (Apple silicon, Metal), runs that snapshot on
`examples/sensor-node.pc` and uploads the PNGs and logs. `patches/0002-embedded-host.patch` adds the one upstream file it
needs (`crates/app/src/embedded.rs`: build the app without eframe, and tell when a project open finished);
`render-server/Cargo.lock.patch` is the new workspace member's lock entry, applied only by that workflow.
