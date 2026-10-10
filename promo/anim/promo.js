// Manga Cleaner promo, cute cut: canvas animation scenes interleaved with real app clips.
// Every frame is a function of time t, so render.mjs can seek frame by frame.
// App clips come from promo/app/clips (recorded headlessly from the real app UI).
// Page results come from promo/cleaned (the real pipeline, run headlessly).

const W = 1920, H = 1080
const C = {
  paper: '#fdfbff', ink: '#23202b', muted: '#6f6a7d', pink: '#ff5c8a', pinkSoft: '#ffe1ea', pinkMid: '#ffb3c8',
  white: '#ffffff',
}
const DISPLAY = 'Fredoka, Nunito, system-ui, sans-serif'
const BODY = 'Nunito, system-ui, sans-serif'
const params = new URLSearchParams(location.search)
const RENDER = params.has('render')

const canvas = document.getElementById('c')
const ctx = canvas.getContext('2d')

// ---------- math ----------
const clamp = (x, a = 0, b = 1) => Math.min(b, Math.max(a, x))
const lerp = (a, b, k) => a + (b - a) * k
const seg = (t, a, b) => clamp((t - a) / (b - a))
const easeOut = k => 1 - Math.pow(1 - k, 3)
const easeIn = k => k * k * k
const easeInOut = k => (k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2)
// Damped spring from 0 to 1, for squash and stretch.
const spring = (k, f = 3.2, d = 5) => (k <= 0 ? 0 : 1 - Math.exp(-d * k) * Math.cos(f * Math.PI * k))
function rng(seed) {
  return () => { seed |= 0; seed = (seed + 0x6d2b79f5) | 0; let x = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    x = (x + Math.imul(x ^ (x >>> 7), 61 | x)) ^ x; return ((x ^ (x >>> 14)) >>> 0) / 4294967296 }
}

// ---------- assets ----------
const PAGES = ['page-010', 'page-025', 'page-047', 'page-048', 'page-049', 'page-050', 'page-100']
const img = {}, clean = {}
let regions = {}
const CLIPS = ['01-library', '02-detect', '03-clean', '04-layers', '05-colour', '06-longstrip', '07-denoise', '08-cloud-setup']
const clips = {}

const loadImage = src => new Promise(res => { const i = new Image(); i.onload = () => res(i); i.onerror = () => res(null); i.src = src })
function loadVideo(src) {
  return new Promise(res => {
    const v = document.createElement('video'); v.muted = true; v.preload = 'auto'; v.playsInline = true; v.src = src
    v.onloadeddata = () => res(v); v.onerror = () => res(null); document.body.appendChild(v)
  })
}
function normaliseRegions(raw) {
  const out = {}
  const list = Array.isArray(raw) ? raw : Array.isArray(raw?.pages) ? raw.pages : []
  for (const p of list) {
    const id = String(p.page || p.name || '').match(/page-\d{3}/)?.[0]; if (!id) continue
    out[id] = (p.regions || []).map(r => ({ x: r.bbox[0], y: r.bbox[1], w: r.bbox[2], h: r.bbox[3], outside: /outside/.test(r.kind || ''), status: r.status }))
  }
  return out
}
async function loadAll() {
  await Promise.all(PAGES.map(async id => {
    img[id] = await loadImage(`../pages/${id}.png`)
    clean[id] = await loadImage(`../cleaned/${id}-clean.png`)
  }))
  img.icon = await loadImage('assets/icon.png')
  try { const r = await fetch('../cleaned/regions.json'); if (r.ok) regions = normaliseRegions(await r.json()) } catch {}
  await Promise.all(CLIPS.map(async n => { clips[n] = await loadVideo(`../app/clips-seek/${n}.mp4`) }))
  try { await document.fonts.load('700 40px Fredoka'); await document.fonts.load('800 40px Nunito'); await document.fonts.ready } catch {}
}

// ---------- drawing primitives ----------
function rrect(g, x, y, w, h, r) {
  g.beginPath(); g.moveTo(x + r, y); g.arcTo(x + w, y, x + w, y + h, r); g.arcTo(x + w, y + h, x, y + h, r)
  g.arcTo(x, y + h, x, y, r); g.arcTo(x, y, x + w, y, r); g.closePath()
}
let tonePattern = null
function buildTone() {
  const o = document.createElement('canvas'); o.width = o.height = 28
  const g = o.getContext('2d'); g.fillStyle = C.pinkSoft
  g.beginPath(); g.arc(7, 7, 3.2, 0, Math.PI * 2); g.arc(21, 21, 3.2, 0, Math.PI * 2); g.fill()
  tonePattern = ctx.createPattern(o, 'repeat')
}
// Paper background with a drifting screentone field that fades out from one corner.
function paper(g, lt, corner = 'br') {
  g.fillStyle = C.paper; g.fillRect(0, 0, W, H)
  const cx = corner[1] === 'r' ? W : 0, cy = corner[0] === 'b' ? H : 0
  g.save(); g.translate((lt * 14) % 28, (lt * 7) % 28); g.fillStyle = tonePattern; g.fillRect(-28, -28, W + 56, H + 56); g.restore()
  const fade = g.createRadialGradient(cx, cy, 250, cx, cy, 1150)
  fade.addColorStop(0, 'rgba(253,251,255,0)'); fade.addColorStop(1, 'rgba(253,251,255,1)')
  g.fillStyle = fade; g.fillRect(0, 0, W, H)
}
function text(g, str, x, y, { size = 48, weight = 700, font = DISPLAY, color = C.ink, alpha = 1, align = 'left' } = {}) {
  if (alpha <= 0) return
  g.save(); g.globalAlpha *= alpha; g.fillStyle = color; g.textAlign = align; g.textBaseline = 'alphabetic'
  g.font = `${weight} ${size}px ${font}`; g.fillText(str, x, y); g.restore()
}
// Headline whose words hop in one by one.
function hop(g, str, x, y, lt, at, { size = 84, color = C.ink, stagger = 0.07, weight = 700, font = DISPLAY, out = 1e9 } = {}) {
  g.save(); g.font = `${weight} ${size}px ${font}`
  const fade = 1 - seg(lt, out, out + 0.35)
  let cx = x
  for (const [i, w] of str.split(' ').entries()) {
    const k = seg(lt, at + i * stagger, at + i * stagger + 0.55)
    const s = spring(k), ww = g.measureText(w).width
    if (k > 0) {
      g.save(); g.translate(cx + ww / 2, y); g.scale(lerp(0.4, 1, s), lerp(0.4, 1, s))
      text(g, w, -ww / 2, (1 - Math.min(1, s)) * 30, { size, color, weight, font, alpha: clamp(k * 3) * fade }); g.restore()
    }
    cx += ww + size * 0.26
  }
  g.restore()
}
function para(g, str, x, y, lt, at, { size = 30, color = C.muted } = {}) {
  const k = easeOut(seg(lt, at, at + 0.5))
  text(g, str, x, y + (1 - k) * 16, { size, color, weight: 700, font: BODY, alpha: k })
}
// Pill sticker, slightly rotated, springs in. Returns its width.
function sticker(g, label, x, y, k, { rot = -0.04, fill = C.pink, color = C.white, size = 30 } = {}) {
  g.save(); g.font = `700 ${size}px ${DISPLAY}`
  const w = g.measureText(label).width + size * 1.3, h = size * 1.9
  if (k <= 0) { g.restore(); return w }
  const s = spring(k)
  g.translate(x + w / 2, y + h / 2); g.rotate(rot); g.scale(s, s)
  g.shadowColor = 'rgba(255,92,138,.35)'; g.shadowBlur = 18; g.shadowOffsetY = 8
  rrect(g, -w / 2, -h / 2, w, h, h / 2); g.fillStyle = fill; g.fill()
  g.shadowColor = 'transparent'; g.fillStyle = color; g.textAlign = 'center'; g.textBaseline = 'middle'; g.fillText(label, 0, 2)
  g.restore(); return w
}
function pageBox(id, cx, cy, h) {
  const i = img[id], iw = i ? i.width : 1414, ih = i ? i.height : 2000, s = h / ih
  return { x: cx - (iw * s) / 2, y: cy - h / 2, w: iw * s, h, s }
}
function drawPage(g, im, b, { rot = 0, alpha = 1 } = {}) {
  if (!im) return
  g.save(); g.globalAlpha *= alpha; g.translate(b.x + b.w / 2, b.y + b.h / 2); g.rotate(rot)
  g.shadowColor = 'rgba(255,92,138,.28)'; g.shadowBlur = 50; g.shadowOffsetY = 24
  rrect(g, -b.w / 2 - 12, -b.h / 2 - 12, b.w + 24, b.h + 24, 22); g.fillStyle = C.white; g.fill()
  g.shadowColor = 'transparent'; g.drawImage(im, -b.w / 2, -b.h / 2, b.w, b.h); g.restore()
}

// ---------- the mascot: a speech bubble with a face ----------
// mood: 'happy' | 'itchy' | 'wow' | 'sleepy'. letters: real lettering inside (0..1). sx/sy: squash and stretch.
function bubbo(g, x, y, r, lt, { mood = 'happy', sx = 1, sy = 1, letters = 0, blush = 1, look = 0, rot = 0, fuzz = 0 } = {}) {
  g.save(); g.translate(x, y); g.rotate(rot); g.scale(sx, sy)
  const rx = r, ry = r * 0.8
  const body = () => {
    g.beginPath(); g.ellipse(0, 0, rx, ry, 0, 0, Math.PI * 2)
    g.moveTo(-rx * 0.35, ry * 0.8); g.lineTo(-rx * 0.62, ry * 1.32); g.lineTo(-rx * 0.02, ry * 0.9)
  }
  g.shadowColor = 'rgba(255,92,138,.30)'; g.shadowBlur = 30; g.shadowOffsetY = 14
  body(); g.fillStyle = C.white; g.fill(); g.shadowColor = 'transparent'
  // Real lettering cropped from page 010, fading as it gets cleaned.
  if (letters > 0 && img['page-010']) {
    const src = (regions['page-010'] || []).filter(q => !q.outside).sort((a, b) => b.w * b.h - a.w * a.h)[0] || { x: 794, y: 71, w: 192, h: 260 }
    g.save(); g.beginPath(); g.ellipse(0, 0, rx * 0.86, ry * 0.86, 0, 0, Math.PI * 2); g.clip(); g.globalAlpha = letters
    const s = (ry * 1.5) / src.h
    g.drawImage(img['page-010'], src.x, src.y, src.w, src.h, rx * 0.35 - (src.w * s) / 2, -ry * 0.75, src.w * s, src.h * s)
    g.restore()
  }
  g.lineWidth = r * 0.07; g.strokeStyle = C.ink; g.lineJoin = 'round'; body(); g.stroke()
  // Face sits left of the lettering while there is lettering.
  const fx = letters > 0 ? -rx * 0.38 : 0, fy = -ry * 0.05
  const blink = ((lt + x * 0.001) % 3.1) < 0.12
  const eyeY = fy - r * 0.08
  for (const dir of [-1, 1]) {
    g.save(); g.translate(fx + dir * r * 0.2 + look * r * 0.05, eyeY)
    g.lineWidth = r * 0.05; g.lineCap = 'round'; g.lineJoin = 'round'; g.strokeStyle = C.ink
    if (mood === 'sleepy' || blink) { g.beginPath(); g.arc(0, -r * 0.02, r * 0.07, 0.15 * Math.PI, 0.85 * Math.PI); g.stroke() }
    else if (mood === 'itchy') {
      // > <  squeezed eyes
      g.beginPath(); g.moveTo(-dir * r * 0.07, -r * 0.06); g.lineTo(dir * r * 0.06, 0); g.lineTo(-dir * r * 0.07, r * 0.06); g.stroke()
    } else {
      const er = mood === 'wow' ? r * 0.1 : r * 0.085
      g.beginPath(); g.ellipse(0, 0, er * 0.8, er, 0, 0, Math.PI * 2); g.fillStyle = C.ink; g.fill()
      g.beginPath(); g.arc(-er * 0.25, -er * 0.35, er * 0.3, 0, Math.PI * 2); g.fillStyle = C.white; g.fill()
    }
    g.restore()
  }
  if (blush > 0) {
    g.fillStyle = `rgba(255,92,138,${0.45 * blush})`
    for (const dx of [-r * 0.36, r * 0.36]) { g.beginPath(); g.ellipse(fx + dx, fy + r * 0.1, r * 0.09, r * 0.05, 0, 0, Math.PI * 2); g.fill() }
  }
  g.lineWidth = r * 0.05; g.lineCap = 'round'; g.strokeStyle = C.ink; g.beginPath()
  if (mood === 'wow') { g.ellipse(fx, fy + r * 0.2, r * 0.06, r * 0.08, 0, 0, Math.PI * 2); g.fillStyle = C.ink; g.fill() }
  else if (mood === 'itchy') { g.moveTo(fx - r * 0.12, fy + r * 0.2); for (let i = 1; i <= 4; i++) g.lineTo(fx - r * 0.12 + i * r * 0.06, fy + r * 0.2 + (i % 2 ? -r * 0.04 : 0)); g.stroke() }
  else if (mood === 'sleepy') { g.moveTo(fx - r * 0.06, fy + r * 0.2); g.lineTo(fx + r * 0.06, fy + r * 0.2); g.stroke() }
  else { g.arc(fx, fy + r * 0.1, r * 0.12, 0.15 * Math.PI, 0.85 * Math.PI); g.stroke() }
  // Static fuzz for the denoise scene.
  if (fuzz > 0) {
    const R = rng(Math.floor(lt * 15)); g.save(); g.beginPath(); g.ellipse(0, 0, rx * 1.15, ry * 1.2, 0, 0, Math.PI * 2); g.clip()
    for (let i = 0; i < 900 * fuzz; i++) { g.fillStyle = R() > 0.5 ? 'rgba(35,32,43,.55)' : 'rgba(255,255,255,.7)'; g.fillRect((R() - 0.5) * rx * 2.3, (R() - 0.5) * ry * 2.4, 5, 5) }
    g.restore()
  }
  g.restore()
}
function sparkle(g, x, y, s, k) {
  if (k <= 0 || k >= 1) return
  const r = s * Math.sin(k * Math.PI)
  g.save(); g.translate(x, y); g.rotate(k * 1.2); g.fillStyle = C.pink
  g.beginPath(); for (let i = 0; i < 8; i++) { const ang = (i * Math.PI) / 4, rr = i % 2 ? r * 0.28 : r; g.lineTo(Math.cos(ang) * rr, Math.sin(ang) * rr) }
  g.closePath(); g.fill(); g.restore()
}

// ---------- scenes ----------
function sceneIntro(g, lt) {
  paper(g, lt, 'br')
  // Bubbo drops in stuffed with real lettering, itches, gets squeegeed, smiles.
  const drop = seg(lt, 0.2, 1.1), land = spring(seg(lt, 1.0, 2.0), 3, 5)
  const y = lt < 1.1 ? lerp(-300, 560, easeIn(drop)) : 560
  const squash = lt < 1.0 ? 1 : 1 + (1 - land) * 0.25
  const itchy = lt > 1.4 && lt < 3.4
  const wig = itchy ? Math.sin(lt * 30) * 0.06 : 0
  const cleanK = easeInOut(seg(lt, 3.4, 4.3))
  bubbo(g, 1320, y, 250, lt, { mood: cleanK > 0.6 ? 'happy' : itchy ? 'itchy' : 'wow', sx: squash, sy: 2 - squash, letters: 1 - cleanK, rot: wig, blush: cleanK })
  if (cleanK > 0 && cleanK < 1) {
    const sx = lerp(1000, 1640, cleanK)
    g.save(); g.fillStyle = C.pink; rrect(g, sx - 14, 320, 28, 470, 14); g.fill(); rrect(g, sx - 60, 290, 120, 40, 20); g.fill(); g.restore()
  }
  sparkle(g, 1120, 380, 60, seg(lt, 4.2, 5.0)); sparkle(g, 1560, 420, 44, seg(lt, 4.4, 5.2)); sparkle(g, 1500, 760, 36, seg(lt, 4.6, 5.3))
  hop(g, 'Too much', 140, 400, lt, 1.4, { size: 96 })
  hop(g, 'text?', 140, 510, lt, 1.7, { size: 96, color: C.pink })
  hop(g, 'Manga Cleaner', 140, 690, lt, 4.6, { size: 72 })
  para(g, 'Finds and cleans text on manga and comic pages.', 144, 750, lt, 5.1)
  sticker(g, 'Free and open source', 140, 790, seg(lt, 5.6, 6.3))
}

function sceneDetect(g, lt) {
  paper(g, lt, 'bl')
  const id = 'page-010', b = pageBox(id, 1380, 540, 940)
  const enter = spring(seg(lt, 0, 0.9), 2.5, 5)
  drawPage(g, img[id], { ...b, y: b.y + (1 - enter) * 700 }, { rot: (1 - enter) * 0.1 - 0.015 })
  // Real detected regions pop on with a bounce; outside-bubble text is dashed.
  const list = regions[id] || []
  const cx0 = b.x + b.w / 2, cy0 = b.y + b.h / 2
  list.forEach((r, i) => {
    const k = seg(lt, 1.2 + i * 0.06, 1.6 + i * 0.06); if (k <= 0) return
    const s = spring(k)
    g.save(); g.translate(cx0, cy0); g.rotate(-0.015); g.translate(-cx0, -cy0)
    const cx = b.x + (r.x + r.w / 2) * b.s, cy = b.y + (r.y + r.h / 2) * b.s
    const w = r.w * b.s * s + 8, h = r.h * b.s * s + 8
    rrect(g, cx - w / 2, cy - h / 2, w, h, 10); g.fillStyle = 'rgba(255,92,138,.16)'; g.fill()
    g.lineWidth = 4; g.strokeStyle = C.pink; if (r.outside) g.setLineDash([10, 7]); g.stroke(); g.restore()
  })
  bubbo(g, 260, 880, 100, lt, { mood: lt > 2.5 ? 'wow' : 'happy', look: 1, sx: 1 + Math.sin(lt * 5) * 0.02, sy: 1 - Math.sin(lt * 5) * 0.02 })
  hop(g, 'Finds', 140, 300, lt, 0.5, { size: 96 })
  hop(g, 'every word.', 140, 410, lt, 0.8, { size: 96, color: C.pink })
  para(g, 'Speech bubbles, captions, even sound effects.', 144, 480, lt, 2.2)
  if (list.length) sticker(g, `${list.length} found on this page`, 140, 540, seg(lt, 3.4, 4.0), { rot: 0.03, fill: C.ink })
}

function sceneClean(g, lt) {
  paper(g, lt, 'tr')
  // Real before and after with a swinging wipe.
  const id = 'page-025', b = pageBox(id, 1380, 540, 940), rot = 0.012
  drawPage(g, img[id], b, { rot })
  const wk = seg(lt, 1.0, 1.6)
  const wipe = 0.5 - 0.44 * Math.cos(Math.max(0, lt - 1.0) * 1.1)
  if (clean[id] && wk > 0) {
    const wx = b.x + b.w * lerp(0, wipe, easeOut(wk))
    g.save(); g.translate(b.x + b.w / 2, b.y + b.h / 2); g.rotate(rot); g.translate(-(b.x + b.w / 2), -(b.y + b.h / 2))
    g.save(); g.beginPath(); g.rect(b.x, b.y, wx - b.x, b.h); g.clip(); g.drawImage(clean[id], b.x, b.y, b.w, b.h); g.restore()
    const my = b.y + b.h / 2
    g.fillStyle = C.pink; rrect(g, wx - 5, b.y - 20, 10, b.h + 40, 5); g.fill()
    g.beginPath(); g.arc(wx, my, 30, 0, Math.PI * 2); g.fill()
    g.fillStyle = C.white; g.beginPath(); g.moveTo(wx - 16, my); g.lineTo(wx - 5, my - 10); g.lineTo(wx - 5, my + 10); g.closePath()
    g.moveTo(wx + 16, my); g.lineTo(wx + 5, my - 10); g.lineTo(wx + 5, my + 10); g.closePath(); g.fill()
    g.restore()
  }
  // Two steps: Fill for flat paper, LaMa Manga for tone and line art.
  const steps = [['Fill', 'flat paper, no AI needed'], ['LaMa Manga', 'screentone and line art']]
  steps.forEach(([name, note], i) => {
    const k = seg(lt, 2.0 + i * 0.5, 2.6 + i * 0.5); if (k <= 0) return
    const s = spring(k), y = 720 + i * 116
    g.save(); g.translate(150, y); g.scale(s, s)
    rrect(g, 0, -50, 640, 96, 48); g.fillStyle = i ? C.pink : C.pinkSoft; g.fill()
    text(g, name, 36, 12, { size: 40, color: i ? C.white : C.ink })
    g.font = `700 40px ${DISPLAY}`; const nw = g.measureText(name).width
    text(g, note, 36 + nw + 20, 10, { size: 24, weight: 700, font: BODY, color: i ? 'rgba(255,255,255,.92)' : C.muted })
    g.restore()
  })
  hop(g, 'Cleans it', 140, 300, lt, 0.3, { size: 96 })
  hop(g, 'like new.', 140, 410, lt, 0.6, { size: 96, color: C.pink })
  para(g, 'The lightest model that does the job, bubble by bubble.', 144, 480, lt, 1.4)
  hop(g, 'Black and white? Use LaMa Manga.', 144, 600, lt, 3.4, { size: 40, weight: 600, stagger: 0.04 })
  para(g, 'Better models are on the way.', 144, 648, lt, 4.2, { size: 26 })
}

function modelCard(g, x, y, w, h, rot, title, sub, k, { cloud = false, hero = false } = {}) {
  if (k <= 0) return
  const s = spring(k)
  g.save(); g.translate(x, y + (1 - Math.min(1, s)) * 120); g.rotate(rot); g.scale(s, s)
  g.shadowColor = 'rgba(255,92,138,.3)'; g.shadowBlur = 28; g.shadowOffsetY = 14
  rrect(g, -w / 2, -h / 2, w, h, 26); g.fillStyle = hero ? C.pink : C.white; g.fill(); g.shadowColor = 'transparent'
  g.lineWidth = 4; g.strokeStyle = hero ? C.pink : C.ink; g.stroke()
  if (cloud) { g.fillStyle = C.pinkSoft; rrect(g, w / 2 - 118, -h / 2 + 18, 100, 38, 19); g.fill(); text(g, 'cloud', w / 2 - 68, -h / 2 + 45, { size: 22, weight: 800, font: BODY, color: C.pink, align: 'center' }) }
  const lines = title.split('\n')
  lines.forEach((l, i) => text(g, l, -w / 2 + 26, -h / 2 + (hero ? 64 : 100) + i * 40, { size: 34, color: hero ? C.white : C.ink }))
  text(g, sub, -w / 2 + 26, h / 2 - 28, { size: 22, weight: 700, font: BODY, color: hero ? 'rgba(255,255,255,.92)' : C.muted })
  g.restore()
}
function sceneColour(g, lt) {
  paper(g, lt, 'bl')
  const id = 'page-048', b = pageBox(id, 460, 560, 900)
  drawPage(g, img[id], b, { rot: -0.03 })
  hop(g, 'Colour comics?', 900, 250, lt, 0.4, { size: 84 })
  hop(g, 'FLUX and Qwen shine.', 900, 350, lt, 0.8, { size: 84, color: C.pink, stagger: 0.06 })
  const cards = [
    ['FLUX.2 Klein\n4B', 'local or cloud', false], ['FLUX.2 Klein\n9B', 'cloud GPU', true],
    ['Qwen-Image-\nEdit-2511', 'great on sound effects', true],
  ]
  cards.forEach(([t, s, cl], i) => modelCard(g, 1060 + i * 300, 560, 270, 250, (i - 1) * 0.05, t, s, seg(lt, 2.0 + i * 0.35, 2.7 + i * 0.35), { cloud: cl }))
  para(g, 'More models land with every release.', 904, 760, lt, 3.6)
  modelCard(g, 1360, 900, 620, 150, 0.02, 'Our own comic model', 'planned: mid-sized, fine-tuned for comics', seg(lt, 4.8, 5.5), { hero: true })
  bubbo(g, 1780, 200, 70, lt, { mood: 'wow', rot: 0.1 })
}

function sceneStrip(g, lt) {
  paper(g, lt, 'tr')
  // Bubbo stretches into a tall noodle, like a long strip.
  const st = spring(seg(lt, 0.4, 1.6), 2.4, 4)
  bubbo(g, 1380, 560, 150, lt, { mood: 'wow', sx: lerp(1, 0.6, st), sy: lerp(1, 2.3, st) })
  hop(g, 'Webtoons?', 140, 440, lt, 0.3, { size: 110 })
  hop(g, 'Long strips too.', 140, 560, lt, 0.8, { size: 84, color: C.pink })
  para(g, 'Split only between panels, never through a face.', 144, 630, lt, 1.6)
}

function sceneNoise(g, lt) {
  paper(g, lt, 'br')
  const shake = seg(lt, 2.4, 3.6), fuzz = 1 - easeOut(shake)
  const wob = shake > 0 && shake < 1 ? Math.sin(lt * 50) * 0.12 * (1 - shake) : 0
  bubbo(g, 1380, 540, 230, lt, { mood: fuzz > 0.4 ? 'itchy' : 'happy', fuzz, rot: wob, blush: 1 - fuzz })
  for (let i = 0; i < 6; i++) sparkle(g, 1380 + Math.cos(i) * 330, 540 + Math.sin(i * 1.7) * 260, 30, seg(lt, 3.2 + i * 0.08, 4.0 + i * 0.08))
  hop(g, 'Grainy scans?', 140, 380, lt, 0.3, { size: 96 })
  hop(g, 'Shake it off.', 140, 490, lt, 2.6, { size: 96, color: C.pink })
  para(g, 'Denoise a whole chapter, then compare before and after.', 144, 560, lt, 3.4)
  let x = 140
  ;['MangaJaNai', 'waifu2x', 'Real-CUGAN'].forEach((s, i) => { x += sticker(g, s, x, 610, seg(lt, 4.0 + i * 0.25, 4.6 + i * 0.25), { fill: i === 0 ? C.pink : C.ink, rot: (i - 1) * 0.04 }) + 18 })
}

function laptopBuddy(g, cx, cy, s, stress, lt) {
  g.save(); g.translate(cx, cy); g.scale(s, s)
  g.shadowColor = 'rgba(255,92,138,.25)'; g.shadowBlur = 30; g.shadowOffsetY = 14
  rrect(g, -220, -170, 440, 290, 30); g.fillStyle = C.white; g.fill(); g.shadowColor = 'transparent'
  g.lineWidth = 8; g.strokeStyle = C.ink; g.stroke()
  rrect(g, -280, 120, 560, 46, 23); g.fillStyle = C.pinkSoft; g.fill(); g.stroke()
  for (const dx of [-60, 60]) {
    g.save(); g.translate(dx, -40); g.scale(1, lerp(1, 0.2, stress))
    g.beginPath(); g.ellipse(0, 0, 14, 18, 0, 0, Math.PI * 2); g.fillStyle = C.ink; g.fill(); g.restore()
  }
  g.fillStyle = `rgba(255,92,138,${0.5 * (1 - stress)})`
  for (const dx of [-100, 100]) { g.beginPath(); g.ellipse(dx, 0, 22, 12, 0, 0, Math.PI * 2); g.fill() }
  g.lineWidth = 7; g.lineCap = 'round'; g.beginPath(); g.moveTo(-36, 40); g.quadraticCurveTo(0, 40 + lerp(-20, 26, 1 - stress), 36, 40); g.stroke()
  if (stress > 0.3) for (let i = 0; i < 2; i++) {
    const d = (lt * 0.9 + i * 0.5) % 1
    g.save(); g.globalAlpha = (1 - d) * stress; g.translate(150 + i * 36, -120 + d * 80)
    g.beginPath(); g.moveTo(0, -16); g.quadraticCurveTo(14, 8, 0, 12); g.quadraticCurveTo(-14, 8, 0, -16); g.fillStyle = C.pink; g.fill(); g.restore()
  }
  g.restore()
}
function sceneLight(g, lt) {
  paper(g, lt, 'bl')
  const facts = [
    ['About 2 GB', 'peak memory, for 10 pages or 200'],
    ['No AI', 'when the paper is flat'],
    ['Frees memory', 'by itself when your machine runs low'],
    ['About 320 MB', 'of models to download, no Python'],
  ]
  const shown = facts.filter((_, i) => lt > 2.0 + i * 1.3).length
  const stress = 1 - easeInOut(clamp(shown / facts.length))
  laptopBuddy(g, 1420, 520, 1.15 + Math.sin(lt * 3) * 0.01, stress, lt)
  hop(g, 'Old computer?', 140, 300, lt, 0.3, { size: 96 })
  hop(g, 'No sweat.', 140, 410, lt, 1.0, { size: 96, color: C.pink })
  facts.forEach(([big, small], i) => {
    const k = seg(lt, 2.0 + i * 1.3, 2.6 + i * 1.3); if (k <= 0) return
    const s = spring(k), y = 490 + i * 118, x = 140 + (i % 2) * 40
    g.save(); g.translate(x, y); g.scale(s, s)
    rrect(g, 0, 0, 760, 96, 48); g.fillStyle = i % 2 ? C.pinkSoft : C.white; g.fill(); g.lineWidth = 3; g.strokeStyle = C.ink; g.stroke()
    text(g, big, 34, 62, { size: 38, color: C.pink })
    g.font = `700 38px ${DISPLAY}`; const bw = g.measureText(big).width
    text(g, small, 34 + bw + 18, 60, { size: 24, weight: 700, font: BODY, color: C.muted })
    g.restore()
  })
}

function cloudBuddy(g, x, y, s, lt) {
  g.save(); g.translate(x, y + Math.sin(lt * 2.2) * 10); g.scale(s, s)
  g.shadowColor = 'rgba(255,92,138,.3)'; g.shadowBlur = 30; g.shadowOffsetY = 14
  g.beginPath()
  for (const [cx, cy, r] of [[-110, 30, 70], [-30, -30, 100], [80, -5, 82], [150, 45, 55], [-170, 60, 45]]) { g.moveTo(cx + r, cy); g.arc(cx, cy, r, 0, Math.PI * 2) }
  g.fillStyle = C.white; g.fill(); g.shadowColor = 'transparent'
  g.fillRect(-170, 40, 330, 65)
  for (const dx of [-45, 35]) { g.beginPath(); g.ellipse(dx, 10, 11, 14, 0, 0, Math.PI * 2); g.fillStyle = C.ink; g.fill() }
  g.fillStyle = 'rgba(255,92,138,.5)'; for (const dx of [-85, 75]) { g.beginPath(); g.ellipse(dx, 36, 18, 10, 0, 0, Math.PI * 2); g.fill() }
  g.lineWidth = 6; g.lineCap = 'round'; g.strokeStyle = C.ink; g.beginPath(); g.arc(-5, 30, 18, 0.15 * Math.PI, 0.85 * Math.PI); g.stroke()
  g.restore()
}
function hoursDial(g, x, y, k, big, small) {
  g.save(); g.translate(x, y)
  g.beginPath(); g.arc(0, 0, 105, 0, Math.PI * 2); g.fillStyle = C.white; g.fill(); g.lineWidth = 3; g.strokeStyle = C.ink; g.stroke()
  g.lineWidth = 18; g.lineCap = 'round'; g.strokeStyle = C.pink
  if (k > 0) { g.beginPath(); g.arc(0, 0, 80, -Math.PI / 2, -Math.PI / 2 + Math.PI * 2 * k); g.stroke() }
  text(g, big, 0, 16, { size: 46, align: 'center' })
  text(g, small, 0, 150, { size: 26, weight: 800, font: BODY, color: C.muted, align: 'center' })
  g.restore()
}
function sceneCloud(g, lt) {
  paper(g, lt, 'tr')
  const come = spring(seg(lt, 0.2, 1.4), 2, 4)
  cloudBuddy(g, lerp(2300, 1480, come), 300, 1.2, lt)
  hop(g, 'Still too slow?', 140, 300, lt, 0.4, { size: 84 })
  hop(g, 'Borrow a free GPU.', 140, 400, lt, 1.2, { size: 84, color: C.pink })
  para(g, 'On your own Modal or Beam account. The app sets it up.', 144, 470, lt, 2.0)
  hoursDial(g, 360, 720, easeInOut(seg(lt, 2.8, 4.2)), '20+ h', 'a month on L4')
  hoursDial(g, 680, 720, easeInOut(seg(lt, 3.1, 4.0)) * 0.5, '~10 h', 'a month on L40S')
  sticker(g, 'Only the crop is sent. You confirm every run.', 960, 640, seg(lt, 4.8, 5.4), { fill: C.ink, rot: -0.02, size: 28 })
  para(g, 'The provider asks for card verification.', 964, 780, lt, 5.8, { size: 24 })
}

function sceneOutro(g, lt) {
  paper(g, lt, 'br')
  const tiles = [['Mac', 'works fully today', true], ['Windows', 'help us polish it', false], ['Linux', 'help us polish it', false]]
  const out = easeInOut(seg(lt, 5.4, 6.2))
  tiles.forEach(([n, s, ok], i) => {
    const k = seg(lt, 0.3 + i * 0.3, 0.9 + i * 0.3); if (k <= 0 || out >= 1) return
    const sp = spring(k), x = 420 + i * 540, y = 420
    g.save(); g.globalAlpha = 1 - out; g.translate(x, y - out * 80); g.rotate((i - 1) * 0.04); g.scale(sp, sp)
    g.shadowColor = 'rgba(255,92,138,.3)'; g.shadowBlur = 30; g.shadowOffsetY = 14
    rrect(g, -220, -150, 440, 300, 40); g.fillStyle = ok ? C.pink : C.white; g.fill(); g.shadowColor = 'transparent'
    g.lineWidth = 4; g.strokeStyle = ok ? C.pink : C.ink; if (!ok) g.setLineDash([16, 12]); g.stroke(); g.setLineDash([])
    text(g, n, 0, 0, { size: 72, color: ok ? C.white : C.ink, align: 'center' })
    text(g, s, 0, 60, { size: 28, weight: 800, font: BODY, color: ok ? 'rgba(255,255,255,.95)' : C.muted, align: 'center' })
    g.restore()
  })
  hop(g, 'Windows and Linux: collaborators wanted.', 360, 780, lt, 2.0, { size: 52, stagger: 0.05, out: 5.4 })
  const e = seg(lt, 6.0, 6.9)
  if (e > 0) {
    const s = spring(e)
    if (img.icon) { g.save(); g.translate(W / 2, 320); g.scale(s, s); g.drawImage(img.icon, -140, -140, 280, 280); g.restore() }
    bubbo(g, W / 2 + 290, 250, 80, lt, { mood: 'happy', rot: Math.sin(lt * 4) * 0.08, sx: s, sy: s })
    g.save(); g.font = `700 110px ${DISPLAY}`; const tw = g.measureText('Manga Cleaner').width; g.restore()
    hop(g, 'Manga Cleaner', W / 2 - tw / 2 - 14, 600, lt, 6.4, { size: 110 })
    g.save(); g.font = `700 30px ${DISPLAY}`; const sw = g.measureText('Free. Open source. GPL-3.0.').width + 39; g.restore()
    sticker(g, 'Free. Open source. GPL-3.0.', W / 2 - sw / 2, 650, seg(lt, 7.0, 7.6), { rot: -0.02 })
    text(g, 'cleaner.komiq.cc', W / 2, 850, { size: 56, align: 'center', alpha: seg(lt, 7.6, 8.2) })
    text(g, 'github.com/k-omiq/manga-cleaner', W / 2, 910, { size: 30, weight: 800, font: BODY, color: C.muted, align: 'center', alpha: seg(lt, 7.9, 8.5) })
  }
}

// App clip scene: the real recording in a soft window with a sticker caption and Bubbo peeking.
// rate speeds the recording up; zoom keys are [clipSeconds, centreX, centreY, scale] in clip-relative units.
function camera(keys, t) {
  if (!keys || !keys.length) return [0.5, 0.5, 1]
  if (t <= keys[0][0]) return keys[0].slice(1)
  for (let i = 1; i < keys.length; i++) {
    const [ta, ...a] = keys[i - 1], [tb, ...b] = keys[i]
    if (t <= tb) { const k = easeInOut(seg(t, ta, tb)); return a.map((v, j) => lerp(v, b[j], k)) }
  }
  return keys.at(-1).slice(1)
}
function appScene(name, caption, { rate = 1.6, zoom = null } = {}) {
  return {
    dur: () => { const v = clips[name]; return v ? v.duration / rate : 4 },
    video: name, rate,
    draw(g, lt) {
      paper(g, lt, 'bl')
      const k = spring(seg(lt, 0, 0.8), 2.4, 5)
      const fw = 1560, fh = 975, fx = W / 2 - fw / 2 + 60, fy = 52
      g.save(); g.translate(fx + fw / 2, fy + fh / 2); g.scale(lerp(0.85, 1, k), lerp(0.85, 1, k)); g.translate(-(fx + fw / 2), -(fy + fh / 2))
      g.shadowColor = 'rgba(255,92,138,.32)'; g.shadowBlur = 60; g.shadowOffsetY = 26
      rrect(g, fx - 14, fy - 14, fw + 28, fh + 28, 36); g.fillStyle = C.white; g.fill(); g.shadowColor = 'transparent'
      g.lineWidth = 4; g.strokeStyle = C.ink; g.stroke()
      g.save(); rrect(g, fx, fy, fw, fh, 24); g.clip()
      const v = clips[name]
      if (v && v.readyState >= 2) {
        const [cx, cy, sc] = camera(zoom, lt * rate)
        const vw = v.videoWidth, vh = v.videoHeight, sw = vw / sc, sh = vh / sc
        const sx = clamp(cx * vw - sw / 2, 0, vw - sw), sy = clamp(cy * vh - sh / 2, 0, vh - sh)
        g.drawImage(v, sx, sy, sw, sh, fx, fy, fw, fh)
      } else { g.fillStyle = C.pinkSoft; g.fillRect(fx, fy, fw, fh); text(g, `app clip ${name} is missing`, fx + fw / 2, fy + fh / 2, { size: 40, align: 'center', color: C.pink }) }
      g.restore(); g.restore()
      sticker(g, caption, fx - 40, fy + fh - 40, seg(lt, 0.4, 1.0), { rot: -0.035, size: 34 })
      const peek = spring(seg(lt, 0.8, 1.6), 2.2, 4)
      if (peek > 0) bubbo(g, fx - 20, fy + 150 + (1 - peek) * 120, 70, lt, { mood: 'wow', rot: -0.25, sx: peek, sy: peek, look: 1 })
    },
  }
}
const js = (dur, draw) => ({ dur: () => dur, draw })

const PLAN = [
  js(7.0, sceneIntro),
  appScene('01-library', 'Open your chapter', { rate: 1.6, zoom: [[0, 0.3, 0.62, 1.5], [3.5, 0.3, 0.62, 1.5], [5, 0.5, 0.3, 1.2], [7, 0.5, 0.5, 1]] }),
  js(5.5, sceneDetect),
  appScene('02-detect', 'Detect, for real', { rate: 1.5, zoom: [[0, 0.5, 0.5, 1], [2, 0.55, 0.35, 1.35], [4, 0.42, 0.45, 1.45]] }),
  js(6.5, sceneClean),
  appScene('03-clean', 'Clean the whole chapter', { rate: 1.6, zoom: [[0, 0.5, 0.5, 1], [2, 0.32, 0.38, 1.35], [9, 0.32, 0.38, 1.35], [11, 0.5, 0.5, 1.05]] }),
  appScene('04-layers', 'Every bubble is a layer', { rate: 1.6, zoom: [[0, 0.5, 0.5, 1], [1.5, 0.28, 0.5, 1.6]] }),
  js(7.0, sceneColour),
  appScene('05-colour', 'Pick a model per bubble', { rate: 1.6, zoom: [[0, 0.5, 0.5, 1], [1.2, 0.5, 0.35, 1.35], [5.5, 0.5, 0.35, 1.35], [7, 0.3, 0.6, 1.5], [10.5, 0.3, 0.6, 1.5], [11.8, 0.5, 0.55, 1.35]] }),
  js(3.6, sceneStrip),
  appScene('06-longstrip', 'Long strips, one scroll', { rate: 1.9, zoom: [[0, 0.5, 0.5, 1], [3, 0.45, 0.5, 1.2]] }),
  js(5.5, sceneNoise),
  appScene('07-denoise', 'Denoise from the chapter menu', { rate: 1.8, zoom: [[0, 0.5, 0.5, 1], [5, 0.5, 0.5, 1.12]] }),
  js(7.8, sceneLight),
  js(7.0, sceneCloud),
  appScene('08-cloud-setup', 'Cloud setup takes one token', { rate: 1.7, zoom: [[0, 0.5, 0.5, 1], [6, 0.5, 0.5, 1], [8, 0.45, 0.42, 1.4]] }),
  js(10.0, sceneOutro),
]
const TRANS = 0.7
let TIMELINE = [], DURATION = 120
function buildTimeline() {
  let t = 0
  TIMELINE = PLAN.map(p => { const d = p.dur(); const e = { ...p, s: t, d }; t += d; return e })
  DURATION = t
}

// ---------- transition: a pink speech bubble swells to cover, then opens on the next scene ----------
const bufA = document.createElement('canvas'), bufB = document.createElement('canvas')
bufA.width = bufB.width = W; bufA.height = bufB.height = H
const ga = bufA.getContext('2d'), gb = bufB.getContext('2d')
function bubblePath(g, cx, cy, r) {
  g.beginPath(); g.ellipse(cx, cy, r, r * 0.82, 0, 0, Math.PI * 2)
  g.moveTo(cx - r * 0.3, cy + r * 0.78); g.lineTo(cx - r * 0.55, cy + r * 1.2); g.lineTo(cx + r * 0.02, cy + r * 0.85)
}
function bubbleWipe(g, A, B, p) {
  const R = 1500, cx = W * 0.55, cy = H * 0.45
  if (p < 0.5) {
    g.drawImage(A, 0, 0)
    g.fillStyle = C.pink; bubblePath(g, cx, cy, easeIn(p * 2) * R); g.fill()
  } else {
    g.fillStyle = C.pink; g.fillRect(0, 0, W, H)
    g.save(); bubblePath(g, cx, cy, easeOut((p - 0.5) * 2) * R); g.clip(); g.drawImage(B, 0, 0); g.restore()
  }
}

// ---------- rendering ----------
async function seekVideo(sc, lt) {
  const v = clips[sc.video]; if (!v) return
  const target = Math.min(v.duration - 0.01, Math.max(0, lt * (sc.rate || 1)))
  if (Math.abs(v.currentTime - target) < 0.004) return
  await new Promise(res => { v.addEventListener('seeked', res, { once: true }); v.currentTime = target })
}
async function drawScene(g, sc, lt) { if (sc.video) await seekVideo(sc, lt); sc.draw(g, lt) }
async function render(t) {
  t = clamp(t, 0, DURATION - 1e-6)
  const i = TIMELINE.findIndex(sc => t >= sc.s && t < sc.s + sc.d)
  const sc = TIMELINE[i], lt = t - sc.s
  if (i > 0 && lt < TRANS) {
    const prev = TIMELINE[i - 1]
    await drawScene(ga, prev, prev.d - 0.001); await drawScene(gb, sc, lt)
    bubbleWipe(ctx, bufA, bufB, lt / TRANS)
  } else await drawScene(ctx, sc, lt)
}

// ---------- boot ----------
let playing = false, t0 = 0, cur = 0, busy = false
const seekEl = document.getElementById('seek'), clockEl = document.getElementById('clock'), playEl = document.getElementById('play')
async function show(t) { cur = t; busy = true; await render(t); busy = false; seekEl.value = t; clockEl.textContent = `${t.toFixed(2)} / ${DURATION.toFixed(1)}` }
function loop(now) {
  if (!playing) return
  const t = (now - t0) / 1000
  if (t >= DURATION) { playing = false; playEl.textContent = 'Play'; return }
  if (!busy) show(t)
  requestAnimationFrame(loop)
}
playEl.onclick = () => { playing = !playing; playEl.textContent = playing ? 'Pause' : 'Play'; if (playing) { t0 = performance.now() - cur * 1000; requestAnimationFrame(loop) } }
seekEl.oninput = () => { playing = false; playEl.textContent = 'Play'; show(+seekEl.value) }
document.body.classList.add(RENDER ? 'render' : 'preview')

window.ready = loadAll().then(async () => {
  buildTone(); buildTimeline(); seekEl.max = DURATION
  await show(+(params.get('t') || 0)); return true
})
window.seek = async t => { await render(t); return true }
window.duration = () => DURATION
