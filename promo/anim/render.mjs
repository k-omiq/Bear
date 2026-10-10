// Renders promo/anim/index.html to video, frame by frame, with headless Chrome and ffmpeg.
// Needs the page served over http (the "promo" entry in .claude/launch.json serves promo/ on 5288).
//
//   node render.mjs                        full video, 30 fps -> promo/out/manga-cleaner-promo.mp4
//   node render.mjs --fps 60               60 fps
//   node render.mjs --from 22 --to 41      a range only (default: the whole timeline)
//   node render.mjs --stills 3,14,48       PNG stills -> promo/out/stills/

import { spawn } from 'node:child_process'
import { mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'

const here = dirname(fileURLToPath(import.meta.url))
const outDir = join(here, '..', 'out')
const args = process.argv.slice(2)
const opt = (name, dflt) => { const i = args.indexOf(`--${name}`); return i < 0 ? dflt : args[i + 1] }
const fps = +opt('fps', 30)
const from = +opt('from', 0)
let to = opt('to', null)
const stills = opt('stills', null)
const base = opt('url', 'http://127.0.0.1:5288/anim/index.html')
const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const port = 9300 + Math.floor(Math.random() * 500)
const profile = join(tmpdir(), `promo-chrome-${port}`)

const browser = spawn(chrome, [
  '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  '--window-size=1920,1080', '--hide-scrollbars', '--force-device-scale-factor=1', '--no-first-run', 'about:blank',
], { stdio: 'ignore' })

async function wsUrl() {
  for (let i = 0; i < 400; i++) {
    try { const r = await fetch(`http://127.0.0.1:${port}/json/list`); const l = await r.json(); const p = l.find(t => t.type === 'page'); if (p) return p.webSocketDebuggerUrl } catch {}
    await new Promise(r => setTimeout(r, 100))
  }
  throw new Error('Chrome did not start')
}

const ws = new WebSocket(await wsUrl())
await new Promise(r => ws.addEventListener('open', r, { once: true }))
let id = 0
const pending = new Map()
ws.addEventListener('message', e => {
  const m = JSON.parse(e.data)
  if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); m.error ? rej(new Error(m.error.message)) : res(m.result) }
})
const send = (method, params = {}) => new Promise((res, rej) => { const i = ++id; pending.set(i, { res, rej }); ws.send(JSON.stringify({ id: i, method, params })) })
const evaluate = async expr => {
  const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result.value
}

await send('Page.enable')
await send('Emulation.setDeviceMetricsOverride', { width: 1920, height: 1080, deviceScaleFactor: 1, mobile: false })
await send('Page.navigate', { url: `${base}?render=1` })
for (let i = 0; i < 200; i++) { try { if (await evaluate('!!window.ready')) break } catch {} await new Promise(r => setTimeout(r, 100)) }
await evaluate('window.ready')
const total = await evaluate('window.duration()')
to = to === null ? total : +to
const full = from === 0 && to === total

const shot = async t => {
  await evaluate(`seek(${t})`)
  const r = await send('Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width: 1920, height: 1080, scale: 1 } })
  return Buffer.from(r.data, 'base64')
}

try {
  if (stills) {
    const dir = join(outDir, 'stills'); mkdirSync(dir, { recursive: true })
    for (const t of stills.split(',').map(Number)) { writeFileSync(join(dir, `t${String(t).padStart(6, '0')}.png`), await shot(t)); console.log('still', t) }
  } else {
    mkdirSync(outDir, { recursive: true })
    const name = full ? 'manga-cleaner-promo.mp4' : `promo-${from}-${to}.mp4`
    const out = join(outDir, name)
    const ff = spawn('ffmpeg', ['-loglevel', 'error', '-y', '-f', 'image2pipe', '-framerate', String(fps), '-i', '-',
      '-c:v', 'libx264', '-preset', 'slow', '-crf', '16', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', out], { stdio: ['pipe', 'inherit', 'inherit'] })
    const frames = Math.round((to - from) * fps)
    const started = Date.now()
    for (let f = 0; f < frames; f++) {
      const buf = await shot(from + f / fps)
      if (!ff.stdin.write(buf)) await new Promise(r => ff.stdin.once('drain', r))
      if (f % (fps * 5) === 0) console.log(`frame ${f}/${frames}  ${((Date.now() - started) / 1000).toFixed(0)} s`)
    }
    ff.stdin.end()
    await new Promise(r => ff.on('close', r))
    console.log('wrote', out)
  }
} finally {
  ws.close(); browser.kill()
  try { rmSync(profile, { recursive: true, force: true }) } catch {}
}
