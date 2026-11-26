// Serve the trunk-built editor and the harness page, then drive headless Chrome through one
// load/edit/save cycle and check the saved archive.
//
// Usage: node run.mjs --dist <dist/web> --project <file.pc> --out <dir> [--cli <pcbcraft-cli>]
//
// WebGPU is removed before any page script runs, so the editor takes its WebGL2 path; the
// renderer probe reports which context the canvas ended up with. The project loads through
// `pcbz_host_open`, one edit runs through `pcbz_host_command`, `pcbz_host_save` sends the
// archive to `/save.pc`, and `open_check.py` reopens it with the pinned CLI.

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';
import { PNG } from 'pngjs';

const HERE = fileURLToPath(new URL('.', import.meta.url));
const RENAMED = 'Sensor node harness';

function options() {
  const args = new Map();
  for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i].replace(/^--/, ''), process.argv[i + 1]);
  return {
    dist: resolve(args.get('dist') || 'upstream/dist/web'),
    project: resolve(args.get('project') || 'upstream/examples/sensor-node.pc'),
    out: resolve(args.get('out') || 'proof'),
    cli: args.get('cli') ? resolve(args.get('cli')) : null,
    harness: resolve(args.get('harness') || HERE),
  };
}

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.woff2': 'font/woff2',
  '.txt': 'text/plain; charset=utf-8',
};

function sha256(buffer) {
  return createHash('sha256').update(buffer).digest('hex');
}

function pixelStats(buffer) {
  const png = PNG.sync.read(buffer);
  const colors = new Set();
  let ink = 0;
  for (let i = 0; i < png.data.length; i += 4) {
    const r = png.data[i];
    const g = png.data[i + 1];
    const b = png.data[i + 2];
    if (Math.abs(r - 39) <= 4 && Math.abs(g - 43) <= 4 && Math.abs(b - 52) <= 4) continue;
    ink += 1;
    if (colors.size < 4096) colors.add(((r >> 3) << 10) | ((g >> 3) << 5) | (b >> 3));
  }
  return { width: png.width, height: png.height, ink, colors: colors.size };
}

function safeJoin(root, path) {
  const clean = decodeURIComponent(path).replace(/^\/+/, '');
  const target = join(root, clean);
  return target === root || target.startsWith(root + sep) ? target : null;
}

async function listFiles(dir, prefix = '') {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const full = join(dir, entry.name);
    const name = prefix ? prefix + '/' + entry.name : entry.name;
    if (entry.isDirectory()) files.push(...(await listFiles(full, name)));
    else files.push({ path: name, bytes: (await stat(full)).size });
  }
  return files.sort((a, b) => b.bytes - a.bytes);
}

async function pollUntil(page, predicate, arg, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    last = await page.evaluate(predicate, arg);
    if (last && last.error) throw new Error(`${label}: ${last.error}`);
    if (last && last.ok) return last.value;
    await page.waitForTimeout(250);
  }
  throw new Error(`${label} timed out; last=${JSON.stringify(last)}`);
}

async function waitForCanvas(page, file, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    const buffer = await page.screenshot();
    last = pixelStats(buffer);
    if (last.ink > 2000 && last.colors > 8) {
      await writeFile(file, buffer);
      return last;
    }
    await page.waitForTimeout(500);
  }
  throw new Error(`canvas stayed blank: ${JSON.stringify(last)}`);
}

async function main() {
  const { dist, project, out, cli, harness } = options();
  await mkdir(out, { recursive: true });

  const files = await readdir(dist);
  const appJs =
    files.find((name) => /^pcbcraft-web-[0-9a-f]+\.js$/.test(name)) ||
    files.find((name) => /^pcbcraft-web.*\.js$/.test(name) && !name.endsWith('_bg.js'));
  const wasm = files.find((name) => name.endsWith('_bg.wasm')) || files.find((name) => name.endsWith('.wasm'));
  if (!appJs) throw new Error(`no pcbcraft-web-*.js in ${dist}`);
  if (!wasm) throw new Error(`no wasm in ${dist}`);
  const inputBytes = await readFile(project);
  const inputSha = sha256(inputBytes);

  let savedSha = null;
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url, 'http://127.0.0.1');
      if (request.method === 'POST' && url.pathname === '/save.pc') {
        const chunks = [];
        for await (const chunk of request) chunks.push(chunk);
        const body = Buffer.concat(chunks);
        savedSha = sha256(body);
        await writeFile(join(out, 'saved.pc'), body);
        response.writeHead(200, { 'content-type': 'text/plain' }).end(savedSha);
        return;
      }
      if (url.pathname === '/' || url.pathname === '/index.html') {
        const html = await readFile(join(harness, 'index.html'), 'utf8');
        const body = html.replace('__APP_JS__', '/dist/web/' + appJs).replace('__WASM_URL__', '/dist/web/' + wasm);
        response.writeHead(200, { 'content-type': MIME['.html'] }).end(body);
        return;
      }
      if (url.pathname === '/host.js') {
        response.writeHead(200, { 'content-type': MIME['.js'] }).end(await readFile(join(harness, 'host.js')));
        return;
      }
      if (url.pathname === '/project.pc') {
        response.writeHead(200, { 'content-type': 'application/zip' }).end(inputBytes);
        return;
      }
      const relative = url.pathname.startsWith('/dist/web/') ? url.pathname.slice('/dist/web'.length) : url.pathname;
      const target = safeJoin(dist, relative);
      if (target) {
        try {
          const body = await readFile(target);
          response.writeHead(200, { 'content-type': MIME[extname(target)] || 'application/octet-stream' }).end(body);
          return;
        } catch {
          // fall through to 404
        }
      }
      response.writeHead(404).end('not found');
    } catch (error) {
      response.writeHead(500).end(String(error && error.stack ? error.stack : error));
    }
  });
  await new Promise((done) => server.listen(0, '127.0.0.1', done));
  const base = `http://127.0.0.1:${server.address().port}`;

  const launch = {
    headless: true,
    args: [
      '--no-sandbox',
      '--disable-dev-shm-usage',
      '--use-gl=angle',
      '--use-angle=swiftshader',
      '--enable-unsafe-swiftshader',
      '--ignore-gpu-blocklist',
      '--enable-webgl',
      '--disable-features=WebGPU',
    ],
  };
  const candidates = [process.env.CHROME_BIN, '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/opt/google/chrome/chrome', '/usr/bin/chromium'];
  const executable = candidates.find((path) => path && existsSync(path));
  const browser = executable
    ? await chromium.launch({ ...launch, executablePath: executable })
    : await chromium.launch({ ...launch, channel: 'chrome' });
  const consoleLog = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 });
  page.on('console', (message) => consoleLog.push(`${message.type()}: ${message.text()}`));
  page.on('pageerror', (error) => consoleLog.push(`pageerror: ${error.message}`));

  const metrics = { load_ms: null, init_ms: null, renderer: null, input: { bytes: inputBytes.length, sha256: inputSha }, saved: null, sizes: [], cli: null, console: [] };
  try {
    // Remove WebGPU before anything runs so the editor takes its WebGL2 fallback.
    await page.addInitScript(() => {
      try {
        Object.defineProperty(Navigator.prototype, 'gpu', { get: () => undefined, configurable: true });
      } catch {}
      try {
        Object.defineProperty(navigator, 'gpu', { value: undefined, configurable: true });
      } catch {}
    });
    await page.goto(base + '/', { waitUntil: 'load', timeout: 180000 });
    await pollUntil(page, () => {
      const h = window.__pcbzHarness;
      if (h.inited === 'error') return { ok: false, error: h.errors[0] || 'init failed' };
      return { ok: h.inited === true, value: h.initMs };
    }, null, 180000, 'wasm init');
    metrics.init_ms = await page.evaluate(() => window.__pcbzHarness.initMs);

    const loadStarted = Date.now();
    const loadedBytes = await page.evaluate(() => window.__pcbzLoad('/project.pc'));
    if (loadedBytes !== inputBytes.length) throw new Error(`loaded ${loadedBytes} bytes, served ${inputBytes.length}`);
    await pollUntil(page, () => {
      const h = window.__pcbzHarness;
      if (h.errors.length) return { ok: false, error: h.errors[0] };
      const text = window.__pcbzStatus();
      if (!text) return { ok: false };
      const status = JSON.parse(text);
      return { ok: status.home === false && status.project !== null && status.busy === false, value: status };
    }, null, 180000, 'project open');
    metrics.load_ms = Date.now() - loadStarted;
    await pollUntil(page, () => ({ ok: !document.getElementById('pcbcraft_loading') }), null, 30000, 'loading overlay');

    const beforeStats = await waitForCanvas(page, join(out, 'before.png'), 180000);
    metrics.renderer = await page.evaluate(() => {
      const canvas = document.getElementById('pcbcraft_canvas');
      return { navigator_gpu: typeof navigator.gpu, webgl2: !!canvas.getContext('webgl2') };
    });

    await page.evaluate((name) => window.__pcbzCommand('project.rename', { name }), RENAMED);
    await pollUntil(page, (name) => {
      const text = window.__pcbzStatus();
      const status = text ? JSON.parse(text) : {};
      return { ok: status.project === name && status.busy === false, value: status };
    }, RENAMED, 60000, 'rename');
    await page.waitForTimeout(500);

    await page.evaluate(() => window.__pcbzSave());
    const saved = await pollUntil(page, () => {
      const h = window.__pcbzHarness;
      if (h.errors.length) return { ok: false, error: h.errors[0] };
      return { ok: h.savedSha !== null && h.savedResponse !== null, value: { sha: h.savedSha, name: h.savedName, response: h.savedResponse, bytes: h.saved ? h.saved.length : null } };
    }, null, 180000, 'save');
    metrics.saved = { bytes: saved.bytes, sha256: saved.sha, name: saved.name, differs: saved.sha !== inputSha, server_sha256: savedSha };
    await writeFile(join(out, 'after.png'), await page.screenshot());

    if (saved.sha !== savedSha) throw new Error(`browser saw ${saved.sha}, server wrote ${savedSha}`);
    if (saved.sha === inputSha) throw new Error('saved archive is byte-identical to the input');

    const diffText = execFileSync('python3', [join(harness, 'archive_diff.py'), project, join(out, 'saved.pc')], { encoding: 'utf8' });
    await writeFile(join(out, 'save_diff.json'), diffText);
    metrics.save_diff = JSON.parse(diffText);

    metrics.sizes = await listFiles(dist);
    metrics.console = consoleLog.filter((line) => /warn|error|Gl|adapter|renderer/i.test(line)).slice(0, 80);

    if (cli) {
      metrics.cli = execFileSync('python3', [join(harness, 'open_check.py'), cli, join(out, 'saved.pc'), RENAMED], { encoding: 'utf8' }).trim();
    }
    metrics.canvas = beforeStats;
    await writeFile(join(out, 'metrics.json'), JSON.stringify(metrics, null, 2));
    console.log(JSON.stringify({ ok: true, load_ms: metrics.load_ms, init_ms: metrics.init_ms, renderer: metrics.renderer, saved: metrics.saved, cli: metrics.cli }, null, 2));
  } catch (error) {
    metrics.error = String(error && error.stack ? error.stack : error);
    metrics.console = consoleLog.slice(-80);
    await writeFile(join(out, 'metrics.json'), JSON.stringify(metrics, null, 2));
    throw error;
  } finally {
    await browser.close();
    await new Promise((done) => server.close(done));
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});