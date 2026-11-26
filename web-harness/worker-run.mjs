// Drive the real PCBZ wasm page (dist/web/index.html + pcbz-page.js/pcbz-worker.js/pcbz-host.js) in
// headless Chrome against a fake host, and prove the red line: the editor runs in a Worker on a
// transferred OffscreenCanvas, never on the page's main thread.
//
// Usage: node worker-run.mjs --dist <dist/web> --project <file.pc> --out <dir> [--cli <pcbcraft-cli>]
//
// Checks: the page never imports the app or instantiates wasm (static grep + the Worker's frames);
// the Worker paints frames; the bundled CJK face draws Chinese text (the proof-only overlay, checked
// by pixels); a rename through the host command and a save through /pcbz/wasm/save carry the page's
// write base; the saved archive reopens in the pinned CLI under its Chinese name.

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { existsSync } from 'node:fs'
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises'
import { extname, join, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium } from 'playwright-core'
import { PNG } from 'pngjs'

const HERE = fileURLToPath(new URL('.', import.meta.url))
const RENAMED = '中文工程 电源板'
const SESSION = 'proof'
const STATIC_PREFIX = '/pcbz/wasm/static/'

const args = new Map()
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i].replace(/^--/, ''), process.argv[i + 1])
const dist = resolve(args.get('dist') || 'upstream/dist/web')
const project = resolve(args.get('project') || 'upstream/examples/sensor-node.pc')
const out = resolve(args.get('out') || 'proof')
const cli = args.get('cli') ? resolve(args.get('cli')) : null

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.wasm': 'application/wasm', '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.otf': 'font/otf', '.ttf': 'font/ttf', '.woff2': 'font/woff2',
  '.txt': 'text/plain; charset=utf-8',
}
const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex')

function safeJoin(root, path) {
  const clean = decodeURIComponent(path).replace(/^\/+/, '')
  const target = join(root, clean)
  return target === root || target.startsWith(root + sep) ? target : null
}

function pixelStats(buffer) {
  const png = PNG.sync.read(buffer)
  const counts = new Map()
  let top = 0
  for (let i = 0; i < png.data.length; i += 4) {
    const key = (png.data[i] >> 3 << 10) | (png.data[i + 1] >> 3 << 5) | (png.data[i + 2] >> 3)
    const count = (counts.get(key) || 0) + 1
    counts.set(key, count)
    if (count > top) top = count
  }
  return { width: png.width, height: png.height, ink: png.width * png.height - top, colors: counts.size }
}

/** White glyph pixels on the proof overlay's black plate at the top-left. */
function cjkStats(buffer) {
  const png = PNG.sync.read(buffer)
  let white = 0
  let black = 0
  const x1 = Math.min(png.width, 900)
  const y1 = Math.min(png.height, 110)
  for (let y = 0; y < y1; y++) {
    for (let x = 0; x < x1; x++) {
      const i = (y * png.width + x) * 4
      const r = png.data[i], g = png.data[i + 1], b = png.data[i + 2]
      if (r > 180 && g > 180 && b > 180) white += 1
      else if (r < 30 && g < 30 && b < 30) black += 1
    }
  }
  return { width: png.width, height: png.height, white, black }
}

async function listFiles(dir, prefix = '') {
  const files = []
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    const name = prefix ? `${prefix}/${entry.name}` : entry.name
    if (entry.isDirectory()) files.push(...await listFiles(full, name))
    else files.push({ path: name, bytes: (await stat(full)).size })
  }
  return files.sort((a, b) => b.bytes - a.bytes)
}

async function waitFor(page, predicate, arg, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs
  let last = null
  while (Date.now() < deadline) {
    last = await page.evaluate(predicate, arg)
    if (last && last.error) throw new Error(`${label}: ${last.error}`)
    if (last && last.ok) return last.value
    await page.waitForTimeout(250)
  }
  throw new Error(`${label} timed out; last=${JSON.stringify(last)}`)
}

/** The red line, checked on the built page: no app module, no wasm, no host export on the main thread. */
async function checkNoMainThreadWasm() {
  const html = await readFile(join(dist, 'index.html'), 'utf8')
  const pageJs = await readFile(join(dist, 'pcbz-page.js'), 'utf8')
  const workerJs = await readFile(join(dist, 'pcbz-worker.js'), 'utf8')
  const bad = (message) => { throw new Error(`main-thread wasm check: ${message}`) }
  for (const [name, text] of [['index.html', html], ['pcbz-page.js', pageJs], ['pcbz-worker.js', workerJs]]) {
    if (/importScripts/.test(text)) bad(`${name} uses importScripts`)
  }
  if (/pcbcraft-web-[0-9a-f]+/.test(html)) bad('index.html names the app module')
  if (/\binit\s*\(/.test(html)) bad('index.html calls init')
  if (/pcbz_host_/.test(html)) bad('index.html calls a host export')
  if (/WebAssembly/.test(pageJs)) bad('pcbz-page.js touches WebAssembly')
  if (/pcbcraft-web-[0-9a-f]+/.test(pageJs)) bad('pcbz-page.js names the app module')
  if (/\binit\s*\(/.test(pageJs)) bad('pcbz-page.js calls init')
  if (/\bimport\s*\(/.test(pageJs)) bad('pcbz-page.js imports a module dynamically')
  if (!/transferControlToOffscreen/.test(pageJs)) bad('pcbz-page.js never transfers the canvas')
  if (!/new Worker/.test(pageJs)) bad('pcbz-page.js never creates a Worker')
  if (!/module_or_path/.test(workerJs)) bad('pcbz-worker.js does not init the wasm with its hashed path')
  if (!/\bimport\s*\(/.test(workerJs)) bad('pcbz-worker.js does not import the app module')
  return { html_bytes: html.length, page_bytes: pageJs.length, worker_bytes: workerJs.length }
}

async function main() {
  await mkdir(out, { recursive: true })
  const inputBytes = await readFile(project)
  const inputSha = sha256(inputBytes)
  let saved = null
  let savedBytes = null
  let probe = null
  let staticHits = 0

  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1')
    const json = (status, body) => response.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' }).end(JSON.stringify(body))
    try {
      if (url.pathname === '/pcbz/wasm/state') {
        return json(200, { on: true, project: { name: 'sensor node', path: '/pcbz/wasm/project', revision: 1 }, readOnly: false, version: 'proof' })
      }
      if (url.pathname === '/pcbz/wasm/project') {
        const etag = '"1"'
        if (request.headers['if-none-match'] === etag) return response.writeHead(304, { etag, 'cache-control': 'no-store' }).end()
        return response.writeHead(200, { 'content-type': 'application/zip', etag, 'cache-control': 'no-store' }).end(inputBytes)
      }
      if (request.method === 'POST' && url.pathname === '/pcbz/wasm/save') {
        const chunks = []
        for await (const chunk of request) chunks.push(chunk)
        const bytes = Buffer.concat(chunks)
        const base = String(request.headers['x-pcbz-base-hash'] || '').trim().toLowerCase()
        if (base !== inputSha) {
          return json(409, { reason: '工程已经被更新，正在重新载入。', code: 'reload' })
        }
        const hash = sha256(bytes)
        savedBytes = bytes
        saved = {
          bytes: bytes.length, hash, base, base_matches: base === inputSha,
          name: decodeURIComponent(String(request.headers['x-pcbz-name'] || '')),
          revision: request.headers['x-pcbz-revision'] ?? null, differs: hash !== inputSha,
        }
        await writeFile(join(out, 'saved.pc'), bytes)
        return json(200, { saved: true, hash, revision: 2 })
      }
      if (request.method === 'POST' && url.pathname === '/pcbz/wasm/probe') {
        const chunks = []
        for await (const chunk of request) chunks.push(chunk)
        try { probe = JSON.parse(Buffer.concat(chunks).toString('utf8')) } catch { probe = null }
        response.writeHead(204, { 'cache-control': 'no-store' }).end()
        return
      }
      if (url.pathname.startsWith(STATIC_PREFIX)) {
        const target = safeJoin(dist, url.pathname.slice(STATIC_PREFIX.length))
        if (target) {
          try {
            const body = await readFile(target)
            staticHits += 1
            response.writeHead(200, { 'content-type': MIME[extname(target)] || 'application/octet-stream' }).end(body)
            return
          } catch { /* 404 below */ }
        }
      }
      response.writeHead(404, { 'content-type': 'text/plain' }).end('not found')
    } catch (error) {
      response.writeHead(500).end(String(error && error.stack ? error.stack : error))
    }
  })
  await new Promise((done) => server.listen(0, '127.0.0.1', done))
  const base = `http://127.0.0.1:${server.address().port}`

  const launch = {
    headless: true,
    args: [
      '--no-sandbox', '--disable-dev-shm-usage', '--use-gl=angle', '--use-angle=swiftshader',
      '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--enable-webgl', '--disable-features=WebGPU',
    ],
  }
  const candidates = [process.env.CHROME_BIN, '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/opt/google/chrome/chrome', '/usr/bin/chromium']
  const executable = candidates.find((path) => path && existsSync(path))
  const browser = executable ? await chromium.launch({ ...launch, executablePath: executable }) : await chromium.launch({ ...launch, channel: 'chrome' })
  const consoleLog = []
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 })
  page.on('console', (message) => consoleLog.push(`${message.type()}: ${message.text()}`))
  page.on('pageerror', (error) => consoleLog.push(`pageerror: ${error.message}`))

  const metrics = { static_hits: 0, main_thread: null, worker: null, frames: null, before: null, cjk: null, probe: null, saved: null, cli: null, sizes: [], console: [] }
  try {
    await page.addInitScript(() => {
      try { Object.defineProperty(Navigator.prototype, 'gpu', { get: () => undefined, configurable: true }) } catch {}
      try { Object.defineProperty(navigator, 'gpu', { value: undefined, configurable: true }) } catch {}
    })
    await page.goto(base + STATIC_PREFIX + 'index.html?session=' + SESSION + '&v=proof', { waitUntil: 'load', timeout: 180000 })
    const started = await waitFor(page, () => {
      const proof = window.__pcbzProof
      if (!proof) return { ok: false }
      return proof.ready && proof.frames > 0
        ? { ok: true, value: { frames: proof.frames, backend: proof.backend, font: proof.font } }
        : { ok: false, error: null }
    }, null, 180000, 'worker first frame')
    metrics.worker = started
    if (started.backend === 'none' || started.backend === 'unknown') throw new Error(`no graphics backend in the Worker: ${started.backend}`)
    if (started.font !== true) throw new Error('the bundled CJK font did not load in the Worker')
    // The first painted frame may be Home: the state route names a bound project, the worker opens it, and only then may
    // a host command (the rename below) run; a command queued while the file job is busy is retried, but a wrong order here
    // would silently fail the rename.
    await waitFor(page, async () => {
      const text = await window.__pcbzProof.status()
      const status = text ? JSON.parse(text) : {}
      return status.project !== null && status.project !== undefined && status.busy === false ? { ok: true, value: status } : { ok: false }
    }, null, 120000, 'project open in the Worker')

    const done = Date.now()
    const shots = []
    for (let i = 0; i < 5; i++) shots.push(await page.evaluate(() => performance.now()))
    metrics.main_thread = { samples: shots.length, max_round_trip_ms: Date.now() - done }
    if (metrics.main_thread.max_round_trip_ms > 1000) throw new Error('the main thread answered too slowly while the Worker painted')

    const snapshot = async (name) => {
      const b64 = await page.evaluate(() => window.__pcbzProof.snapshot())
      const buffer = Buffer.from(b64, 'base64')
      await writeFile(join(out, name), buffer)
      return buffer
    }
    const before = await snapshot('worker-before.png')
    metrics.before = pixelStats(before)
    if (metrics.before.ink < 2000 || metrics.before.colors < 8) throw new Error(`the Worker's frame is blank: ${JSON.stringify(metrics.before)}`)

    await page.evaluate(() => window.__pcbzProof.text('中文工程 电源网络 VCC'))
    await page.waitForTimeout(600)
    const cjk = await snapshot('worker-cjk.png')
    metrics.cjk = cjkStats(cjk)
    if (metrics.cjk.white < 80) throw new Error(`no Chinese glyph pixels: ${JSON.stringify(metrics.cjk)}`)
    if (metrics.cjk.black < 1500) throw new Error(`the proof plate is missing: ${JSON.stringify(metrics.cjk)}`)

    await page.evaluate((name) => window.__pcbzProof.command('project.rename', { name }), RENAMED)
    await waitFor(page, async (name) => {
      const text = await window.__pcbzProof.status()
      const status = text ? JSON.parse(text) : {}
      return status.project === name && status.busy === false ? { ok: true, value: status } : { ok: false }
    }, RENAMED, 60000, 'rename through the host command')

    const ack = await page.evaluate(() => window.__pcbzProof.save())
    if (!ack || ack.ok !== true) throw new Error(`the flush/save was refused: ${JSON.stringify(ack)}`)
    if (saved === null || savedBytes === null) throw new Error('the save route received no archive')
    metrics.saved = saved
    if (!saved.base_matches) throw new Error('the page did not send the sha256 of the bytes it loaded as its write base')

    if (cli) {
      metrics.cli = JSON.parse(execFileSync('python3', [join(HERE, 'open_check.py'), cli, join(out, 'saved.pc'), RENAMED], { encoding: 'utf8' }).trim())
      if (metrics.cli.ok !== true) throw new Error('the CLI did not reopen the saved archive')
    }

    metrics.main_thread_check = await checkNoMainThreadWasm()
    metrics.probe = probe
    if (probe === null) throw new Error('the page sent no runtime probe')
    for (const field of ['worker', 'webgl2', 'offscreen', 'ime', 'cjk']) {
      if (typeof probe[field] !== 'boolean') throw new Error(`the runtime probe lacks ${field}`)
    }
    if (probe.cjk !== true) throw new Error('the runtime probe says CJK did not load')
    metrics.frames = await page.evaluate(() => window.__pcbzProof.frames)
    metrics.static_hits = staticHits
    metrics.sizes = await listFiles(dist)
    metrics.console = consoleLog.filter((line) => /error|warn|panic|unreachable|failed/i.test(line)).slice(0, 80)
    await writeFile(join(out, 'worker-metrics.json'), JSON.stringify(metrics, null, 2))
    console.log(JSON.stringify({ ok: true, backend: metrics.worker.backend, frames: metrics.frames, cjk: metrics.cjk, saved: metrics.saved, cli: metrics.cli }, null, 2))
  } catch (error) {
    metrics.error = String(error && error.stack ? error.stack : error)
    metrics.console = consoleLog.slice(-80)
    try { metrics.banner = await page.evaluate(() => document.getElementById('pcbz-banner')?.textContent ?? '') } catch {}
    await writeFile(join(out, 'worker-metrics.json'), JSON.stringify(metrics, null, 2))
    throw error
  } finally {
    await browser.close()
    await new Promise((done) => server.close(done))
  }
}

main().catch((error) => {
  console.error(error)
  process.exit(1)
})
