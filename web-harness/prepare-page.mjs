// Put the PCBZ page into trunk's dist/web and name the hashed build files for it.
//
// Usage: node prepare-page.mjs --dist <dist/web> --page <page dir>
//
// The page cannot rely on trunk's own index.html: it mounts the editor in a Worker and calls the
// `pcbz_worker_*` exports (patch 0012), and trunk renames the wasm to pcbcraft-web-<hash>_bg.wasm,
// so the worker must be told the module and wasm paths (`init({module_or_path})`). This copies the
// page's four files and its font, then writes pcbz-build.json; the page reads only that manifest.

import { cp, readdir, writeFile } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { join, resolve } from 'node:path'

const args = new Map()
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i].replace(/^--/, ''), process.argv[i + 1])
const dist = resolve(args.get('dist') || 'upstream/dist/web')
const page = resolve(args.get('page') || 'web-harness/page')
if (!existsSync(dist)) throw new Error(`no dist at ${dist}`)
if (!existsSync(join(page, 'index.html'))) throw new Error(`no page at ${page}`)

const files = await readdir(dist)
const app = files.find((name) => /^pcbcraft-web-[0-9a-f]+\.js$/.test(name))
  || files.find((name) => /^pcbcraft-web.*\.js$/.test(name) && !name.endsWith('_bg.js'))
const wasm = files.find((name) => name.endsWith('_bg.wasm')) || files.find((name) => name.endsWith('.wasm'))
if (!app) throw new Error(`no pcbcraft-web-*.js in ${dist}`)
if (!wasm) throw new Error(`no wasm in ${dist}`)

await cp(join(page, 'index.html'), join(dist, 'index.html'))
for (const name of ['pcbz-page.js', 'pcbz-worker.js', 'pcbz-host.js']) await cp(join(page, name), join(dist, name))
await cp(join(page, 'fonts'), join(dist, 'fonts'), { recursive: true })
await writeFile(join(dist, 'pcbz-build.json'), JSON.stringify({ app, wasm, font: 'fonts/NotoSansSC-GB2312.otf' }, null, 2) + '\n')
console.log(JSON.stringify({ app, wasm, font: 'fonts/NotoSansSC-GB2312.otf' }))
