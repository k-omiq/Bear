import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { tick } from 'svelte'
import { setBackend } from '../api/backend.js'
import { editor } from '../state/editor.svelte.js'
import { app } from '../state/app.svelte.js'
import { session } from '../state/session.svelte.js'
import CanvasStage from './CanvasStage.svelte'
import KeyboardLayer from '../shell/KeyboardLayer.svelte'
import { NAVIGATION_PRESETS, defaultNavigation } from './navigation.js'

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  setBackend(null)
  editor.project = null
  editor.chapter = null
  editor.fit = true
  editor.zoom = 1
  editor.panHeld = false
  session.navigation = defaultNavigation()
  app.modals.length = 0
  document.body.replaceChildren()
})

/**
 * One zoomed page in a scroller that can move both ways, with a control (a
 * region's button) and some empty canvas inside it to press on.
 */
async function fixture() {
  const pages = [{ id: 'p0', index: 0, number: 1, width: 1000, height: 1400, status: 'unclean', regions: [], resident: true }]
  setBackend(/** @type {any} */ ({ loadPages: vi.fn(async () => pages) }))
  editor.project = /** @type {any} */ ({ id: 'pan-project', mode: 'single' })
  editor.chapter = /** @type {any} */ ({ id: 'pan-chapter', pages, review: [] })
  editor.pageIndex = 0
  editor.loading = false
  editor.fit = false
  editor.zoom = 2
  app.route = /** @type {any} */ ({ name: 'editor', projectId: 'pan-project', chapterId: 'pan-chapter' })
  const box = document.createElement('div')
  box.setAttribute('data-canvas-viewport', '')
  document.body.append(box)
  Object.defineProperties(box, {
    clientWidth: { configurable: true, value: 1200 },
    clientHeight: { configurable: true, value: 600 },
  })
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => (
    /** @type {DOMRect} */ ({ top: 0, left: 0, width: 2000, height: 2800 })
  ))
  render(CanvasStage, { target: box })
  render(KeyboardLayer)
  await tick()
  box.scrollLeft = 500
  box.scrollTop = 500
  const control = document.createElement('button')
  const empty = document.createElement('div')
  // `RegionLayer`'s clear-selection target: a button, and empty canvas.
  const clear = document.createElement('button')
  clear.setAttribute('data-canvas-empty', '')
  const stage = /** @type {HTMLElement} */ (box.querySelector('.stage'))
  stage.append(control, empty, clear)
  const pressed = vi.fn()
  const clicked = vi.fn()
  const cleared = vi.fn()
  control.addEventListener('pointerdown', pressed)
  control.addEventListener('click', clicked)
  clear.addEventListener('click', cleared)
  return { box, control, empty, clear, pressed, clicked, cleared }
}

/**
 * Press, move by (dx, dy), release.
 *
 * @param {Element} target
 * @param {number} button
 * @param {number} dx
 * @param {number} dy
 */
async function drag(target, button, dx, dy) {
  const down = await fireEvent.pointerDown(target, { button, pointerId: 1, clientX: 100, clientY: 100 })
  await fireEvent.pointerMove(target, { pointerId: 1, clientX: 100 + dx, clientY: 100 + dy })
  await fireEvent.pointerUp(target, { button, pointerId: 1, clientX: 100 + dx, clientY: 100 + dy })
  if (button === 0) await fireEvent.click(target, { button })
  return down
}

describe('the hand', () => {
  it('pans on a middle drag, before the control under it sees the press', async () => {
    const { box, control, pressed } = await fixture()
    const notDefault = await drag(control, 1, -40, -30)
    expect(notDefault).toBe(false)
    expect(pressed).not.toHaveBeenCalled()
    expect(box.scrollLeft).toBe(540)
    expect(box.scrollTop).toBe(530)
  })

  it('leaves the middle button alone when it is turned off', async () => {
    const { box, control, pressed } = await fixture()
    session.navigation = { ...session.navigation, middlePan: false }
    await drag(control, 1, -40, -30)
    expect(pressed).toHaveBeenCalled()
    expect(box.scrollLeft).toBe(500)
  })

  it('pans a left drag while Space is held, and gives the release back', async () => {
    const { box, control, pressed, clicked } = await fixture()
    await drag(control, 0, 20, 10)
    expect(box.scrollLeft).toBe(500)
    expect(pressed).toHaveBeenCalledTimes(1)
    expect(clicked).toHaveBeenCalledTimes(1)

    await fireEvent.keyDown(window, { key: ' ' })
    expect(editor.panHeld).toBe(true)
    expect(box.dataset.pan).toBe('grab')
    await drag(control, 0, 20, 10)
    expect(pressed).toHaveBeenCalledTimes(1)
    expect(clicked).toHaveBeenCalledTimes(1)
    expect(box.scrollLeft).toBe(480)
    expect(box.scrollTop).toBe(490)

    const up = new KeyboardEvent('keyup', { key: ' ', cancelable: true })
    window.dispatchEvent(up)
    expect(up.defaultPrevented).toBe(true)
    expect(editor.panHeld).toBe(false)
    await waitFor(() => expect(box.dataset.pan).toBeUndefined())
  })

  it('pans a left drag on empty canvas with the mouse preset, never one on a control', async () => {
    const { box, control, empty } = await fixture()
    await drag(empty, 0, 20, 10)
    expect(box.scrollLeft).toBe(500)
    session.navigation = { ...NAVIGATION_PRESETS.mouse }
    await drag(empty, 0, 20, 10)
    expect(box.scrollLeft).toBe(480)
    await drag(control, 0, 20, 10)
    expect(box.scrollLeft).toBe(480)
  })

  it('pans a drag on the page\'s empty space, and clears the selection only on a click', async () => {
    const { box, clear, cleared } = await fixture()
    session.navigation = { ...NAVIGATION_PRESETS.mouse }
    await drag(clear, 0, 30, 0)
    expect(box.scrollLeft).toBe(470)
    expect(cleared).not.toHaveBeenCalled()
    await drag(clear, 0, 1, 0)
    expect(cleared).toHaveBeenCalledTimes(1)
  })

  it('swallows no click after a middle drag', async () => {
    const { control, clicked } = await fixture()
    await drag(control, 1, 30, 0)
    await fireEvent.pointerDown(control, { button: 0, pointerId: 1 })
    await fireEvent.pointerUp(control, { button: 0, pointerId: 1 })
    await fireEvent.click(control, { button: 0 })
    expect(clicked).toHaveBeenCalledTimes(1)
  })

  it('zooms on a plain wheel with the mouse preset, and scrolls with Shift', async () => {
    const { box } = await fixture()
    const plain = new WheelEvent('wheel', { deltaY: 50, cancelable: true })
    box.dispatchEvent(plain)
    expect(plain.defaultPrevented).toBe(false)

    session.navigation = { ...NAVIGATION_PRESETS.mouse }
    const zoom = new WheelEvent('wheel', { deltaY: -50, cancelable: true })
    box.dispatchEvent(zoom)
    expect(zoom.defaultPrevented).toBe(true)
    await waitFor(() => expect(editor.zoom).toBeGreaterThan(2))

    const shifted = new WheelEvent('wheel', { deltaY: -50, shiftKey: true, cancelable: true })
    box.dispatchEvent(shifted)
    expect(shifted.defaultPrevented).toBe(false)
  })
})
