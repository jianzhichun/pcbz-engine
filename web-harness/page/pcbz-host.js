// PCBZ wasm page: the host bridge *inside the Worker*. The upstream editor calls globalThis.pcbzHostPick instead of its
// file input and globalThis.pcbzOnSave instead of its anchor download (patch 0011); the export menus call
// globalThis.pcbzOnExport (patch 0012). The bytes never leave this Worker except through the plugin's routes:
//   GET  /pcbz/wasm/project?session=      the bound .pcbz (the page gets bytes, never a path)
//   POST /pcbz/wasm/save?session=         the archive; X-PCBZ-Base-Hash is the page's write base (design §2.2 [F2])
//   POST /pcbz/wasm/export?session=&name=&media=   one exported file, through the plugin's export rules
// One save in flight; a save requested while one runs keeps the newest bytes and runs after it, and a flush's ack is
// the answer of the last save (an older save's hash would tell the plugin less than the page holds). A 409 `busy` is
// retried in place; the plugin's 409 `reload` is handed to the page, which drops the refused bytes and reloads.

/** How many times a save the plugin calls "busy" is asked again, and how far apart (design §2.2: retry, do not reload). */
const SAVE_RETRY_MAX = 8
const SAVE_RETRY_MS = 1000

export const host = {
  urls: null, baseHash: null, revision: null, readOnly: false,
  pending: null, inFlight: false, queued: null, retry: null, post: () => {}, saveTimeoutMs: 8000,
}

export function installHost(options) {
  host.urls = options.urls
  host.baseHash = typeof options.baseHash === 'string' ? options.baseHash : null
  host.revision = typeof options.revision === 'number' ? options.revision : null
  host.readOnly = options.readOnly === true
  host.post = options.post
  host.saveTimeoutMs = typeof options.saveTimeoutMs === 'number' ? options.saveTimeoutMs : 8000
  host.pending = null
  host.inFlight = false
  host.queued = null
  host.retry = null

  globalThis.pcbzHostPick = async () => {
    const { buffer, hash } = await fetchProject()
    // the bytes the page now holds are its write base: the route's own sha256 of exactly those bytes (design §2.1/§2.2 [F2])
    if (hash !== null) host.baseHash = hash
    return buffer
  }
  globalThis.pcbzOnSave = (bytes, name) => {
    // `bytes` is a view into wasm memory; copy before the module may reuse it (the spike's host did the same).
    const copy = new Uint8Array(bytes)
    void save(copy, typeof name === 'string' ? name : '')
  }
  globalThis.pcbzOnExport = (bytes, name, media) => {
    void exportBytes(new Uint8Array(bytes), typeof name === 'string' ? name : '', typeof media === 'string' ? media : 'application/octet-stream')
  }
  globalThis.pcbzIsHost = true
}

/** Set the write base. An empty hash clears it: a base is never guessed, and a save with none is refused (not sent stale). */
export function setBase(hash) { if (typeof hash === 'string') host.baseHash = hash === '' ? null : hash }
export function setRevision(revision) { if (typeof revision === 'number') host.revision = revision }

/** The sha256 of an archive as lowercase hex, or null where the host has no SubtleCrypto. */
export async function sha256Hex(bytes) {
  try {
    const digest = await crypto.subtle.digest('SHA-256', bytes)
    return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('')
  } catch { return null }
}

/** The session's project bytes and the hash that is their write base. The project route sends `x-pcbz-sha256`; when a
 * host cannot carry it the digest is computed, so a webview without a secure context still gets a base. */
export async function fetchProject() {
  const response = await fetch(host.urls.project, { cache: 'no-store' })
  if (!response.ok) throw new Error(`project HTTP ${response.status}`)
  const buffer = await response.arrayBuffer()
  const header = String(response.headers && typeof response.headers.get === 'function' ? response.headers.get('x-pcbz-sha256') : '').toLowerCase()
  const hash = /^[0-9a-f]{64}$/.test(header) ? header : await sha256Hex(new Uint8Array(buffer))
  return { buffer, hash }
}

/** A flush from the control socket: the next save answers it (one write, not two). */
export function beginFlush(id) {
  settle(false, '上一次保存还没有结束。')
  host.pending = {
    id,
    timer: setTimeout(() => settle(false, '等待保存超时。'), host.saveTimeoutMs),
  }
}

/** A save that did not run (no project open): answer the flush without touching the file. */
export function ackFlush(ok, note) { settle(ok === true, note) }

/** Answer the pending flush, once. Nothing newer may be waiting: an older save's hash is not what the page holds. */
function finish(ok, note) {
  if (host.queued !== null) return
  settle(ok, note)
}

function settle(ok, note) {
  console.log('DBG settle', Math.round(performance.now()), ok, note || '')
  const pending = host.pending
  if (!pending) return
  host.pending = null
  clearTimeout(pending.timer)
  host.post({ t: 'flush-ack', id: pending.id, ok: ok === true, hash: host.baseHash, revision: host.revision, ...(note ? { note } : {}) })
}

function reasonOf(body, fallback) {
  if (body && typeof body.reason === 'string' && /[一-鿿]/.test(body.reason)) return body.reason.trim().slice(0, 300)
  return fallback
}

async function save(bytes, name, tries = 0) {
  console.log('DBG save begin', Math.round(performance.now()), bytes.length)
  if (host.readOnly) {
    host.post({ t: 'read-only', readOnly: true, reason: '当前授权不能修改这个工程。' })
    settle(false, '只读。')
    return
  }
  if (host.inFlight) {
    // one save at a time: keep the newest bytes; the save's own answer, or the queued one's, answers a waiting flush
    host.queued = { bytes, name }
    return
  }
  if (host.retry) { clearTimeout(host.retry.timer); host.retry = null }
  host.inFlight = true
  const headers = { 'content-type': 'application/octet-stream' }
  if (host.baseHash) headers['x-pcbz-base-hash'] = host.baseHash
  if (host.revision !== null) headers['x-pcbz-revision'] = String(host.revision)
  if (name) headers['x-pcbz-name'] = encodeURIComponent(name)
  try {
    const response = await fetch(host.urls.save, { method: 'POST', headers, body: bytes, cache: 'no-store' })
    const body = await response.json().catch(() => null)
    if (response.ok && body && body.saved === true) {
      if (typeof body.hash === 'string') host.baseHash = body.hash
      if (typeof body.revision === 'number') host.revision = body.revision
      host.post({ t: 'saved', ok: true, hash: host.baseHash, base: host.baseHash, ...(body.note ? { note: String(body.note) } : {}) })
      finish(true)
    } else if (response.status === 409 && body && body.code === 'busy') {
      // a live engine still owes its own change: retrying is safe, reloading would drop the page's edits
      const note = reasonOf(body, '工程正在保存，请稍后再试。')
      host.post({ t: 'saved', ok: false, note })
      if (host.queued === null) {
        if (tries >= SAVE_RETRY_MAX) settle(false, note)
        else {
          const timer = setTimeout(() => { host.retry = null; void save(bytes, name, tries + 1) }, SAVE_RETRY_MS)
          host.retry = { timer }
        }
      }
    } else if (response.status === 409 && body && body.code === 'read-only') {
      // another session holds the file: editing stops here; the picture stays and nothing on disk is touched
      host.readOnly = true
      host.queued = null
      const reason = reasonOf(body, '这个工程现在只读。')
      host.post({ t: 'read-only', readOnly: true, reason })
      settle(false, reason)
    } else if (response.status === 409) {
      host.queued = null
      host.post({ t: 'saved', ok: false, reload: true, note: reasonOf(body, '工程已被更新，正在重新载入。') })
      settle(false, '写基已过期。')
    } else if (response.status === 403) {
      host.readOnly = true
      host.queued = null
      const reason = reasonOf(body, '当前授权不能修改这个工程。')
      host.post({ t: 'read-only', readOnly: true, reason })
      settle(false, reason)
    } else {
      const note = reasonOf(body, `保存没有完成（HTTP ${response.status}）。`)
      host.post({ t: 'saved', ok: false, note })
      finish(false, note)
    }
  } catch {
    host.post({ t: 'saved', ok: false, note: '保存没有完成。' })
    finish(false, '保存没有完成。')
  } finally {
    host.inFlight = false
    const queued = host.queued
    host.queued = null
    if (queued !== null) void save(queued.bytes, queued.name, 0)
  }
}

async function exportBytes(bytes, name, media) {
  // A read-only page may still export (design §2.3: read-existing survives expiry); the export route asks its own mark,
  // and a refusal there is the notice.
  try {
    const url = new URL(host.urls.export)
    url.searchParams.set('name', name)
    url.searchParams.set('media', media)
    const response = await fetch(url.href, { method: 'POST', headers: { 'content-type': 'application/octet-stream' }, body: bytes, cache: 'no-store' })
    if (!response.ok) {
      const body = await response.json().catch(() => null)
      host.post({ t: 'log', message: reasonOf(body, `导出没有完成（HTTP ${response.status}）。`) })
    }
  } catch {
    host.post({ t: 'log', message: '导出没有完成。' })
  }
}