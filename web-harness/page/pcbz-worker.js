// PCBZ wasm page: the dedicated module Worker. It imports the trunk-built app module, instantiates the hashed wasm with
// `init({module_or_path})` (trunk renames it, so the page cannot rely on trunk's autoload), and runs the editor loop on
// the OffscreenCanvas it was handed. Nothing here touches the host's main thread: frames are painted in this Worker, and
// the page only receives small status messages (cursor, clipboard, IME, flush acks). `pcbz-host.js` installs the
// load/save/export globals the upstream editor calls; every project byte goes through the plugin's routes.

import { ackFlush, beginFlush, fetchProject, host, installHost, setBase, setRevision } from './pcbz-host.js'

let app = null
let canvas = null
let ready = false
let closed = false
let firstFrame = false
let pending = []
let scheduled = false
let flushNotes = []
let visible = true
let urls = null

const post = (message) => { try { self.postMessage(message) } catch { /* page gone */ } }
const text = (error) => (error && typeof error.message === 'string' ? error.message : String(error))

function probeBackend() {
  if (!app || typeof app.pcbz_worker_probe !== 'function') return 'unknown'
  try {
    const info = JSON.parse(app.pcbz_worker_probe() || '{}')
    return info && typeof info.backend === 'string' ? info.backend : 'unknown'
  } catch { return 'unknown' }
}

function status() {
  if (!app) return {}
  try { return JSON.parse(app.pcbz_host_status() || '{}') } catch { return {} }
}

function fail(code, reason) {
  if (closed) return
  closed = true
  post({ t: 'failed', code, reason: typeof reason === 'string' && reason ? reason : '编辑界面没有启动。' })
}

function schedule(delay = 0) {
  if (closed || scheduled || !ready) return
  scheduled = true
  const run = () => { scheduled = false; pump() }
  if (delay > 0) setTimeout(run, delay)
  else if (typeof self.requestAnimationFrame === 'function') self.requestAnimationFrame(run)
  else setTimeout(run, 16)
}

// egui asks for a frame from inside the wasm; patch 0012's worker host calls this global.
globalThis.pcbzWorkerWake = () => schedule(0)

function pump() {
  if (!app || !ready || closed) return
  console.log('DBG pump', Math.round(performance.now()), JSON.stringify(status()))
  const batch = pending.splice(0)
  let report
  try {
    report = JSON.parse(app.pcbz_worker_frame(JSON.stringify(batch)))
  } catch (error) {
    fail('frame', `编辑界面的绘制出错了：${text(error).slice(0, 160)}`)
    return
  }
  if (report && report.error) {
    fail('frame', `编辑界面的绘制出错了：${String(report.error).slice(0, 160)}`)
    return
  }
  if (report && typeof report === 'object') {
    if (!firstFrame && report.painted) firstFrame = true
    post({ t: 'frame', ...report })
  }
  if (!visible && !pending.length && !(report && report.repaint)) return
  const delay = report && report.repaint ? Math.max(0, Math.min(250, Number(report.delay_ms) || 0)) : (pending.length ? 0 : -1)
  if (delay >= 0) schedule(delay)
}

async function init(message) {
  urls = message.urls
  canvas = message.canvas
  installHost({ urls, baseHash: message.baseHash, revision: message.revision, readOnly: message.readOnly, post })
  host.readOnly = message.readOnly === true

  let font = new Uint8Array()
  try {
    const response = await fetch(urls.font, { cache: 'force-cache' })
    if (response.ok) font = new Uint8Array(await response.arrayBuffer())
    else flushNotes.push(`font HTTP ${response.status}`)
  } catch { /* the page still runs, without Chinese glyphs */ }

  const started = performance.now()
  let module
  try {
    module = await import(urls.app)
  } catch (error) {
    fail('module', `编辑界面的程序读不出来：${text(error).slice(0, 160)}`)
    return
  }
  try {
    await module.default({ module_or_path: urls.wasm })
  } catch (error) {
    fail('wasm', `编辑界面的程序没有启动：${text(error).slice(0, 160)}`)
    return
  }
  try {
    await module.pcbz_worker_start(message.canvas, message.css.w, message.css.h, message.css.dpr, message.dark === true, message.mac === true, font)
  } catch (error) {
    fail('surface', `编辑界面没有可用的图形设备：${text(error).slice(0, 160)}`)
    return
  }
  app = module
  ready = true
  if (message.readOnly === true) app.pcbz_host_read_only(true)
  if (message.hasProject === true) {
    try {
      const buffer = await globalThis.pcbzHostPick()
      app.pcbz_host_open(new Uint8Array(buffer))
    } catch {
      // the state named a bound project and it did not arrive: Home would claim the project is empty, so say so instead
      fail('project', '工程没有读出来，编辑界面没有打开这个工程。')
      return
    }
  }
  post({ t: 'ready', initMs: Math.round(performance.now() - started), backend: probeBackend(), font: font.length > 0 })
  for (const note of flushNotes.splice(0)) post({ t: 'log', message: note })
  schedule(0)
}

function flush(id) {
  console.log('DBG flush', Math.round(performance.now()), JSON.stringify(status()))
  beginFlush(id)
  const state = status()
  if (!state.project) { ackFlush(true); return }
  app.pcbz_host_save()
  schedule(0)
}

async function reload(hash, revision) {
  if (!app) { post({ t: 'reloaded', ok: false, reason: '编辑界面没有启动。' }); return }
  try {
    const { buffer, hash: fetched } = await fetchProject()
    app.pcbz_host_open(new Uint8Array(buffer))
    // the plugin's reload names the saved hash; a page-initiated reload (after a stale-base refusal) has none, and the
    // bytes just fetched do -- never the base that was refused
    setBase(hash || fetched || '')
    setRevision(revision)
    schedule(0)
    waitForOpen(revision, performance.now() + 8000)
  } catch {
    post({ t: 'reloaded', ok: false, reason: '工程没有读出来。' })
  }
}

function waitForOpen(revision, deadline) {
  const state = status()
  if (!state.busy) {
    post({ t: 'reloaded', ok: Boolean(state.project), ...(state.project ? { revision: typeof state.revision === 'number' ? state.revision : revision } : { reason: '工程没有打开。' }) })
    return
  }
  if (performance.now() >= deadline) { post({ t: 'reloaded', ok: false, reason: '工程打开没有完成。' }); return }
  setTimeout(() => waitForOpen(revision, deadline), 100)
}

function base64(bytes) {
  let out = ''
  const chunk = 0x8000
  for (let i = 0; i < bytes.length; i += chunk) out += String.fromCharCode(...bytes.subarray(i, i + chunk))
  return btoa(out)
}

self.onmessage = (event) => {
  const message = event.data
  if (!message || typeof message.t !== 'string' || closed) return
  switch (message.t) {
    case 'init':
      void init(message)
      return
    case 'input':
      if (Array.isArray(message.events) && message.events.length) { pending.push(...message.events); schedule(0) }
      return
    case 'flush':
      if (typeof message.id === 'string') flush(message.id)
      return
    case 'reload':
      if (typeof message.hash === 'string') void reload(message.hash, typeof message.revision === 'number' ? message.revision : null)
      return
    case 'resize':
      if (app) { app.pcbz_worker_resize(Number(message.w) || 1, Number(message.h) || 1, Number(message.dpr) || 1); schedule(0) }
      return
    case 'focus':
      if (app) app.pcbz_worker_set_focus(message.focused === true)
      return
    case 'read-only':
      host.readOnly = message.readOnly === true
      if (app) { app.pcbz_host_read_only(message.readOnly === true); schedule(0) }
      return
    case 'visible':
      visible = message.visible === true
      if (visible) schedule(0)
      return
    case 'proof-text':
      if (app && typeof app.pcbz_worker_debug_text === 'function') { app.pcbz_worker_debug_text(String(message.text || '')); schedule(0) }
      return
    case 'proof-command':
      if (app) { app.pcbz_host_command(String(message.id || ''), JSON.stringify(message.params || {})); schedule(0) }
      return
    case 'proof-status':
      post({ t: 'proof-status', id: message.id, json: app ? app.pcbz_host_status() : '{}' })
      return
    case 'proof-snapshot':
      if (!canvas || typeof canvas.convertToBlob !== 'function') { post({ t: 'proof-png', id: message.id, error: 'no canvas snapshot' }); return }
      // Capture instrument (harness only; the product never snapshots): a WebGL drawing buffer is
      // only guaranteed to hold the frame inside the task that drew it (wgpu creates the context
      // without preserveDrawingBuffer), so paint one pass and copy it in that same task.
      if (app) { try { app.pcbz_worker_frame('[]') } catch { /* the page reports frame errors itself */ } }
      canvas.convertToBlob({ type: 'image/png' })
        .then(async (blob) => post({ t: 'proof-png', id: message.id, png: base64(new Uint8Array(await blob.arrayBuffer())) }))
        .catch((error) => post({ t: 'proof-png', id: message.id, error: text(error) }))
      return
    default:
      return
  }
}
