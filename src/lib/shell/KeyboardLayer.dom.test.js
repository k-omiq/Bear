import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'

const state = vi.hoisted(() => ({ region: /** @type {any} */ (null), parts: { detect: false, clean: false } }))

vi.mock('../state/editor.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return { ...actual, selectedRegion: vi.fn(() => state.region) }
})

vi.mock('../editor/maskactions.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return {
    ...actual,
    runMaskAction: vi.fn(async () => true),
    runRegionMenuItem: vi.fn(async () => true),
    stepLayerOpacity: vi.fn(),
  }
})

vi.mock('../editor/retouch.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return { ...actual, applySelection: vi.fn(async () => true) }
})

vi.mock('../editor/cloudrun.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return { ...actual, cloudParts: vi.fn(() => state.parts), runStep: vi.fn(() => 'auto') }
})

import KeyboardLayer from './KeyboardLayer.svelte'
import { app } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { draft } from '../editor/draft.svelte.js'
import { applySelection } from '../editor/retouch.svelte.js'
import { runMaskAction, runRegionMenuItem, stepLayerOpacity } from '../editor/maskactions.svelte.js'

/** @param {string} key @param {Object} [extra] */
function press(key, extra = {}) {
  window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...extra }))
}

/** @type {any} */
let route
beforeEach(() => {
  vi.clearAllMocks()
  route = app.route
  app.route = /** @type {any} */ ({ ...app.route, name: 'editor' })
  state.region = null
  state.parts = { detect: false, clean: false }
  render(KeyboardLayer)
})

afterEach(() => {
  cleanup()
  app.route = route
  draft.selection = null
  document.body.innerHTML = ''
})

describe('the keyboard layer', () => {
  it('opens Text cleanup on the first V, then steps its scope, with Project only while all of it runs here', () => {
    editor.tool = 'brush'
    editor.toolParams.autoClean = { ...(editor.toolParams.autoClean ?? {}), scope: 'page' }
    press('v')
    expect(editor.tool).toBe('autoClean')
    expect(editor.toolParams.autoClean.scope).toBe('page')
    press('v')
    expect(editor.toolParams.autoClean.scope).toBe('chapter')
    press('v')
    expect(editor.toolParams.autoClean.scope).toBe('project')
    press('V', { shiftKey: true })
    expect(editor.toolParams.autoClean.scope).toBe('chapter')

    state.parts = { detect: true, clean: false }
    press('v')
    expect(editor.toolParams.autoClean.scope).toBe('page')
  })

  it('cleans a selected detection on B, tries a cleaned one again, and widens it on Shift+B', () => {
    press('b')
    expect(runMaskAction).not.toHaveBeenCalled()
    state.region = { id: 'r1', outcome: 'detected', detected: true }
    press('b')
    expect(runMaskAction).toHaveBeenCalledWith('cleanDetected', state.region)
    state.region = { id: 'r2', outcome: 'cleaned', mask: { id: 'm2' } }
    press('b')
    expect(runRegionMenuItem).toHaveBeenLastCalledWith('retry', state.region)
    press('B', { shiftKey: true })
    expect(runRegionMenuItem).toHaveBeenLastCalledWith('retryWider', state.region)
  })

  it('applies a held selection of the tool that is out on Cmd+Enter, before anything else', () => {
    editor.tool = 'patch'
    draft.selection = /** @type {any} */ ({ tool: 'patch', pageId: 'p1' })
    press('Enter', { metaKey: true })
    expect(applySelection).toHaveBeenCalledTimes(1)
  })

  it('presses the panel\'s run button on Cmd+Enter only while it is enabled, and opens Text cleanup without one', () => {
    editor.tool = 'brush'
    press('Enter', { metaKey: true })
    expect(editor.tool).toBe('autoClean')

    const button = document.createElement('button')
    button.dataset.runAction = ''
    const click = vi.fn()
    button.addEventListener('click', click)
    document.body.append(button)
    button.disabled = true
    press('Enter', { metaKey: true })
    expect(click).not.toHaveBeenCalled()
    button.disabled = false
    press('Enter', { metaKey: true })
    expect(click).toHaveBeenCalledTimes(1)
  })

  it('moves focus through the tool bar\'s sliders, dropdowns and boxes on \\, round to the first', () => {
    const bar = document.createElement('section')
    bar.dataset.shell = 'panel'
    bar.innerHTML = '<div data-range-field><input type="range" id="a"></div><select id="b"><option>x</option></select>'
      + '<input type="checkbox" id="c" disabled><input type="checkbox" id="d">'
    document.body.append(bar)
    press('\\')
    expect(document.activeElement?.id).toBe('a')
    press('\\')
    expect(document.activeElement?.id).toBe('b')
    press('\\')
    expect(document.activeElement?.id).toBe('d')
    press('\\')
    expect(document.activeElement?.id).toBe('a')
  })

  it('steps a region copy\'s own opacity on I while one is being placed, and the tool\'s otherwise', () => {
    editor.tool = 'cloneStamp'
    editor.toolParams.cloneStamp = { ...(editor.toolParams.cloneStamp ?? {}), mode: 'region', opacity: 100 }
    state.region = { id: 'copy1', outcome: 'cleaned', mask: { id: 'm', provenance: { engine: 'clone', params_snapshot: { mode: 'region-copy' } } } }
    press('i')
    expect(stepLayerOpacity).toHaveBeenCalledWith(state.region, -1)
    expect(editor.toolParams.cloneStamp.opacity).toBe(100)
    state.region = null
    editor.tool = 'brush'
    editor.toolParams.brush = { ...(editor.toolParams.brush ?? {}), opacity: 100 }
    press('i')
    expect(editor.toolParams.brush.opacity).toBe(90)
  })
})
