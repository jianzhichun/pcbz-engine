// PCBZ wasm page: the iframe document's own script. It creates the dedicated Worker, transfers the canvas to it, and
// speaks three protocols: to the plugin over the routes and the control socket, to the Worker over postMessage, and to
// the pane over window.postMessage (probe, ready, failed, read-only). The editor itself never runs here (red line a):
// this document only captures DOM input, applies the cursor/clipboard/IME the Worker reports, and draws nothing.
//
// The Worker owns every project byte: `pcbz-host.js` in the Worker fetches /pcbz/wasm/project and POSTs /pcbz/wasm/save
// with X-PCBZ-Base-Hash. This script never sees an archive. A read-only page shows the banner and passes the flag to the
// app; the save route refuses as well.

const V = 1
const STATE_PATH = '/pcbz/wasm/state'
const PROJECT_PATH = '/pcbz/wasm/project'
const SAVE_PATH = '/pcbz/wasm/save'
const NEW_PATH = '/pcbz/wasm/new'
const EXPORT_PATH = '/pcbz/wasm/export'
const CONTROL_PATH = '/pcbz/wasm/control'
const PROBE_PATH = '/pcbz/wasm/probe'
const FLUSH_TIMEOUT_MS = 9500
const RELOAD_TIMEOUT_MS = 12000
const READ_ONLY_TEXT = '只读：当前授权不能修改这个工程。'
const LOCK_TEXT = '助手正在修改这个工程，编辑已暂停。'
const FAILED_TEXT = '编辑界面没有启动，已改用图纸视图。'
const STATE_TEXT = '编辑界面的状态读不出来，已改用图纸视图。'

const params = new URLSearchParams(location.search)
const session = params.get('session') || ''
const version = params.get('v') || ''
// Every asset the page fetches beside itself carries the plugin version, so the static route may mark it immutable: the
// 9 MiB brotli wasm and the hashed app module then come from the webview's disk cache on every later mount (design §1.4).
const versionQuery = version ? `?v=${encodeURIComponent(version)}` : ''
const stage = document.getElementById('pcbz-stage')
const canvas = document.getElementById('pcbz-canvas')
const textarea = document.getElementById('pcbz-text')
const loading = document.getElementById('pcbz-loading')
const banner = document.getElementById('pcbz-banner')

let worker = null
let control = null
let urls = null
let baseHash = null
let revision = null
let hasProject = false
let backend = 'unknown'
let fontLoaded = false
let probeSent = false
let frameCount = 0
let saveWaiter = null
/** The editor caret from the Worker's frame report (CSS px above the canvas); null outside text editing. */
let lastImeRect = null
const proofWaiters = new Map()
let proofSeq = 0
let readOnly = false
let locked = false
let stopped = false
let probed = false
let announced = false
let pending = []
let scheduled = false
let flushTimer = 0
let reloadTimer = 0
let controlRetry = 1000
let pollTimer = 0

const cjk = (text) => typeof text === 'string' && /[一-鿿]/.test(text)

function postParent(kind, fields = {}) {
  try { parent.postMessage({ t: 'pcbz-wasm', v: V, kind, ...fields }, location.origin) } catch { /* no parent */ }
}

function showBanner(kind, text) {
  banner.dataset.kind = kind
  banner.textContent = text
  banner.style.display = text ? 'block' : 'none'
}

function showLock() {
  if (readOnly) showBanner('read-only', READ_ONLY_TEXT)
  else if (locked) showBanner('lock', LOCK_TEXT)
  else showBanner('none', '')
}

function setReadOnly(flag, reason) {
  readOnly = flag === true
  if (readOnly && cjk(reason)) showBanner('read-only', reason.trim().slice(0, 300))
  else showLock()
  try { worker?.postMessage({ t: 'read-only', readOnly }) } catch { /* gone */ }
  postParent('read-only', { readOnly, ...(readOnly && cjk(reason) ? { reason: reason.trim().slice(0, 300) } : {}) })
}

function fail(code, reason) {
  if (stopped) return
  stopped = true
  const text = cjk(reason) ? reason.trim().slice(0, 300) : FAILED_TEXT
  showBanner('error', text)
  loading?.remove()
  probed = false
  postParent('failed', { ok: false, code: String(code || 'failed'), reason: text })
  clearTimeout(pollTimer)
  pollTimer = 0
  try { worker?.terminate() } catch { /* gone */ }
  try { control?.close() } catch { /* gone */ }
  worker = null
  control = null
}

// ---- capability, state, worker -------------------------------------------------------------------------------------

function capability() {
  if (typeof Worker !== 'function') return 'no-worker'
  if (typeof OffscreenCanvas !== 'function') return 'no-offscreen'
  if (!canvas || typeof canvas.transferControlToOffscreen !== 'function') return 'no-offscreen'
  return null
}

async function fetchState() {
  const response = await fetch(`${STATE_PATH}?session=${encodeURIComponent(session)}`, { cache: 'no-store', headers: { accept: 'application/json' } })
  const body = await response.json().catch(() => null)
  return { status: response.status, body }
}

function stateReason(body) {
  if (body && cjk(body.reason)) return body.reason.trim().slice(0, 300)
  return STATE_TEXT
}

async function start() {
  if (!session) return fail('no-session', '没有会话标识，编辑界面不能打开。')
  const missing = capability()
  if (missing) return fail(missing, '这个宿主没有 Worker 或 OffscreenCanvas，编辑界面不能打开。')
  loading.style.display = 'grid'
  let state
  try { state = await fetchState() } catch { return fail('state', STATE_TEXT) }
  const body = state.body
  if (state.status !== 200 || !body || body.on !== true) return fail('state', stateReason(body))
  const project = body.project && typeof body.project === 'object' ? body.project : null
  if (!project && cjk(body.reason)) return fail('state', body.reason.trim())
  const projectPath = project && typeof project.path === 'string' && project.path.startsWith('/pcbz/wasm/') ? project.path : PROJECT_PATH
  revision = project && typeof project.revision === 'number' ? project.revision : null
  readOnly = body.readOnly === true
  hasProject = project !== null
  // the page's write base is the sha256 of the bytes it loads: the project route's x-pcbz-sha256 names exactly those bytes
  baseHash = null
  if (readOnly) showBanner('read-only', READ_ONLY_TEXT)

  let build
  try {
    const response = await fetch(`./pcbz-build.json${versionQuery}`, { cache: 'no-store' })
    if (!response.ok) throw new Error(String(response.status))
    build = await response.json()
  } catch { return fail('build', '编辑界面的文件清单读不出来。') }
  if (!build || typeof build.app !== 'string' || typeof build.wasm !== 'string') return fail('build', '编辑界面的文件清单不完整。')

  const here = new URL('./', location.href)
  const query = `session=${encodeURIComponent(session)}`
  /** An asset beside the document, with the plugin version as its cache key (`?v=` from the iframe URL). */
  const asset = (path) => {
    const url = new URL(path, here)
    if (version) url.searchParams.set('v', version)
    return url.href
  }
  urls = {
    project: `${projectPath}?${query}`, save: `${SAVE_PATH}?${query}`, newProject: `${NEW_PATH}?${query}`,
    export: `${EXPORT_PATH}?${query}`, control: `${CONTROL_PATH}?${query}`, probe: `${PROBE_PATH}?${query}`,
    font: asset(build.font || 'fonts/NotoSansSC-GB2312.otf'),
    app: asset(build.app),
    wasm: asset(build.wasm),
  }

  const workerUrl = new URL('./pcbz-worker.js', here)
  if (version) workerUrl.searchParams.set('v', version)
  worker = new Worker(workerUrl, { type: 'module', name: 'pcbz-editor' })
  worker.onmessage = (event) => onWorker(event.data)
  worker.onerror = (event) => fail('worker', event?.message ? `编辑界面意外退出：${String(event.message).slice(0, 160)}` : FAILED_TEXT)
  worker.onmessageerror = () => fail('worker', FAILED_TEXT)

  const offscreen = canvas.transferControlToOffscreen()
  const dark = typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
  const mac = /Mac|iPhone|iPad/.test(navigator.platform || '')
  // `canvas` is the transferred object itself: a transfer list alone detaches it here but gives the Worker no handle.
  worker.postMessage({
    t: 'init', session, urls, canvas: offscreen, baseHash, revision, readOnly, hasProject,
    css: { w: Math.max(1, stage.clientWidth), h: Math.max(1, stage.clientHeight), dpr: window.devicePixelRatio || 1 },
    dark, mac,
  }, [offscreen])
  resize()
  openControl()
  pollState()
}

// The state answer is also how a licence change reaches an open page without the control socket (design §1.2/§2.3):
// 1 s while visible, 5 s while hidden. An explicit refusal (no licence, not offered, the session gone) stops the page (the
// pane falls back); a read-only flip is applied in place. A failed request or a server fault is not a refusal: an open
// editor with edits in memory keeps going and asks again, because one 500 must not throw the user's work away.
function pollState() {
  if (stopped) return
  const wait = document.visibilityState === 'hidden' ? 5000 : 1000
  fetchState().then(({ status, body }) => {
    if (stopped) return
    if (status === 200 && body && body.on === true) {
      const next = body.readOnly === true
      if (next !== readOnly) setReadOnly(next)
      return
    }
    if (status >= 400 && status < 500) { fail('state', stateReason(body)); return }
    if (status === 200) fail('state', stateReason(body))
    // 0 (no answer) and 5xx: keep the editor, ask again on the next poll
  }).catch(() => undefined).finally(() => {
    if (!stopped) pollTimer = setTimeout(pollState, wait)
  })
}

// ---- worker reports ------------------------------------------------------------------------------------------------

function onWorker(message) {
  if (!message || typeof message.t !== 'string' || stopped) return
  switch (message.t) {
    case 'ready':
      probed = true
      backend = typeof message.backend === 'string' ? message.backend : 'unknown'
      fontLoaded = message.font === true
      postParent('probe', { ok: true })
      return
    case 'frame': {
      frameCount += 1
      if (!announced && message.painted) { announced = true; loading?.remove(); sendProbe(); postParent('ready', { revision }) }
      if (typeof message.cursor === 'string') stage.style.cursor = cursorCss(message.cursor)
      if (Array.isArray(message.copied)) for (const text of message.copied) writeClipboard(String(text))
      if (message.paste === true) readClipboard()
      if (message.ime === true) focusText()
      // The Worker reports the caret in CSS pixels (it owns no DOM): park the hidden textarea there so the browser's IME
      // candidate window opens at the caret. Patch 0015's report carries `ime_rect`; absent means no text edit.
      lastImeRect = message.ime_rect && typeof message.ime_rect === 'object' ? message.ime_rect : null
      if (lastImeRect) placeText(lastImeRect)
      // The hidden textarea holds the keyboard for the editor; it is never blurred because egui is not editing text now.
      // (Blurring it here would lose every key after the next frame: the page listens for keys on the textarea.)
      if (message.ime_interrupt === true) textarea.value = ''
      return
    }
    case 'flush-ack': {
      clearTimeout(flushTimer)
      flushTimer = 0
      if (message.id === 'proof-save' && saveWaiter) {
        const waiter = saveWaiter
        saveWaiter = null
        waiter.resolve(message)
      }
      if (message.ok === false) {
        // no bytes reached the plugin: say so instead of sending a hash the file does not have (design §3.2 step 4)
        sendControl({ t: 'error', code: 'flush-refused' })
        locked = false
        showLock()
        return
      }
      // the model now owns the file: stay locked until its save and the reload (design §3.1-3.2)
      sendControl({ t: 'flush-ack', id: String(message.id || ''), hash: typeof message.hash === 'string' ? message.hash : null, revision: typeof message.revision === 'number' ? message.revision : revision })
      // The proof's own save is not a model turn and no control socket will reload it: keep the page
      // usable for the checks that follow instead of holding the model-turn input lock.
      if (message.id === 'proof-save') { locked = false; showLock(); return }
      locked = true
      showLock()
      return
    }
    case 'reloaded':
      clearTimeout(reloadTimer)
      reloadTimer = 0
      if (message.ok) {
        if (typeof message.revision === 'number') revision = message.revision
        sendControl({ t: 'reloaded', revision })
        locked = false
        showLock()
      } else {
        sendControl({ t: 'error', code: 'reload-failed' })
        locked = false
        showLock()
      }
      return
    case 'saved':
      if (typeof message.hash === 'string') baseHash = message.hash
      if (message.reload === true) {
        // the plugin refused stale bytes (409): drop them and reload what is on disk. The empty hash makes the Worker take
        // the new base from the bytes it fetches (their x-pcbz-sha256); passing the stale base again would be refused again.
        try { worker?.postMessage({ t: 'reload', hash: '', revision }) } catch { /* gone */ }
      }
      return
    case 'read-only':
      setReadOnly(message.readOnly, message.reason)
      return
    case 'failed':
      fail(message.code, message.reason)
      return
    case 'log':
      if (typeof message.message === 'string') console.debug('[pcbz-wasm]', message.message)
      return
    case 'proof-png': {
      const waiter = proofWaiters.get(String(message.id || ''))
      if (waiter) {
        proofWaiters.delete(String(message.id || ''))
        if (typeof message.png === 'string') waiter.resolve(message.png)
        else waiter.reject(new Error(String(message.error || 'snapshot failed')))
      }
      return
    }
    case 'proof-status': {
      const waiter = proofWaiters.get(String(message.id || ''))
      if (waiter) { proofWaiters.delete(String(message.id || '')); waiter.resolve(String(message.json || '{}')) }
      return
    }
    default:
      return
  }
}

// One runtime report per page (design §9): booleans only, no session id, project name or other user data. `webgl2` names
// the backend that actually drew: a WebGPU adapter is not WebGL2, and the per-platform decision needs the truth.
function sendProbe() {
  if (probeSent) return
  probeSent = true
  const report = {
    worker: true,
    webgl2: backend === 'Gl',
    offscreen: true,
    ime: typeof CompositionEvent === 'function',
    cjk: fontLoaded === true,
  }
  try {
    void fetch(urls.probe, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(report), cache: 'no-store' }).catch(() => undefined)
  } catch { /* telemetry only */ }
}

// Proof hooks (the wasm-page workflow drives the real page through them). They are inert in a production
// build: `pcbz_worker_debug_text` exists only with the `proof` cargo feature, and the snapshot/status
// helpers do nothing unless the page was started by the proof runner.
function proofRequest(message, timeoutMs = 20000) {
  const id = String(++proofSeq)
  return new Promise((resolve, reject) => {
    proofWaiters.set(id, { resolve, reject })
    try { worker?.postMessage({ ...message, id }) } catch { proofWaiters.delete(id); reject(new Error('worker gone')) }
    setTimeout(() => { if (proofWaiters.delete(id)) reject(new Error(`proof ${message.t} timed out`)) }, timeoutMs)
  })
}

window.__pcbzProof = {
  get frames() { return frameCount },
  get ready() { return announced },
  get backend() { return backend },
  get font() { return fontLoaded },
  get imeRect() { return lastImeRect },
  text(value) { try { worker?.postMessage({ t: 'proof-text', text: String(value) }) } catch { /* gone */ } },
  command(id, params) { try { worker?.postMessage({ t: 'proof-command', id, params: params || {} }) } catch { /* gone */ } },
  status() { return proofRequest({ t: 'proof-status' }, 10000) },
  snapshot() { return proofRequest({ t: 'proof-snapshot' }) },
  save() {
    return new Promise((resolve, reject) => {
      saveWaiter = { resolve, reject }
      try { worker?.postMessage({ t: 'flush', id: 'proof-save' }) } catch { saveWaiter = null; reject(new Error('worker gone')) }
      setTimeout(() => { if (saveWaiter) { saveWaiter = null; reject(new Error('proof save timed out')) } }, 20000)
    })
  },
}

const CURSORS = {
  Default: 'default', None: 'none', ContextMenu: 'context-menu', Help: 'help', PointingHand: 'pointer', Progress: 'progress',
  Wait: 'wait', Cell: 'cell', Crosshair: 'crosshair', Text: 'text', VerticalText: 'vertical-text', Alias: 'alias', Copy: 'copy',
  Move: 'move', NoDrop: 'no-drop', NotAllowed: 'not-allowed', Grab: 'grab', Grabbing: 'grabbing', AllScroll: 'all-scroll',
  ResizeHorizontal: 'ew-resize', ResizeVertical: 'ns-resize', ResizeNeSw: 'nesw-resize', ResizeNwSe: 'nwse-resize',
  ResizeEast: 'e-resize', ResizeSouth: 's-resize', ResizeWest: 'w-resize', ResizeNorth: 'n-resize',
  ResizeSouthEast: 'se-resize', ResizeSouthWest: 'sw-resize', ResizeNorthEast: 'ne-resize', ResizeNorthWest: 'nw-resize',
  ResizeColumn: 'col-resize', ResizeRow: 'row-resize', ZoomIn: 'zoom-in', ZoomOut: 'zoom-out',
}
const cursorCss = (name) => CURSORS[name] || 'default'

function writeClipboard(text) {
  const clipboard = typeof navigator !== 'undefined' ? navigator.clipboard : undefined
  if (clipboard?.writeText) clipboard.writeText(text).catch(() => undefined)
  else if (typeof document !== 'undefined') {
    // Clipboard API absent: the old selection path, inside this document only
    const area = document.createElement('textarea')
    area.value = text
    area.style.cssText = 'position:absolute;left:-9999px;top:0'
    document.body.append(area)
    area.select()
    try { document.execCommand('copy') } catch { /* refused */ }
    area.remove()
  }
}

function readClipboard() {
  const clipboard = typeof navigator !== 'undefined' ? navigator.clipboard : undefined
  if (!clipboard?.readText) return
  clipboard.readText().then((text) => { if (text) sendEvents([{ e: 'paste', text }]) }).catch(() => undefined)
}

function focusText() {
  if (document.activeElement !== textarea) {
    try { textarea.focus({ preventScroll: true }) } catch { textarea.focus() }
  }
}

/** Park the hidden textarea at the editor's caret so the browser opens its IME candidate window there. The stage is the
 *  canvas's containing block (both inset:0), so a canvas-relative point is a stage-relative one. */
function placeText(rect) {
  const x = Number(rect.x)
  const y = Number(rect.y)
  if (!Number.isFinite(x) || !Number.isFinite(y)) return
  textarea.style.left = `${x}px`
  textarea.style.top = `${y}px`
  const h = Number(rect.h)
  if (Number.isFinite(h) && h > 0) textarea.style.height = `${h}px`
}

// ---- input ---------------------------------------------------------------------------------------------------------

function sendEvents(events) {
  if (locked || stopped || !events.length || !worker) return
  pending.push(...events)
  if (scheduled) return
  scheduled = true
  requestAnimationFrame(() => {
    scheduled = false
    const batch = pending.splice(0)
    if (batch.length && worker && !locked && !stopped) worker.postMessage({ t: 'input', events: batch })
  })
}

const mods = (event) => ({ alt: event.altKey === true, ctrl: event.ctrlKey === true, shift: event.shiftKey === true, meta: event.metaKey === true })
const BUTTONS = ['primary', 'middle', 'secondary', 'extra1', 'extra2']

function point(event) {
  const rect = canvas.getBoundingClientRect()
  return { x: event.clientX - rect.left, y: event.clientY - rect.top }
}

function bindInput() {
  canvas.addEventListener('pointerdown', (event) => {
    focusText()
    if (event.button > 4) return
    try { canvas.setPointerCapture(event.pointerId) } catch { /* not capturable */ }
    const { x, y } = point(event)
    sendEvents([{ e: 'button', x, y, button: BUTTONS[event.button], pressed: true, mods: mods(event) }])
    event.preventDefault()
  })
  canvas.addEventListener('pointermove', (event) => {
    const { x, y } = point(event)
    sendEvents([{ e: 'move', x, y }])
  })
  const release = (event) => {
    if (event.button > 4) return
    const { x, y } = point(event)
    sendEvents([{ e: 'button', x, y, button: BUTTONS[event.button], pressed: false, mods: mods(event) }])
  }
  canvas.addEventListener('pointerup', release)
  canvas.addEventListener('pointercancel', release)
  canvas.addEventListener('pointerleave', () => sendEvents([{ e: 'gone' }]))
  canvas.addEventListener('wheel', (event) => {
    event.preventDefault()
    const unit = event.deltaMode === 1 ? 'line' : event.deltaMode === 2 ? 'page' : 'point'
    sendEvents([{ e: 'wheel', dx: event.deltaX, dy: event.deltaY, unit, mods: mods(event) }])
  }, { passive: false })
  canvas.addEventListener('contextmenu', (event) => event.preventDefault())

  const KEYS = new Set(['Tab', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Backspace', 'Delete', 'Enter', ' ', 'Escape'])
  textarea.addEventListener('keydown', (event) => {
    if (event.isComposing) return
    sendEvents([{ e: 'key', key: event.key, physical: event.code || null, pressed: true, repeat: event.repeat === true, mods: mods(event) }])
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) sendEvents([{ e: 'text', text: event.key }])
    if (KEYS.has(event.key)) event.preventDefault()
  })
  textarea.addEventListener('keyup', (event) => {
    if (event.isComposing) return
    sendEvents([{ e: 'key', key: event.key, physical: event.code || null, pressed: false, repeat: false, mods: mods(event) }])
  })
  textarea.addEventListener('compositionstart', () => { textarea.dataset.composing = '1' })
  textarea.addEventListener('compositionupdate', (event) => {
    if (typeof event.data === 'string') sendEvents([{ e: 'preedit', text: event.data, range: null }])
  })
  textarea.addEventListener('compositionend', (event) => {
    delete textarea.dataset.composing
    textarea.value = ''
    if (typeof event.data === 'string' && event.data) sendEvents([{ e: 'commit', text: event.data }])
  })
  textarea.addEventListener('copy', (event) => { event.preventDefault(); sendEvents([{ e: 'copy' }]) })
  textarea.addEventListener('cut', (event) => { event.preventDefault(); sendEvents([{ e: 'cut' }]) })
  textarea.addEventListener('paste', (event) => {
    event.preventDefault()
    const text = event.clipboardData?.getData('text')
    if (typeof text === 'string' && text) sendEvents([{ e: 'paste', text }])
  })
  textarea.addEventListener('focus', () => { worker?.postMessage({ t: 'focus', focused: true }); sendEvents([{ e: 'focus', focused: true }]) })
  textarea.addEventListener('blur', () => { worker?.postMessage({ t: 'focus', focused: false }); sendEvents([{ e: 'focus', focused: false }]) })
}

function resize() {
  if (!worker) return
  worker.postMessage({ t: 'resize', w: Math.max(1, stage.clientWidth), h: Math.max(1, stage.clientHeight), dpr: window.devicePixelRatio || 1 })
}

// ---- the control socket (design §3.2) ------------------------------------------------------------------------------

function sendControl(message) {
  try { if (control && control.readyState === WebSocket.OPEN) control.send(JSON.stringify(message)) } catch { /* gone */ }
}

function openControl() {
  if (stopped) return
  let socket
  try {
    const url = new URL(`${CONTROL_PATH}?session=${encodeURIComponent(session)}`, location.href)
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
    socket = new WebSocket(url.href)
  } catch { retryControl(); return }
  control = socket
  socket.onopen = () => {
    controlRetry = 1000
    sendControl({ t: 'hello', session, revision })
  }
  socket.onmessage = (event) => {
    if (typeof event.data !== 'string') return   // binary frames are refused by the plugin; ignore any
    let message
    try { message = JSON.parse(event.data) } catch { return }
    if (!message || typeof message.t !== 'string') return
    if (message.t === 'flush' && typeof message.id === 'string') {
      locked = true
      showLock()
      worker?.postMessage({ t: 'flush', id: message.id })
      clearTimeout(flushTimer)
      flushTimer = setTimeout(() => { sendControl({ t: 'error', code: 'flush-timeout' }); locked = false; showLock() }, FLUSH_TIMEOUT_MS)
    } else if (message.t === 'reload' && typeof message.hash === 'string') {
      locked = true
      showLock()
      if (typeof message.hash === 'string') baseHash = message.hash
      worker?.postMessage({ t: 'reload', hash: message.hash, revision: typeof message.revision === 'number' ? message.revision : revision })
      clearTimeout(reloadTimer)
      reloadTimer = setTimeout(() => { sendControl({ t: 'error', code: 'reload-timeout' }); locked = false; showLock() }, RELOAD_TIMEOUT_MS)
    } else if (message.t === 'resume') {
      // the model turn ended without a file change: unlock in place, keep the page's own state and undo
      clearTimeout(flushTimer)
      flushTimer = 0
      clearTimeout(reloadTimer)
      reloadTimer = 0
      locked = false
      showLock()
    } else if (message.t === 'read-only' && typeof message.readOnly === 'boolean') {
      setReadOnly(message.readOnly, message.reason)
    }
  }
  socket.onclose = () => { if (control === socket) control = null; retryControl() }
  socket.onerror = () => { try { socket.close() } catch { /* gone */ } }
}

function retryControl() {
  // A locked page still needs the socket: the reload that ends a model turn arrives on it. (The input lock only stops input.)
  if (stopped) return
  const wait = controlRetry
  controlRetry = Math.min(controlRetry * 2, 30000)
  setTimeout(openControl, wait)
}

// ---- lifecycle -----------------------------------------------------------------------------------------------------

window.addEventListener('message', (event) => {
  if (event.source !== parent || event.origin !== location.origin) return
  const message = event.data
  if (!message || message.t !== 'pcbz-wasm-host' || typeof message.kind !== 'string') return
  if (message.kind === 'visible') worker?.postMessage({ t: 'visible', visible: message.visible === true })
})
document.addEventListener('visibilitychange', () => worker?.postMessage({ t: 'visible', visible: document.visibilityState !== 'hidden' }))
window.addEventListener('pagehide', () => { stopped = true; try { worker?.terminate() } catch { /* gone */ }; try { control?.close() } catch { /* gone */ } })

if (typeof ResizeObserver === 'function') new ResizeObserver(resize).observe(stage)
window.addEventListener('resize', resize)
bindInput()
void start()
