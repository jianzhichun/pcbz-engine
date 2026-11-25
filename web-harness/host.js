// Host side of the wasm page proof: the project route the plugin would provide.
//
// The editor calls `globalThis.pcbzHostPick` instead of its file input and
// `globalThis.pcbzOnSave(bytes, name)` instead of its anchor download (patch 0011). This
// script records every saved archive and POSTs it back to the Node server, which writes it
// to disk for the diff check. It also exposes the small calls the driver makes.

(() => {
  const harness = {
    inited: false,
    initMs: null,
    loadStarted: null,
    saved: null,
    savedName: null,
    savedSha: null,
    savedResponse: null,
    errors: [],
  };
  window.__pcbzHarness = harness;

  async function sha256(bytes) {
    const digest = await crypto.subtle.digest('SHA-256', bytes);
    return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  }

  window.pcbzOnSave = (bytes, name) => {
    // `bytes` is a view into wasm memory; keep a copy before the module can reuse it.
    const copy = new Uint8Array(bytes);
    harness.saved = copy;
    harness.savedName = name;
    harness.savedAt = performance.now();
    sha256(copy).then((sha) => {
      harness.savedSha = sha;
    });
    fetch('/save.pc?name=' + encodeURIComponent(name), { method: 'POST', body: copy })
      .then((response) => {
        if (!response.ok) throw new Error('save HTTP ' + response.status);
        return response.text();
      })
      .then((text) => {
        harness.savedResponse = text;
      })
      .catch((error) => harness.errors.push('save: ' + String(error)));
  };

  window.pcbzHostPick = async () => {
    const response = await fetch('/project.pc');
    if (!response.ok) throw new Error('project HTTP ' + response.status);
    return response.arrayBuffer();
  };

  window.__pcbzLoad = async (url) => {
    const response = await fetch(url || '/project.pc');
    if (!response.ok) throw new Error('project HTTP ' + response.status);
    const buffer = await response.arrayBuffer();
    harness.loadStarted = performance.now();
    window.__pcbzApp.pcbz_host_open(new Uint8Array(buffer));
    return buffer.byteLength;
  };
  window.__pcbzSave = () => window.__pcbzApp.pcbz_host_save();
  window.__pcbzCommand = (id, params) => window.__pcbzApp.pcbz_host_command(id, JSON.stringify(params || {}));
  window.__pcbzStatus = () => window.__pcbzApp.pcbz_host_status();
})();