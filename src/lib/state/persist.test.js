import { describe, it, expect, vi } from 'vitest'
import {
  readRecord,
  writeRecord,
  oneOf,
  boolOr,
  numberIn,
  idOr,
  idList,
  plainObject,
  capEntries,
  STORAGE_PREFIX,
} from './persist.js'

/** A minimal in-memory Storage. `throwOn` simulates private mode / quota. */
function fakeStorage(seed = {}, throwOn = null) {
  const map = new Map(Object.entries(seed))
  return {
    getItem(key) {
      if (throwOn === 'get') throw new Error('denied')
      return map.has(key) ? map.get(key) : null
    },
    setItem(key, value) {
      if (throwOn === 'set') throw new Error('quota')
      map.set(key, value)
    },
    removeItem(key) {
      map.delete(key)
    },
    map,
  }
}

describe('readRecord', () => {
  it('returns the fallback for absent, corrupt and non-object values', () => {
    const store = fakeStorage({
      [`${STORAGE_PREFIX}corrupt`]: '{"theme":',
      [`${STORAGE_PREFIX}primitive`]: '"dark"',
      [`${STORAGE_PREFIX}array`]: '[1,2,3]',
      [`${STORAGE_PREFIX}null`]: 'null',
      [`${STORAGE_PREFIX}empty`]: '',
    })
    const fallback = { ok: true }
    for (const key of ['missing', 'corrupt', 'primitive', 'array', 'null', 'empty']) {
      expect(readRecord(key, fallback, store), key).toBe(fallback)
    }
  })

  it('returns the parsed object when it is one', () => {
    const store = fakeStorage({ [`${STORAGE_PREFIX}session`]: '{"theme":"dark"}' })
    expect(readRecord('session', null, store)).toEqual({ theme: 'dark' })
  })

  it('survives a storage that throws on access', () => {
    expect(readRecord('session', 'fallback', fakeStorage({}, 'get'))).toBe('fallback')
    expect(readRecord('session', 'fallback', null)).toBe('fallback')
  })
})

describe('writeRecord', () => {
  it('writes under the prefix and reports success', () => {
    const store = fakeStorage()
    expect(writeRecord('session', { theme: 'dark' }, store)).toBe(true)
    expect(store.map.get(`${STORAGE_PREFIX}session`)).toBe('{"theme":"dark"}')
  })

  it('reports failure instead of throwing when the quota is gone', () => {
    expect(writeRecord('session', { theme: 'dark' }, fakeStorage({}, 'set'))).toBe(false)
    expect(writeRecord('session', { theme: 'dark' }, null)).toBe(false)
  })
})

describe('validators', () => {
  it('oneOf rejects anything not in the list', () => {
    expect(oneOf('dark', ['light', 'dark'], 'light')).toBe('dark')
    expect(oneOf('neon', ['light', 'dark'], 'light')).toBe('light')
    expect(oneOf(7, ['light', 'dark'], 'light')).toBe('light')
    expect(oneOf(undefined, ['light', 'dark'], 'light')).toBe('light')
  })

  it('boolOr only accepts real booleans', () => {
    expect(boolOr(false, true)).toBe(false)
    expect(boolOr('true', false)).toBe(false)
    expect(boolOr(1, false)).toBe(false)
    expect(boolOr(undefined, true)).toBe(true)
  })

  it('numberIn clamps finite numbers and falls back on the rest', () => {
    expect(numberIn(0.6, { min: 0.2, max: 0.8, fallback: 0.5 })).toBe(0.6)
    expect(numberIn(40, { min: 0.2, max: 0.8, fallback: 0.5 })).toBe(0.8)
    expect(numberIn(-3, { min: 0.2, max: 0.8, fallback: 0.5 })).toBe(0.2)
    expect(numberIn(NaN, { min: 0.2, max: 0.8, fallback: 0.5 })).toBe(0.5)
    expect(numberIn(Infinity, { min: 0.2, max: 0.8, fallback: 0.5 })).toBe(0.5)
    expect(numberIn('0.6', { min: 0.2, max: 0.8, fallback: 0.5 })).toBe(0.5)
  })

  it('idOr requires a non-empty string', () => {
    expect(idOr('region-4', null)).toBe('region-4')
    expect(idOr('', null)).toBe(null)
    expect(idOr(12, null)).toBe(null)
  })

  it('idList filters, dedupes and caps', () => {
    expect(idList(['a', 'b', 'a', '', 3, null])).toEqual(['a', 'b'])
    expect(idList('not-a-list')).toEqual([])
    expect(idList(['a', 'b', 'c'], { max: 2 })).toEqual(['a', 'b'])
  })

  it('plainObject rejects arrays and null', () => {
    expect(plainObject({ a: 1 })).toEqual({ a: 1 })
    expect(plainObject([1])).toEqual({})
    expect(plainObject(null)).toEqual({})
    expect(plainObject('x')).toEqual({})
  })
})

describe('capEntries', () => {
  it('keeps the most recently inserted entries', () => {
    const map = { a: 1, b: 2, c: 3, d: 4 }
    expect(capEntries(map, 2)).toEqual({ c: 3, d: 4 })
    expect(capEntries(map, 10)).toBe(map)
  })
})

describe('sanitizeSession', () => {
  it('keeps a dragged tool panel width inside its bounds, and anything else as the panel\'s own', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({}).toolPanelWidth).toBeNull()
    expect(sanitizeSession({ toolPanelWidth: null }).toolPanelWidth).toBeNull()
    expect(sanitizeSession({ toolPanelWidth: '460' }).toolPanelWidth).toBeNull()
    expect(sanitizeSession({ toolPanelWidth: 460.4 }).toolPanelWidth).toBe(460)
    expect(sanitizeSession({ toolPanelWidth: 100 }).toolPanelWidth).toBe(360)
    expect(sanitizeSession({ toolPanelWidth: 9000 }).toolPanelWidth).toBe(560)
  })
  it('retains an explicit mixed cloud clean choice', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({ cleanLocalFirst: true }).cleanLocalFirst).toBe(true)
    expect(sanitizeSession({}).cleanLocalFirst).toBe(false)
    expect(sanitizeSession({ cleanLocalFirst: 'true' }).cleanLocalFirst).toBe(false)
  })
  it('retains Solid bubbles only, off for anything that is not a boolean', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({ cleanSolidOnly: true }).cleanSolidOnly).toBe(true)
    expect(sanitizeSession({}).cleanSolidOnly).toBe(false)
    expect(sanitizeSession({ cleanSolidOnly: 'true' }).cleanSolidOnly).toBe(false)
  })
  it('retains Clean English text, off for anything that is not a boolean', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({ cleanLatinText: true }).cleanLatinText).toBe(true)
    expect(sanitizeSession({}).cleanLatinText).toBe(false)
    expect(sanitizeSession({ cleanLatinText: 'true' }).cleanLatinText).toBe(false)
  })
  it('preserves sidecarPath from stored session', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    const session = sanitizeSession({ sidecarPath: '/path/to/flux' })
    expect(session.sidecarPath).toBe('/path/to/flux')
  })

  it('falls back to empty string when sidecarPath is missing or invalid', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({}).sidecarPath).toBe('')
    expect(sanitizeSession({ sidecarPath: 123 }).sidecarPath).toBe('')
  })

  it('preserves fluxModel from stored session', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    const session = sanitizeSession({ fluxModel: 'flux2-klein-4b' })
    expect(session.fluxModel).toBe('flux2-klein-4b')
  })

  it('falls back to empty string when fluxModel is missing or invalid', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({}).fluxModel).toBe('')
    expect(sanitizeSession({ fluxModel: 123 }).fluxModel).toBe('')
  })

  it('preserves fluxBackend, accepts the older key, and defaults to auto', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    expect(sanitizeSession({ fluxBackend: 'sdnq' }).fluxBackend).toBe('sdnq')
    expect(sanitizeSession({ fluxBackend: 'mflux' }).fluxBackend).toBe('mflux')
    // The spelling a settings file may still carry.
    expect(sanitizeSession({ sidecarBackend: 'sdnq' }).fluxBackend).toBe('sdnq')
    // Absent, cleared, wrong type, and a backend the row does not offer - every
    // one of them is `auto`, because a stale value must not make the rung
    // unreachable and `sdcpp` declines every open.
    expect(sanitizeSession({}).fluxBackend).toBe('auto')
    expect(sanitizeSession({ fluxBackend: '' }).fluxBackend).toBe('auto')
    expect(sanitizeSession({ fluxBackend: 42 }).fluxBackend).toBe('auto')
    expect(sanitizeSession({ fluxBackend: 'sdcpp' }).fluxBackend).toBe('auto')
  })

  // The tool bar has no height and cannot be folded, but a record written
  // while it was still the tool window carries both - and a restored `fold`
  // would collapse a bar that has no body to collapse and no unfold control
  // on it, which is a window the user cannot get back.
  it('drops the height and the fold a content-sized window cannot have', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    const stored = sanitizeSession({
      windows: {
        tool: { x: 300, y: 62, w: 420, h: 380, open: true, fold: true },
        pages: { x: 16, y: 62, w: 248, h: 380, open: true, fold: true },
      },
    })
    expect(stored.windows.tool.h).toBeNull()
    expect(stored.windows.tool.fold).toBe(false)
    // What it *does* keep: where it was, and the width the clamp needs.
    expect(stored.windows.tool.x).toBe(300)
    expect(stored.windows.tool.w).toBe(420)
    // Every other window keeps both.
    expect(stored.windows.pages.h).toBe(380)
    expect(stored.windows.pages.fold).toBe(true)
  })

  // The selection colour and opacity (Settings › General): the sky blue and
  // 35% until somebody picks otherwise, and only a colour or a number reads
  // back. A number where the colour should be is not one somebody picked.
  it('reads the selection colour and opacity, and defaults to sky blue at 35%', async () => {
    const { DEFAULT_MASK_COLOR, DEFAULT_MASK_OPACITY, sanitizeSession } = await import('./session.svelte.js')
    expect(DEFAULT_MASK_COLOR).toBe('#0284c7')
    expect(DEFAULT_MASK_OPACITY).toBe(35)
    expect(sanitizeSession({})).toMatchObject({ maskColor: '#0284c7', maskOpacity: 35 })
    expect(sanitizeSession({ maskColor: '#FF00AA', maskOpacity: 60 })).toMatchObject({ maskColor: '#ff00aa', maskOpacity: 60 })
    expect(sanitizeSession({ maskColor: 'abc' }).maskColor).toBe('#aabbcc')
    for (const junk of ['red', '#12345', 123456, null, {}]) {
      expect(sanitizeSession({ maskColor: junk }).maskColor, String(junk)).toBe('#0284c7')
    }
    // Clamped into range and rounded like every stored number; not a number is the default.
    expect(sanitizeSession({ maskOpacity: 140 }).maskOpacity).toBe(100)
    expect(sanitizeSession({ maskOpacity: -5 }).maskOpacity).toBe(0)
    expect(sanitizeSession({ maskOpacity: 42.6 }).maskOpacity).toBe(43)
    expect(sanitizeSession({ maskOpacity: '50' }).maskOpacity).toBe(35)
  })

  it('sets the selection colour and opacity, keeping the old value for junk', async () => {
    const { session, setMaskColor, setMaskOpacity } = await import('./session.svelte.js')
    const before = { color: session.maskColor, opacity: session.maskOpacity }
    try {
      setMaskColor('#22AA44')
      expect(session.maskColor).toBe('#22aa44')
      setMaskColor('not a colour')
      expect(session.maskColor).toBe('#22aa44')
      setMaskOpacity(55)
      expect(session.maskOpacity).toBe(55)
      setMaskOpacity(/** @type {any} */ (Number.NaN))
      expect(session.maskOpacity).toBe(55)
    } finally {
      setMaskColor(before.color)
      setMaskOpacity(before.opacity)
    }
  })

  // Two colours since the split: speech bubble text keeps `maskColor`, the
  // key the single colour was saved under, and text outside bubbles gets
  // `outsideMaskColor`, burnt orange until somebody picks otherwise.
  it('reads the outside colour, and carries a saved single colour over as the speech bubble one', async () => {
    const { DEFAULT_OUTSIDE_MASK_COLOR, sanitizeSession } = await import('./session.svelte.js')
    expect(DEFAULT_OUTSIDE_MASK_COLOR).toBe('#c2410c')
    // A record from before the split, and one with neither key.
    expect(sanitizeSession({ maskColor: '#22AA44', maskOpacity: 60 })).toMatchObject({
      maskColor: '#22aa44',
      outsideMaskColor: '#c2410c',
      maskOpacity: 60,
    })
    expect(sanitizeSession({})).toMatchObject({ maskColor: '#0284c7', outsideMaskColor: '#c2410c' })
    expect(sanitizeSession({ maskColor: '#112233', outsideMaskColor: '#FFaa00' })).toMatchObject({
      maskColor: '#112233',
      outsideMaskColor: '#ffaa00',
    })
    for (const junk of ['orange', '#12345', 123456, null, {}]) {
      expect(sanitizeSession({ outsideMaskColor: junk }).outsideMaskColor, String(junk)).toBe('#c2410c')
    }
  })

  it('sets the outside colour on its own, keeping the old value for junk', async () => {
    const { session, setOutsideMaskColor } = await import('./session.svelte.js')
    const before = { inside: session.maskColor, outside: session.outsideMaskColor }
    try {
      setOutsideMaskColor('#AA2244')
      expect(session.outsideMaskColor).toBe('#aa2244')
      expect(session.maskColor).toBe(before.inside)
      setOutsideMaskColor('not a colour')
      expect(session.outsideMaskColor).toBe('#aa2244')
    } finally {
      setOutsideMaskColor(before.outside)
    }
  })

  // Both defaults survive black ink and white paper: 4:1 or better against
  // each, as the outline has to be seen over either.
  it('keeps both default colours legible on white paper and black ink', async () => {
    const { DEFAULT_MASK_COLOR, DEFAULT_OUTSIDE_MASK_COLOR } = await import('./session.svelte.js')
    const luminance = (/** @type {string} */ hex) => {
      const [r, g, b] = [1, 3, 5]
        .map((at) => parseInt(hex.slice(at, at + 2), 16) / 255)
        .map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4))
      return 0.2126 * r + 0.7152 * g + 0.0722 * b
    }
    const contrast = (/** @type {string} */ a, /** @type {string} */ b) => {
      const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
      return (hi + 0.05) / (lo + 0.05)
    }
    for (const color of [DEFAULT_MASK_COLOR, DEFAULT_OUTSIDE_MASK_COLOR]) {
      expect(contrast(color, '#ffffff'), `${color} on white`).toBeGreaterThanOrEqual(4)
      expect(contrast(color, '#000000'), `${color} on black`).toBeGreaterThanOrEqual(4)
    }
  })

  it('carries fluxBackend down to the backend settings patch', async () => {
    const { backendSettingsPatch, setFluxBackend } = await import('./session.svelte.js')
    setFluxBackend('sdnq')
    expect(backendSettingsPatch().fluxBackend).toBe('sdnq')
    setFluxBackend('auto')
    expect(backendSettingsPatch().fluxBackend).toBe('auto')
  })

  it('carries the FLUX 3 working size down to the backend, one of the measured sizes', async () => {
    const { backendSettingsPatch, setCloudWorkLongSide, adoptBackendSettings } = await import('./session.svelte.js')
    expect(backendSettingsPatch().cloudWorkLongSide).toBe(768)
    setCloudWorkLongSide(1280)
    expect(backendSettingsPatch().cloudWorkLongSide).toBe(1280)
    for (const bad of [1100, '1024', null]) setCloudWorkLongSide(bad)
    expect(backendSettingsPatch().cloudWorkLongSide).toBe(1280)
    adoptBackendSettings({ cloudWorkLongSide: 1024 })
    expect(backendSettingsPatch().cloudWorkLongSide).toBe(1024)
    setCloudWorkLongSide(768)
  })

  it('keeps the finishing and generation choices, and the defaults for anything else', async () => {
    const { sanitizeSession } = await import('./session.svelte.js')
    const off = { referencePrompt: false, marker: false, referenceShift: false, megapixelSize: false }
    const finishing = (/** @type {any} */ read) => [read.cloudFinishing, read.cloudFinishingGrayscale, read.finishingEveryModel]
    expect(finishing(sanitizeSession({}))).toEqual(['copy', false, false])
    expect(sanitizeSession({}).cloudFlux3Generation).toEqual(off)
    const kept = sanitizeSession({ cloudFinishing: 'reference-raster', cloudFinishingGrayscale: true, finishingEveryModel: true,
      cloudFlux3Generation: { marker: true, megapixelSize: true } })
    expect(finishing(kept)).toEqual(['reference-raster', true, true])
    expect(kept.cloudFlux3Generation).toEqual({ ...off, marker: true, megapixelSize: true })
    for (const bad of [1, 'true', null, {}, ['copy']]) {
      const read = sanitizeSession({ cloudFinishing: bad, cloudFinishingGrayscale: bad, finishingEveryModel: bad, cloudFlux3Generation: bad })
      expect(finishing(read), JSON.stringify(bad)).toEqual(['copy', false, false])
      expect(read.cloudFlux3Generation).toEqual(off)
    }
    expect(sanitizeSession({ cloudFinishing: 'reference' }).cloudFinishing).toBe('copy')
    expect(sanitizeSession({ cloudFlux3Generation: { marker: 1, referenceShift: 'true', other: true } }).cloudFlux3Generation).toEqual(off)
  })

  it('stores the finishing and generation choices, reads them back, and sends them to the backend', async () => {
    const store = fakeStorage()
    vi.stubGlobal('localStorage', store)
    const { backendSettingsPatch, sanitizeSession, setCloudFinishing, setCloudFlux3Generation } = await import('./session.svelte.js')
    try {
      setCloudFinishing('redraw', true, true)
      setCloudFlux3Generation({ referencePrompt: true, marker: false, referenceShift: true, megapixelSize: false })
      const read = sanitizeSession(JSON.parse(store.map.get(`${STORAGE_PREFIX}session.v1`)))
      const wanted = {
        cloudFinishing: 'redraw', cloudFinishingGrayscale: true, finishingEveryModel: true,
        cloudFlux3Generation: { referencePrompt: true, marker: false, referenceShift: true, megapixelSize: false },
      }
      expect(read).toMatchObject(wanted)
      expect(backendSettingsPatch()).toMatchObject(wanted)
    } finally {
      setCloudFinishing('copy', false, false)
      setCloudFlux3Generation(null)
      vi.unstubAllGlobals()
    }
  })

  it('adopts one finishing key from the backend and keeps the other two', async () => {
    const { adoptBackendSettings, session, setCloudFinishing, setCloudFlux3Generation } = await import('./session.svelte.js')
    const finishing = () => [session.cloudFinishing, session.cloudFinishingGrayscale, session.finishingEveryModel]
    try {
      setCloudFinishing('tone-only', true, false)
      adoptBackendSettings({ finishingEveryModel: true })
      expect(finishing()).toEqual(['tone-only', true, true])
      adoptBackendSettings({ cloudFinishing: 'reference-local' })
      expect(finishing()).toEqual(['reference-local', true, true])
      adoptBackendSettings({ finishingEveryModel: 1, cloudFinishingGrayscale: false })
      expect(finishing()).toEqual(['reference-local', false, true])
      adoptBackendSettings({ cloudFlux3Generation: { marker: true } })
      expect(session.cloudFlux3Generation).toEqual({ referencePrompt: false, marker: true, referenceShift: false, megapixelSize: false })
    } finally {
      setCloudFinishing('copy', false, false)
      setCloudFlux3Generation(null)
    }
  })

  it('carries fluxModel down to the backend settings patch', async () => {
    const { backendSettingsPatch, setFluxModel } = await import('./session.svelte.js')
    setFluxModel('flux2-klein-4b')
    expect(backendSettingsPatch().fluxModel).toBe('flux2-klein-4b')
  })

  it('sanitizes an old tool record from a large display and clamps it to viewport', async () => {
    const { sanitizeSession, clampWindowsToViewport, session } = await import('./session.svelte.js')
    const stored = sanitizeSession({
      windows: {
        tool: { x: 2178, y: 62, w: 306, h: 420, open: true, fold: true },
      },
    })
    expect(stored.windows.tool.h).toBeNull()
    expect(stored.windows.tool.fold).toBe(false)
    expect(stored.windows.tool.w).toBe(306)
    expect(stored.windows.tool.x).toBe(2178)

    session.windows.tool = { ...stored.windows.tool }
    clampWindowsToViewport(1400, 900)
    // 1400 - 306 = 1094: pulled fully onto the viewport
    expect(session.windows.tool.x).toBe(1094)
  })

  it('resets a content-sized window to top-centre using its measured width', async () => {
    const { resetWindowBox, session } = await import('./session.svelte.js')
    session.windows.tool.w = 340
    session.windows.tool.x = 20
    session.windows.tool.y = 200
    // viewport() in node defaults to 1400 x 900
    resetWindowBox('tool')
    // 1400 / 2 - 340 / 2 = 530 (not 1400 / 2 - 280 = 420)
    expect(session.windows.tool.x).toBe(530)
    expect(session.windows.tool.y).toBe(62)

    // Resizable window still uses defaultGeometry
    session.windows.pages.x = 500
    resetWindowBox('pages')
    expect(session.windows.pages.x).toBe(16)
    expect(session.windows.pages.y).toBe(62)
  })
})
