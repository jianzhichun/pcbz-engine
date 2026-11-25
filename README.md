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
