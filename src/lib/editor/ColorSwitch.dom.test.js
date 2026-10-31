/**
 * The two saved colours at the end of the tool rail: the press and the keys
 * that swap them, what the Brush and the Shape paint with, and the record a
 * restart reads them back from. Both colours are put back after each test, as
 * the stores are shared with the rest of the file.
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { editor, setActiveColor, setTool, setToolParam, swapColors } from '../state/editor.svelte.js'
import { sanitizeSession, session } from '../state/session.svelte.js'
import KeyboardLayer from '../shell/KeyboardLayer.svelte'
import ToolRail from './ToolRail.svelte'

/** @param {string} key @param {Object} [extra] @param {EventTarget} [target] */
function press(key, extra = {}, target = window) {
  target.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...extra }))
}

/** What the session wrote last, as the next launch reads it. */
function storedRecord(/** @type {Map<string, string>} */ values) {
  const raw = [...values.entries()].filter(([key]) => key.startsWith('mangaCleaner.')).map(([, value]) => value).at(-1)
  return JSON.parse(String(raw))
}

/** @type {Map<string, string>} */
let stored
/** @type {any} */
let route

beforeEach(() => {
  stored = new Map()
  vi.stubGlobal('localStorage', {
    getItem: (/** @type {string} */ key) => stored.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => stored.set(key, value),
    removeItem: (/** @type {string} */ key) => stored.delete(key),
  })
  route = app.route
  app.route = /** @type {any} */ ({ ...app.route, name: 'editor' })
  setActiveColor('#ffffff')
  session.colorPair = { active: '#ffffff', other: '#000000' }
  render(KeyboardLayer)
  render(ToolRail)
})

afterEach(() => {
  cleanup()
  app.route = route
  session.colorPair = { active: '#ffffff', other: '#000000' }
  setActiveColor('#ffffff')
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

describe('the colour switch', () => {
  it('swaps on one press of the other swatch, and the Brush and the Shape follow', async () => {
    await fireEvent.click(screen.getByRole('button', { name: t('tools.color.pairSwap') }))
    expect(session.colorPair).toEqual({ active: '#000000', other: '#ffffff' })
    expect(editor.toolParams.brush.color).toBe('#000000')
    expect(editor.toolParams.shapes.color).toBe('#000000')
    // A second press comes back.
    swapColors()
    expect(session.colorPair).toEqual({ active: '#ffffff', other: '#000000' })
  })

  it('leaves Text cleanup\'s Solid colour and the Shape\'s outline alone', () => {
    const bubble = editor.toolParams.autoClean.bubbleColor
    const outline = editor.toolParams.shapes.outlineColor
    swapColors()
    expect(editor.toolParams.autoClean.bubbleColor).toBe(bubble)
    expect(editor.toolParams.shapes.outlineColor).toBe(outline)
  })

  it('takes a colour chosen on the Brush\'s or the Shape\'s own bar as the active one', () => {
    setToolParam('brush', 'color', '#123456')
    expect(session.colorPair).toEqual({ active: '#123456', other: '#000000' })
    expect(editor.toolParams.shapes.color).toBe('#123456')
    setToolParam('shapes', 'color', '#abcdef')
    expect(session.colorPair.active).toBe('#abcdef')
    expect(editor.toolParams.brush.color).toBe('#abcdef')
    // The Patch has a `color` row too, a number that is no colour of this pair.
    setToolParam('patch', 'color', 5)
    expect(session.colorPair.active).toBe('#abcdef')
  })

  it('keeps the colour in force when handed something that is not one', () => {
    setActiveColor('not a colour')
    expect(session.colorPair.active).toBe('#ffffff')
  })

  it('writes both colours where a restart reads them, and refuses a damaged record', () => {
    setActiveColor('#112233')
    swapColors()
    const reloaded = sanitizeSession(storedRecord(stored))
    expect(reloaded.colorPair).toEqual({ active: '#000000', other: '#112233' })
    expect(sanitizeSession({}).colorPair).toEqual({ active: '#ffffff', other: '#000000' })
    expect(sanitizeSession({ colorPair: { active: 'red', other: 7 } }).colorPair).toEqual({ active: '#ffffff', other: '#000000' })
  })

  it('opens the picker for the active colour from the front swatch, by label', async () => {
    const swatch = screen.getByRole('button', { name: t('tools.color.pairActive') })
    expect(swatch.getAttribute('aria-expanded')).toBe('false')
    await fireEvent.click(swatch)
    expect(swatch.getAttribute('aria-expanded')).toBe('true')
    expect(screen.getByRole('group', { name: t('tools.color.pairGroup') })).toBeTruthy()
  })
})

describe('the shortcuts', () => {
  it('swaps on Shift+X, and a bare X is still the selection\'s remove', () => {
    setTool('brush')
    press('X', { shiftKey: true })
    expect(session.colorPair).toEqual({ active: '#000000', other: '#ffffff' })
    expect(editor.tool).not.toBe('maskSelect')
    press('x')
    expect(session.colorPair).toEqual({ active: '#000000', other: '#ffffff' })
    expect(editor.tool).toBe('maskSelect')
  })

  it('fires neither key while a text field has the focus', () => {
    const open = vi.fn()
    vi.stubGlobal('EyeDropper', class { open = open })
    const field = document.body.appendChild(document.createElement('input'))
    press('X', { shiftKey: true }, field)
    press('E', { shiftKey: true }, field)
    expect(session.colorPair).toEqual({ active: '#ffffff', other: '#000000' })
    expect(open).not.toHaveBeenCalled()
  })

  it('samples the screen into the active colour on Shift+E, once while the sampler is open', async () => {
    /** @type {(value: {sRGBHex: string}) => void} */
    let finish = () => {}
    const open = vi.fn(() => new Promise((resolve) => { finish = resolve }))
    vi.stubGlobal('EyeDropper', class { open = open })
    press('E', { shiftKey: true })
    press('E', { shiftKey: true })
    expect(open).toHaveBeenCalledTimes(1)
    finish({ sRGBHex: '#FF8800' })
    await vi.waitFor(() => expect(session.colorPair.active).toBe('#ff8800'))
    expect(editor.toolParams.brush.color).toBe('#ff8800')
    expect(session.colorPair.other).toBe('#000000')
  })

  it('leaves the colour when the sampler is cancelled, and can sample again', async () => {
    const open = vi.fn(async () => { throw new DOMException('cancelled', 'AbortError') })
    vi.stubGlobal('EyeDropper', class { open = open })
    press('E', { shiftKey: true })
    await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(1))
    await Promise.resolve()
    await Promise.resolve()
    expect(session.colorPair.active).toBe('#ffffff')
    press('E', { shiftKey: true })
    await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(2))
  })

  it('opens the active colour\'s picker on Shift+E where the platform has no sampler', async () => {
    const swatch = screen.getByRole('button', { name: t('tools.color.pairActive') })
    press('E', { shiftKey: true })
    await vi.waitFor(() => expect(swatch.getAttribute('aria-expanded')).toBe('true'))
  })
})
