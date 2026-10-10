// Records Manga Cleaner app-usage clips headlessly: the app's real Svelte UI in
// headless Chrome, against its browser mock backend patched with `?promo=1`
// (scratch copy only) so pages, detections and cleaned pixels are the real
// pipeline's (promo/cleaned). Frames come from Page.startScreencast and are
// encoded at their real timestamps.
//
//   node record.mjs                     every clip
//   node record.mjs 02-detect 03-clean  some clips
//   node record.mjs --base http://localhost:5311
//
// Needs the scratch mc-ui Vite server running (see promo/app/README in the report).

import { spawnSync } from 'node:child_process'
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'
import { connect, launchChrome } from './cdp.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const args = process.argv.slice(2)
const opt = (name, dflt) => { const i = args.indexOf(`--${name}`); return i < 0 ? dflt : args[i + 1] }
const BASE = opt('base', 'http://localhost:5311')
const ONLY = args.filter((a, i) => !a.startsWith('--') && !(i > 0 && args[i - 1].startsWith('--')))
const CLIPS_DIR = join(here, 'clips')
const STILLS_DIR = join(here, 'stills')
const W = 1440, H = 900, DSF = 2
mkdirSync(CLIPS_DIR, { recursive: true })
mkdirSync(STILLS_DIR, { recursive: true })

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/* ---------------- page-side overlay: cursor, ripple, keycap ---------------- */

const OVERLAY = String.raw`(() => {
  if (window.__promo) return
  const css = '#__pc{position:fixed;left:0;top:0;width:30px;height:30px;z-index:2147483647;pointer-events:none;will-change:transform;filter:drop-shadow(0 3px 5px rgba(0,0,0,.28)) drop-shadow(0 1px 1px rgba(0,0,0,.25));transition:opacity .25s}' +
    '.__rp{position:fixed;z-index:2147483646;pointer-events:none;width:14px;height:14px;margin:-7px 0 0 -7px;border-radius:50%;border:2.5px solid rgba(56,152,236,.95);background:rgba(56,152,236,.18);animation:__rp .55s cubic-bezier(.2,.7,.3,1) forwards}' +
    '@keyframes __rp{from{transform:scale(.4);opacity:1}to{transform:scale(3.4);opacity:0}}' +
    '#__kc{position:fixed;left:50%;bottom:92px;transform:translate(-50%,12px) scale(.92);z-index:2147483647;pointer-events:none;opacity:0;transition:opacity .18s ease,transform .22s cubic-bezier(.3,1.4,.5,1);display:flex;gap:10px;align-items:center;padding:10px 18px 10px 12px;border-radius:16px;background:rgba(28,30,36,.88);color:#fff;font:600 17px/1 Inter,-apple-system,system-ui,sans-serif;box-shadow:0 8px 24px rgba(0,0,0,.25)}' +
    '#__kc.on{opacity:1;transform:translate(-50%,0) scale(1)}' +
    '#__kc b{display:inline-flex;align-items:center;justify-content:center;min-width:34px;height:34px;padding:0 9px;border-radius:9px;background:#fff;color:#1c1e24;font:700 17px/1 Inter,-apple-system,system-ui,sans-serif;box-shadow:inset 0 -3px 0 rgba(0,0,0,.14)}' +
    '#__kc.down b{transform:translateY(2px);box-shadow:inset 0 -1px 0 rgba(0,0,0,.14)}'
  const svg = '<svg viewBox="0 0 30 30" width="30" height="30"><path d="M5.5 3.2c0-1 1.1-1.5 1.9-.9l17.2 13.1c.9.7.4 2.1-.7 2.1h-7.4c-.5 0-1 .2-1.3.6l-5.3 6.6c-.7.8-2 .4-2.1-.7z" fill="#fff" stroke="#1c1e24" stroke-width="1.9" stroke-linejoin="round"/></svg>'
  let x = innerWidth * 0.62, y = innerHeight * 0.72
  let el, kc, hideTimer
  const mount = () => {
    if (el && el.isConnected) return
    const style = document.createElement('style'); style.textContent = css; document.documentElement.appendChild(style)
    el = document.createElement('div'); el.id = '__pc'; el.innerHTML = svg; document.documentElement.appendChild(el)
    kc = document.createElement('div'); kc.id = '__kc'; document.documentElement.appendChild(kc)
    place()
  }
  const place = () => { if (el) el.style.transform = 'translate(' + (x - 5) + 'px,' + (y - 2) + 'px)' }
  const ease = (t) => t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2
  window.__promo = {
    mount,
    where: () => ({ x, y }),
    set(nx, ny) { mount(); x = nx; y = ny; place() },
    move(nx, ny, ms) {
      mount()
      const x0 = x, y0 = y, t0 = performance.now()
      const dist = Math.hypot(nx - x0, ny - y0)
      const dur = ms ?? Math.max(240, Math.min(700, 200 + dist * 0.5))
      // a slight arc, like a hand
      const bend = Math.min(60, dist * 0.12)
      const nxv = -(ny - y0) / (dist || 1), nyv = (nx - x0) / (dist || 1)
      return new Promise((res) => {
        const step = (now) => {
          const t = Math.min(1, (now - t0) / dur), e = ease(t), arc = Math.sin(Math.PI * e) * bend
          x = x0 + (nx - x0) * e + nxv * arc; y = y0 + (ny - y0) * e + nyv * arc; place()
          t < 1 ? requestAnimationFrame(step) : res()
        }
        requestAnimationFrame(step)
      })
    },
    ripple(px, py) {
      mount()
      const r = document.createElement('div'); r.className = '__rp'; r.style.left = px + 'px'; r.style.top = py + 'px'
      document.documentElement.appendChild(r); setTimeout(() => r.remove(), 700)
      el.animate([{ scale: 1 }, { scale: .86 }, { scale: 1 }], { duration: 220 })
    },
    key(label, sub, ms) {
      mount(); clearTimeout(hideTimer)
      kc.innerHTML = '<b>' + label + '</b>' + (sub ? '<span>' + sub + '</span>' : '')
      kc.classList.add('on')
      if (ms) hideTimer = setTimeout(() => kc.classList.remove('on', 'down'), ms)
    },
    keyDown(d) { kc && kc.classList.toggle('down', d) },
    keyHide() { kc && kc.classList.remove('on', 'down') },
    hide(h) { mount(); el.style.opacity = h ? '0' : '1' },
    // Stand-in for the native <select> popup, which headless Chrome does not
    // paint: the option texts are read from the real element.
    menu(r, items, sel) {
      mount(); this.menuClose()
      const m = document.createElement('div'); m.id = '__menu'
      m.style.cssText = 'position:fixed;z-index:2147483645;pointer-events:none;padding:5px;border-radius:8px;background:rgba(246,246,246,.97);box-shadow:0 0 0 .5px rgba(0,0,0,.22),0 10px 32px rgba(0,0,0,.22);font:13px/1 -apple-system,BlinkMacSystemFont,Inter,system-ui,sans-serif;color:#1d1d1f;opacity:0;transform:scale(.98);transition:opacity .12s,transform .12s'
      const rowH = 22
      items.forEach((t, i) => {
        const row = document.createElement('div')
        row.style.cssText = 'height:' + rowH + 'px;display:flex;align-items:center;gap:6px;padding:0 14px 0 6px;border-radius:4px;white-space:nowrap'
        row.innerHTML = '<span style="width:14px;text-align:center;font-size:12px">' + (i === sel ? '✓' : '') + '</span><span></span>'
        row.lastChild.textContent = t
        m.appendChild(row)
      })
      document.documentElement.appendChild(m)
      const w = Math.max(r.w, m.getBoundingClientRect().width)
      m.style.width = w + 'px'
      let top = r.y + r.h / 2 - 5 - sel * rowH - rowH / 2
      top = Math.max(8, Math.min(innerHeight - m.getBoundingClientRect().height - 8, top))
      m.style.left = (r.x - 6) + 'px'; m.style.top = top + 'px'
      requestAnimationFrame(() => { m.style.opacity = '1'; m.style.transform = 'none' })
      return [...m.children].map((row) => { const b = row.getBoundingClientRect(); return { x: b.left + Math.min(90, b.width / 2), y: b.top + b.height / 2 } })
    },
    menuHover(i) {
      const m = document.getElementById('__menu'); if (!m) return
      ;[...m.children].forEach((row, j) => { row.style.background = j === i ? '#1b6ef3' : 'transparent'; row.style.color = j === i ? '#fff' : '#1d1d1f' })
    },
    menuClose() { document.getElementById('__menu')?.remove() },
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount); else mount()
})()`

// Seeds a clean session before the app reads it: setup already offered,
// light theme, and the Text cleanup window to the right of the page.
const seedSession = (extra = {}) => `(() => {
  try {
    localStorage.clear()
    localStorage.setItem('mangaCleaner.session.v1', JSON.stringify(${JSON.stringify({
      firstLaunchOffered: true,
      theme: 'light',
      windows: {
        pages: { x: 16, y: 62, w: 248, h: 288, open: true, fold: false },
        layers: { x: 16, y: 362, w: 272, h: 470, open: true, fold: false },
        tool: { x: 990, y: 62, w: 384, h: null, open: true, fold: false },
      },
      ...extra,
    })}))
  } catch {}
})()`

/* ---------------- recorder ---------------- */

async function newSession(port) {
  const c = await connect(port)
  await c.send('Page.enable')
  await c.send('Runtime.enable')
  await c.send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: DSF, mobile: false })
  const errors = []
  c.on('Runtime.exceptionThrown', (p) => errors.push(p.exceptionDetails.exception?.description ?? p.exceptionDetails.text))
  c.on('Runtime.consoleAPICalled', (p) => { if (p.type === 'error') errors.push(p.args.map((a) => a.value ?? a.description).join(' ')) })
  c.errors = errors
  return c
}

function driver(c) {
  const ev = async (expr) => {
    // replMode serialises a returned promise instead of awaiting it; await it here.
    const r = await c.send('Runtime.evaluate', { expression: `await (${expr})`, awaitPromise: true, returnByValue: true, replMode: true })
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text)
    return r.result.value
  }
  // Finds a visible element and answers its centre. `q` is either
  // {text} (button/menuitem/option whose accessible name or text starts with it),
  // {sel} (CSS selector), or {js} (an expression answering an element).
  const locate = async (q) => ev(`(() => {
    const q = ${JSON.stringify(q)}
    const vis = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden' }
    const name = (e) => (e.getAttribute('aria-label') || e.innerText || e.value || '').replace(/\\s+/g, ' ').trim()
    let el = null
    if (q.js) el = (0, eval)(q.js)
    else if (q.sel) el = [...document.querySelectorAll(q.sel)].filter(vis)[q.nth ?? 0]
    else {
      const pool = [...document.querySelectorAll(q.within ? q.within + ' ' + (q.tags ?? 'button,[role=menuitem],[role=menuitemradio],[role=option],[role=tab],a,summary,label') : (q.tags ?? 'button,[role=menuitem],[role=menuitemradio],[role=option],[role=tab],a,summary,label'))].filter(vis)
      const hits = pool.filter((e) => q.exact ? name(e) === q.text : name(e).startsWith(q.text))
      el = hits[q.nth ?? 0]
    }
    if (!el) return null
    if (q.scroll !== false) el.scrollIntoView({ block: 'nearest', inline: 'nearest' })
    const r = el.getBoundingClientRect()
    return { x: r.left + r.width * (q.fx ?? 0.5), y: r.top + r.height * (q.fy ?? 0.5), w: r.width, h: r.height }
  })()`)
  const waitFor = async (expr, timeout = 8000, label = expr) => {
    const t0 = Date.now()
    while (Date.now() - t0 < timeout) {
      try { if (await ev(expr)) return true } catch {}
      await sleep(60)
    }
    throw new Error(`timed out waiting for ${label}`)
  }
  const find = async (q, timeout = 6000) => {
    const t0 = Date.now()
    while (Date.now() - t0 < timeout) {
      const at = await locate(q)
      if (at) return at
      await sleep(60)
    }
    throw new Error(`no element for ${JSON.stringify(q)}`)
  }
  const mouse = (type, x, y, extra = {}) =>
    c.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1, pointerType: 'mouse', ...extra })
  const moveTo = async (x, y, ms) => {
    await ev(`window.__promo.move(${x}, ${y}, ${ms ?? 'undefined'})`)
    await mouse('mouseMoved', x, y, { button: 'none' })
  }
  const click = async (q, { pause = 160, after = 250 } = {}) => {
    const at = await find(q)
    await moveTo(at.x, at.y)
    await sleep(pause)
    await ev(`window.__promo.ripple(${at.x}, ${at.y})`)
    await mouse('mousePressed', at.x, at.y)
    await sleep(70)
    await mouse('mouseReleased', at.x, at.y)
    await sleep(after)
    return at
  }
  const hover = async (q) => { const at = await find(q); await moveTo(at.x, at.y); return at }
  const KEYS = { m: 77, o: 79, Escape: 27 }
  const keyEvent = (type, key) => c.send('Input.dispatchKeyEvent', {
    type, key, code: key.length === 1 ? `Key${key.toUpperCase()}` : key,
    windowsVirtualKeyCode: KEYS[key] ?? key.toUpperCase().charCodeAt(0),
    nativeVirtualKeyCode: KEYS[key] ?? key.toUpperCase().charCodeAt(0),
    ...(type === 'keyDown' && key.length === 1 ? { text: key, unmodifiedText: key } : {}),
  })
  const press = async (key, label = key.toUpperCase(), sub = '') => {
    await ev(`window.__promo.key(${JSON.stringify(label)}, ${JSON.stringify(sub)}, 1100)`)
    await sleep(180)
    await ev('window.__promo.keyDown(true)')
    await keyEvent('keyDown', key)
    await sleep(90)
    await keyEvent('keyUp', key)
    await ev('window.__promo.keyDown(false)')
  }
  const hold = async (key, ms, label, sub = '') => {
    await ev(`window.__promo.key(${JSON.stringify(label)}, ${JSON.stringify(sub)})`)
    await sleep(200)
    await ev('window.__promo.keyDown(true)')
    await keyEvent('keyDown', key)
    await sleep(ms)
    await keyEvent('keyUp', key)
    await ev('window.__promo.keyDown(false)')
    await sleep(250)
    await ev('window.__promo.keyHide()')
  }
  const wheel = async (x, y, dy, steps = 1, gap = 16) => {
    for (let i = 0; i < steps; i++) {
      await c.send('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: 0, deltaY: dy, pointerType: 'mouse' })
      await sleep(gap)
    }
  }
  // Clicks a native <select> and shows its real options in the stand-in menu.
  const openSelect = async (q) => {
    const at = await find(q)
    await moveTo(at.x, at.y)
    await sleep(160)
    await ev(`window.__promo.ripple(${at.x}, ${at.y})`)
    const info = await ev(`(() => {
      const s = ${q.js}
      s.focus()
      const r = s.getBoundingClientRect()
      return { r: { x: r.left, y: r.top, w: r.width, h: r.height }, items: [...s.options].map((o) => o.text), sel: s.selectedIndex }
    })()`)
    await sleep(90)
    const rows = await ev(`window.__promo.menu(${JSON.stringify(info.r)}, ${JSON.stringify(info.items)}, ${info.sel})`)
    return { ...info, rows }
  }
  const menuTo = async (menu, i, ms) => {
    await moveTo(menu.rows[i].x, menu.rows[i].y, ms)
    await ev(`window.__promo.menuHover(${i})`)
  }
  // What choosing an entry in the native popup does: the value moves and the
  // select fires change, which the app handles as a real pick.
  const choose = async (q, menu, i) => {
    await ev(`window.__promo.ripple(${menu.rows[i].x}, ${menu.rows[i].y})`)
    await sleep(120)
    await ev(`window.__promo.menuClose()`)
    await ev(`(() => { const s = ${q.js}; s.selectedIndex = ${i}; s.dispatchEvent(new Event('input', { bubbles: true })); s.dispatchEvent(new Event('change', { bubbles: true })) })()`)
  }
  return { ev, locate, find, waitFor, click, hover, moveTo, press, hold, wheel, mouse, openSelect, menuTo, choose, key: keyEvent }
}

async function record(port, clip) {
  const name = clip.name
  const c = await newSession(port)
  const d = driver(c)
  const frameDir = join(tmpdir(), `promo-frames-${name}-${process.pid}`)
  rmSync(frameDir, { recursive: true, force: true })
  mkdirSync(frameDir, { recursive: true })

  // Fresh storage for this origin, then the seed and the overlay on every document.
  await c.send('Storage.clearDataForOrigin', { origin: BASE, storageTypes: 'all' })
  const scripts = []
  for (const source of [seedSession(clip.session ?? {}), OVERLAY]) {
    scripts.push((await c.send('Page.addScriptToEvaluateOnNewDocument', { source })).identifier)
  }
  await c.send('Page.navigate', { url: 'about:blank' })
  await sleep(200)
  await c.send('Page.navigate', { url: `${BASE}/${clip.query ?? '?promo=1'}` })
  await d.waitFor('!!document.querySelector("#app, body") && document.body.innerText.length > 20', 15000, 'app mount')
  await sleep(600)
  await d.ev('window.__promo.mount()')

  // Setup that should not be on camera.
  if (clip.prepare) await clip.prepare(d, c)
  if (clip.cursorAt) await d.ev(`window.__promo.set(${clip.cursorAt[0]}, ${clip.cursorAt[1]})`)
  await sleep(500)

  const frames = []
  let writing = Promise.resolve()
  c.on('Page.screencastFrame', (p) => {
    c.send('Page.screencastFrameAck', { sessionId: p.sessionId }).catch(() => {})
    const file = join(frameDir, `f${String(frames.length).padStart(5, '0')}.png`)
    frames.push({ file, t: p.metadata.timestamp })
    const buf = Buffer.from(p.data, 'base64')
    writing = writing.then(() => writeFileSync(file, buf))
  })
  await c.send('Page.startScreencast', { format: 'png', everyNthFrame: 1, maxWidth: W * DSF, maxHeight: H * DSF })
  // Nudge a repaint so the first frame arrives at once.
  await d.ev('window.__promo.set(window.__promo.where().x, window.__promo.where().y)')
  const t0 = Date.now() / 1000
  const stills = []
  const still = async (label) => {
    const r = await c.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
    const file = join(STILLS_DIR, `${name}${label ? '-' + label : ''}.png`)
    writeFileSync(file, Buffer.from(r.data, 'base64'))
    stills.push(file)
  }
  let failure = null
  try {
    await sleep(650)
    await clip.run(d, { still, c })
    await sleep(700)
  } catch (error) {
    failure = error
  }
  const tEnd = Date.now() / 1000
  await c.send('Page.stopScreencast')
  await writing
  for (const id of scripts) await c.send('Page.removeScriptToEvaluateOnNewDocument', { identifier: id }).catch(() => {})
  const errors = [...c.errors]
  c.close()

  if (frames.length < 2) throw new Error(`${name}: only ${frames.length} frames captured`)
  // Hold each frame until the next one's timestamp; the last until the end.
  const start = frames[0].t
  const lines = []
  frames.forEach((f, i) => {
    const next = i + 1 < frames.length ? frames[i + 1].t : Math.max(f.t + 1 / 30, f.t + (tEnd - t0) - (f.t - start))
    lines.push(`file '${f.file}'`, `duration ${Math.max(0.001, next - f.t).toFixed(4)}`)
  })
  lines.push(`file '${frames.at(-1).file}'`)
  const list = join(frameDir, 'list.txt')
  writeFileSync(list, lines.join('\n') + '\n')
  const out = join(CLIPS_DIR, `${name}.mp4`)
  const ff = spawnSync('ffmpeg', ['-loglevel', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', list,
    '-vf', `scale=${W * DSF}:${H * DSF}:flags=lanczos,fps=30,format=yuv420p`, '-fps_mode', 'cfr', '-r', '30',
    '-c:v', 'libx264', '-preset', 'slow', '-crf', '16', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', out], { encoding: 'utf8' })
  if (ff.status !== 0) throw new Error(`ffmpeg failed for ${name}: ${ff.stderr}`)
  rmSync(frameDir, { recursive: true, force: true })
  const probe = spawnSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration:stream=width,height,nb_frames', '-of', 'csv=p=0', out], { encoding: 'utf8' })
  return { out, frames: frames.length, seconds: (frames.at(-1).t - start).toFixed(1), probe: probe.stdout.trim().replace(/\n/g, ' '), stills, errors, failure }
}

/* ---------------- clips ---------------- */

function clickByText(t) {
  const b = [...document.querySelectorAll('button,[role=menuitem],[role=option]')]
    .filter((e) => e.getClientRects().length)
    .find((e) => (e.getAttribute('aria-label') || e.innerText || '').replace(/\s+/g, ' ').trim().startsWith(t))
  if (!b) throw new Error('no ' + t)
  b.click()
}
const byText = (t) => `(${clickByText.toString()})(${JSON.stringify(t)})`
const chapterRow = (name) => `[...document.querySelectorAll('li button')].find(b => b.innerText.includes(${JSON.stringify(name)}) && !b.getAttribute('aria-label'))`
const tilesReady = `[...document.querySelectorAll('[data-artwork="source"] img')].length > 0 && [...document.querySelectorAll('[data-artwork="source"] img')].every(i => i.complete && i.naturalWidth > 0)`
const pagesText = `(document.querySelector('[aria-label="Close Pages"]')?.closest('section,div[class*=window],aside')?.innerText ?? '')`
const pagesHeader = (re) => `/${re}/.test([...document.querySelectorAll('*')].find(e => e.childElementCount === 0 && /of \\d+ cleaned$/.test(e.textContent.trim()) && e.closest('[class]') && e.getClientRects().length && !/pages cleaned/.test(e.textContent))?.textContent ?? '')`
const layerRow = (engine, nth = 0) => `[...document.querySelectorAll('button.summary')].filter(b => b.getClientRects().length && b.innerText.includes(${JSON.stringify(engine)}))[${nth}]`
const expandedSelect = `[...document.querySelectorAll('select')].find(s => s.getClientRects().length && s.closest('.pick-control, [class*=pick]'))`

// Off camera: straight into a chapter, on `page`.
const quietOpen = async (d, { project = 'Give My Regards', chapter = 'Volume 2', page } = {}) => {
  await d.ev(byText(project))
  await d.waitFor(`!!${chapterRow(chapter)}`, 8000, 'chapter row')
  await d.ev(`${chapterRow(chapter)}.click()`)
  await d.waitFor(tilesReady, 10000, 'page tiles')
  if (page) {
    await d.ev(byText(page))
    await sleep(300)
    await d.waitFor(tilesReady, 10000, 'page tiles')
  }
  await sleep(600)
}
// Off camera: clean the current page with the real recorded results.
const quietCleanPage = async (d) => {
  await d.ev(byText('Clean anyway'))
  await d.ev(byText('Detect & clean page'))
  await d.waitFor(`/^\\d+ masks?$/.test([...document.querySelectorAll('*')].find(e => e.childElementCount === 0 && /\\bmasks?$/.test(e.textContent.trim()) && e.getClientRects().length)?.textContent.trim() ?? '') && !document.body.innerText.includes('Jobs: 1 running')`, 20000, 'page cleaned')
  await sleep(900)
}
const layersDrawn = `document.querySelectorAll('[data-artwork="cleaned"] canvas[data-layer]').length > 0 && [...document.querySelectorAll('[data-artwork="cleaned"] canvas[data-layer]')].every(c => c.width > 0 || c.style.display === 'none')`

const CLIPS = [
  {
    name: '01-library',
    cursorAt: [980, 700],
    async run(d, { still }) {
      await sleep(600)
      await d.hover({ text: 'Give My Regards' })
      await sleep(550)
      await still()
      await d.click({ text: 'Give My Regards' })
      await d.waitFor(`!!${chapterRow('Volume 2')}`, 8000, 'chapter row')
      await sleep(900)
      await d.click({ js: chapterRow('Volume 2'), fx: 0.2 })
      await d.waitFor(tilesReady, 10000, 'page tiles')
      await sleep(2000)
    },
  },
  {
    name: '02-detect',
    cursorAt: [1180, 600],
    prepare: (d) => quietOpen(d, { page: 'p. 02' }),
    async run(d, { still }) {
      await sleep(400)
      await d.click({ text: 'Detect', exact: true })
      await sleep(350)
      await d.click({ text: 'Detect page' })
      await d.waitFor(`document.querySelectorAll('button.summary').length >= 7`, 10000, 'detections in Layers')
      await d.moveTo(1180, 640, 700)
      await sleep(1200)
      await still('detected')
      // Hovering a detection lights its mask.
      await d.hover({ sel: 'button.region', nth: 3 })
      await sleep(1600)
      await d.moveTo(1180, 700, 700)
      await sleep(500)
    },
  },
  {
    name: '03-clean',
    cursorAt: [1180, 620],
    prepare: (d) => quietOpen(d),
    async run(d, { still }) {
      await d.click({ text: 'Chapter', exact: true }, { after: 120 })
      await d.click({ text: 'Clean anyway' }, { after: 120 })
      await d.click({ text: 'Detect & clean chapter' }, { after: 100 })
      await d.moveTo(1180, 700, 800)
      await d.waitFor(`document.body.innerText.includes('4 of 7 cleaned')`, 30000, 'run finished')
      await sleep(400)
      await still('cleaned')
      await d.press('m', 'M', 'Show every outline')
      await sleep(1100)
      await still('outlines')
      await d.hold('o', 1300, 'O', 'Hold to show the original')
    },
  },
  {
    name: '04-layers',
    query: '?promo=1&cloudGpu=ready',
    session: { cloudAllowed: true },
    cursorAt: [700, 560],
    async prepare(d) {
      await quietOpen(d)
      await quietCleanPage(d)
      await d.ev(byText('Close Text cleanup'))
      await sleep(400)
      await d.waitFor(layersDrawn, 10000, 'layers drawn')
    },
    async run(d, { still }) {
      await sleep(300)
      await d.click({ js: layerRow('LaMa Manga', 0), fx: 0.45 })
      await d.waitFor(`!!${expandedSelect}`, 5000, 'Clean with picker')
      await sleep(700)
      const q = { js: expandedSelect }
      const menu = await d.openSelect(q)
      await sleep(500)
      for (let i = 0; i < menu.items.length; i++) { await d.menuTo(menu, i, 420); await sleep(380) }
      await still('picker')
      await sleep(700)
      await d.ev('window.__promo.menuClose()')
      await d.key('keyDown', 'Escape'); await d.key('keyUp', 'Escape')
      await d.moveTo(760, 600, 700)
      await sleep(800)
    },
  },
  {
    name: '05-colour',
    query: '?promo=1&cloudGpu=ready',
    session: { cloudAllowed: true },
    cursorAt: [1180, 620],
    prepare: (d) => quietOpen(d, { page: 'p. 04' }),
    async run(d, { still }) {
      await sleep(300)
      await d.click({ text: 'Clean anyway' })
      await sleep(200)
      await d.click({ text: 'Detect & clean page' })
      await d.waitFor(`document.querySelectorAll('button.summary').length >= 3 && document.body.innerText.includes('1 of 7 cleaned')`, 10000, 'page cleaned')
      await sleep(700)
      await d.click({ text: 'Close Text cleanup' }, { after: 150 })
      await d.click({ js: layerRow('LaMa Manga', 0), fx: 0.45 })
      await d.waitFor(`!!${expandedSelect}`, 5000, 'Clean with picker')
      await sleep(400)
      const q = { js: expandedSelect }
      const menu = await d.openSelect(q)
      await sleep(300)
      const cloud = menu.items.findIndex((t) => t.startsWith('☁'))
      if (cloud < 0) throw new Error('no cloud entry in ' + menu.items.join(' | '))
      await d.menuTo(menu, cloud, 520)
      await sleep(700)
      await still('picker')
      await d.choose(q, menu, cloud)
      await d.waitFor(`!!document.querySelector('[role=dialog]') && document.querySelector('[role=dialog]').innerText.includes('cloud GPU')`, 6000, 'consent dialog')
      await sleep(300)
      await d.moveTo(760, 560, 600)
      await sleep(1000)
      await still('consent')
    },
  },
  {
    name: '06-longstrip',
    cursorAt: [1060, 640],
    async prepare(d) {
      await quietOpen(d, { project: 'Black Jack (long', chapter: 'Chapter 1' })
      await d.ev(byText('Chapter'))
      await d.ev(byText('Clean anyway'))
      await d.ev(byText('Zoom out'))
      await sleep(250)
      await d.ev(byText('Zoom out'))
      await sleep(600)
    },
    async run(d, { still }) {
      await sleep(300)
      await d.click({ text: 'Detect & clean chapter' })
      await sleep(300)
      await d.click({ text: 'Close Text cleanup' })
      await d.moveTo(1180, 560, 700)
      await sleep(300)
      // A smooth scroll down the strip while it cleans: the strip's own
      // scroller, eased in the page (wheel events through CDP stutter while
      // the screencast is encoding).

      const res = await d.ev(`(() => {
        const vis = (e) => e.scrollHeight > e.clientHeight + 200 && /(auto|scroll)/.test(getComputedStyle(e).overflowY)
        const box = [...document.querySelectorAll('*')].filter(vis).sort((a, b) => b.scrollHeight - a.scrollHeight)[0]
        const from = box.scrollTop, to = box.scrollHeight - box.clientHeight, t0 = performance.now(), dur = 6000
        const ease = (t) => t < .5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2
        return new Promise((res) => { const step = (now) => { const t = Math.min(1, (now - t0) / dur); box.scrollTop = from + (to - from) * ease(t); t < 1 ? requestAnimationFrame(step) : res([to, box.className]) }; requestAnimationFrame(step) })
      })()`)
      await sleep(400)
      await still('strip')
      await sleep(500)
    },
  },
  {
    name: '07-denoise',
    query: '?promo=1&cloudGpu=ready',
    session: { cloudAllowed: true },
    cursorAt: [900, 700],
    async run(d, { still }) {
      await sleep(300)
      await d.click({ text: 'Give My Regards' })
      await d.waitFor(`!!${chapterRow('Volume 2')}`, 8000, 'chapter row')
      await sleep(700)
      await d.click({ text: 'Actions for Volume 2' })
      await sleep(500)
      await d.click({ text: 'Denoise' })
      await d.waitFor(`!!document.querySelector('[role=dialog]')`, 5000, 'denoise dialog')
      await sleep(900)
      await d.click({ text: 'Cloud GPU', within: '[role=dialog]' })
      await d.waitFor(`document.querySelector('[role=dialog]')?.innerText.includes('Real-CUGAN')`, 5000, 'presets')
      await sleep(900)
      await d.hover({ text: 'Real-CUGAN 3x strong', tags: '[role=dialog] label, [role=dialog] [role=radio], [role=dialog] button, [role=dialog] li' })
      await sleep(500)
      await d.click({ text: 'Real-CUGAN 3x strong', tags: '[role=dialog] label, [role=dialog] [role=radio], [role=dialog] button, [role=dialog] li' })
      await sleep(1200)
      await still('presets')
      await sleep(600)
    },
  },
  {
    name: '08-cloud-setup',
    query: '?promo=1&firstLaunch=1',
    session: { firstLaunchOffered: false },
    cursorAt: [900, 640],
    async run(d, { still, c }) {
      await sleep(300)
      await d.click({ text: 'Set up the app' })
      await sleep(600)
      const next = async (label) => {
        await d.click({ text: label, exact: true }, { pause: 80, after: 150 })
        await sleep(150)
      }
      await next('Continue') // theme
      await next('Skip') // Hugging Face key
      await next('Continue') // background
      await next('Continue') // detection
      await next('Continue') // cleaning
      await next('Continue') // page denoise
      await d.waitFor(`document.body.innerText.includes('Set up now')`, 5000, 'cloud step')
      await sleep(500)
      await still('cloud-step')
      await d.click({ text: 'Set up now' })
      await d.waitFor(`document.body.innerText.includes('Paste Modal token command')`, 5000, 'token form')
      await sleep(400)
      // No token is typed: the form is shown empty.
      await d.hover({ js: `[...document.querySelectorAll('textarea, input:not([type]), input[type=text], input[type=password]')].find(i => i.getClientRects().length)`, fx: 0.3 })
      await sleep(1400)
      await still('token')
    },
  },
]

/* ---------------- main ---------------- */

const port = 9500 + Math.floor(Math.random() * 400)
const profile = join(tmpdir(), `promo-rec-${port}`)
const chrome = launchChrome(port, profile)
const results = []
try {
  for (const clip of CLIPS) {
    if (ONLY.length && !ONLY.includes(clip.name)) continue
    const t = Date.now()
    try {
      const r = await record(port, clip)
      results.push({ name: clip.name, ...r })
      console.log(`${clip.name}: ${r.failure ? 'FAILED ' + r.failure.message.split('\n')[0] : 'ok'} | ${r.frames} frames over ${r.seconds}s | ${r.probe} | ${((Date.now() - t) / 1000).toFixed(0)}s wall${r.errors.length ? ' | page errors: ' + r.errors.slice(0, 3).join(' || ').slice(0, 400) : ''}`)
    } catch (error) {
      console.log(`${clip.name}: ERROR ${error.message.split('\n')[0]}`)
    }
  }
} finally {
  chrome.kill()
  await sleep(300)
  try { rmSync(profile, { recursive: true, force: true }) } catch {}
}
