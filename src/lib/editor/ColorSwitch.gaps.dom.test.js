/**
 * Gaps found by trying to break the saved colour pair (Shift+X, Shift+E) and
 * Shift+B. Each test states the behaviour that should hold and fails today, so
 * each is skipped with its reason in the title. Remove the `.skip` with the fix.
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({ region: /** @type {any} */ (null) }))

vi.mock('../state/editor.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return { ...actual, selectedRegion: vi.fn(() => state.region) }
})

vi.mock('./maskactions.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return { ...actual, runRegionMenuItem: vi.fn(actual.runRegionMenuItem) }
})

import { t } from '../i18n/index.js'
import { app, clearNotices } from '../state/app.svelte.js'
import { editor, setActiveColor } from '../state/editor.svelte.js'
import { sanitizeSession, session } from '../state/session.svelte.js'
import { matchShortcut, setShortcutOverrides } from '../shortcuts.js'
import KeyboardLayer from '../shell/KeyboardLayer.svelte'
import { draft } from './draft.svelte.js'
import { runRegionMenuItem } from './maskactions.svelte.js'
import RetouchOptions from './RetouchOptions.svelte'
import ToolRail from './ToolRail.svelte'

/** @param {string} key @param {Object} [extra] */
function press(key, extra = {}) {
  window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...extra }))
}

const settled = () => new Promise((resolve) => setTimeout(resolve))
const activeSwatch = () => screen.getByRole('button', { name: new RegExp(t('tools.color.pairActive')) })
const otherSwatch = () => screen.getByRole('button', { name: new RegExp(t('tools.color.pairSwap')) })

/** A sampler that never answers, as a person still choosing a pixel. */
function stubSampler() {
  const open = vi.fn(() => new Promise(() => {}))
  vi.stubGlobal('EyeDropper', class { open = open })
  return open
}

/** @type {any} */
let route

beforeEach(() => {
  vi.clearAllMocks()
  route = app.route
  app.route = /** @type {any} */ ({ ...app.route, name: 'editor' })
  state.region = null
  session.colorPair = { active: '#ffffff', other: '#000000' }
  setActiveColor('#ffffff')
  clearNotices()
})

afterEach(() => {
  cleanup()
  app.route = route
  draft.active = null
  session.colorPair = { active: '#ffffff', other: '#000000' }
  setActiveColor('#ffffff')
  setShortcutOverrides({})
  clearNotices()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})

describe('a key pressed during a drag', () => {
  beforeEach(() => {
    render(KeyboardLayer)
    draft.active = /** @type {any} */ ({ kind: 'stroke', tool: 'brush', pageId: 'p1', points: [{ x: 1, y: 1 }] })
  })

  it.skip('BUG: Shift+E opens the sampler over a stroke still being drawn, which takes the pointer away from it', async () => {
    const open = stubSampler()
    press('E', { shiftKey: true })
    await settled()
    expect(open).not.toHaveBeenCalled()
  })

  it.skip('BUG: Shift+B starts a re-render during a stroke, and its undo step lands before the stroke\'s', () => {
    state.region = { id: 'r1', outcome: 'cleaned', mask: { id: 'm1', provenance: { engine: 'lama' } } }
    press('B', { shiftKey: true })
    expect(runRegionMenuItem).not.toHaveBeenCalled()
  })
})

describe('Shift+E', () => {
  it.skip('BUG: opens a second sampler while the picker\'s own eyedropper is open, as the two guards are separate', async () => {
    const open = stubSampler()
    render(KeyboardLayer)
    render(ToolRail)
    await fireEvent.click(activeSwatch())
    await settled()
    await fireEvent.click(screen.getByRole('button', { name: t('tools.action.eyedropper') }))
    press('E', { shiftKey: true })
    await settled()
    expect(open).toHaveBeenCalledTimes(1)
  })
})

describe('Shift+B on a layer it cannot widen', () => {
  it.skip('BUG: does nothing and says nothing, where the Layers row says why it is off', async () => {
    render(KeyboardLayer)
    state.region = { id: 'r1', outcome: 'cleaned', mask: { id: 'm1', provenance: { engine: 'paint' } } }
    press('B', { shiftKey: true })
    await settled()
    expect(app.notices.length).toBeGreaterThan(0)
  })
})

describe('the other colours in the editor', () => {
  it.skip('BUG: the Gradient\'s Foreground to background ignores the saved other colour and picks black or white', async () => {
    session.colorPair = { active: '#ff0000', other: '#00ff00' }
    setActiveColor('#ff0000')
    const { colors, kind } = editor.toolParams.gradient
    editor.toolParams.gradient.kind = 'linear'
    render(RetouchOptions, { tool: 'gradient', panel: true })
    try {
      await fireEvent.click(screen.getByRole('button', { name: t('retouch.gradient.stops') }))
      await fireEvent.click(screen.getByRole('button', { name: t('retouch.gradient.toBackground') }))
      expect(editor.toolParams.gradient.colors.map((/** @type {any} */ stop) => stop.color)).toEqual(['#ff0000', '#00ff00'])
    } finally {
      editor.toolParams.gradient.colors = colors
      editor.toolParams.gradient.kind = kind
    }
  })
})

describe('what the swatches show and say', () => {
  beforeEach(() => {
    render(ToolRail)
  })

  it.skip('BUG: the front swatch draws a colour rounded through HSB, not the one the tools paint with', async () => {
    setActiveColor('#123456')
    await settled()
    expect(activeSwatch().style.backgroundColor).toBe('rgb(18, 52, 86)')
  })

  it.skip('BUG: neither swatch tells a screen reader which colour it holds', async () => {
    session.colorPair = { active: '#ff0000', other: '#00ff00' }
    await settled()
    expect(activeSwatch().getAttribute('aria-label')).toContain('#ff0000')
    expect(otherSwatch().getAttribute('aria-label')).toContain('#00ff00')
  })

  it.skip('BUG: nothing on the front swatch names Shift+E, by tooltip or to a screen reader', () => {
    expect(activeSwatch().getAttribute('aria-keyshortcuts')).toBe('Shift+E')
    expect(activeSwatch().title).toContain('Shift+E')
  })

  it.skip('BUG: a rebound swap key on a Mac reaches aria-keyshortcuts as a glyph, where ARIA wants Meta', async () => {
    cleanup()
    vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('MacIntel')
    setShortcutOverrides({ 'tool.swapColors': { key: 'x', shift: true, accel: true } })
    render(ToolRail)
    expect(otherSwatch().getAttribute('aria-keyshortcuts')).toBe('Meta+Shift+X')
  })
})

describe('a saved record', () => {
  it.skip('BUG: a binding the user saved on Shift+E before this key existed stops working, with no word about it', () => {
    setShortcutOverrides({
      'app.export': { key: 'F2', shift: false, accel: false },
      'panel.tools': { key: 'e', shift: true, accel: false },
    })
    expect(matchShortcut({ key: 'E', shiftKey: true }, { scope: 'editor' })?.id).toBe('panel.tools')
  })

  it.skip('BUG: a pair with one colour lost can come back as the same colour twice, and then a swap does nothing', () => {
    const { active, other } = sanitizeSession({ colorPair: { active: '#000000', other: 'lost' } }).colorPair
    expect(active).toBe('#000000')
    expect(other).not.toBe(active)
  })
})
