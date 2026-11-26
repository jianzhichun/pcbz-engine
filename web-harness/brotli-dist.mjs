// The wasm page ships brotli-only (design §1.4). Compress every .wasm beside itself as .wasm.br with
// zlib's brotli (quality 9), leave the identity file for the browser proof, and print the sizes.
//
// Usage: node brotli-dist.mjs --dist <dist/web>

import { readdir, readFile, stat, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { brotliCompressSync, constants } from 'node:zlib'

const args = new Map()
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i].replace(/^--/, ''), process.argv[i + 1])
const dist = resolve(args.get('dist') || 'upstream/dist/web')
const rows = []
for (const name of await readdir(dist)) {
  if (!name.endsWith('.wasm')) continue
  const file = join(dist, name)
  const raw = await readFile(file)
  const br = brotliCompressSync(raw, { params: { [constants.BROTLI_PARAM_QUALITY]: 9 } })
  await writeFile(file + '.br', br)
  rows.push({ name, raw: raw.length, brotli: br.length })
}
let total = 0
for (const name of await readdir(dist)) total += (await stat(join(dist, name))).size
console.log(JSON.stringify({ wasm: rows, distBytes: total }, null, 2))
