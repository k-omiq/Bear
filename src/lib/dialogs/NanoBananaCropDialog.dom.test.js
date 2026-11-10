/**
 * The crop window, mounted through the modal host and opened by
 * `editor/nanobananacrop.svelte.js` for a `nano-banana-crop` event: Send
 * answers the suggested areas (`keep: null`) unless areas were drawn, a drag
 * on the picture draws one in crop pixels, a slip of the hand draws nothing,
 * Skip, Stop and Escape answer as they say, and a refused answer (the
 * request already ended) only closes the window.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { showCrop, startNanoBananaCrops, stopNanoBananaCrops } from '../editor/nanobananacrop.svelte.js'
import ModalHost from '../shell/ModalHost.svelte'

afterEach(() => {
  cleanup()
  stopNanoBananaCrops()
  app.modals.length = 0
  vi.restoreAllMocks()
})

/** @param {Record<string, any>} [overrides] */
function cropEvent(overrides = {}) {
  return {
    type: 'nano-banana-crop',
    id: 'crop-1',
    model: 'gemini-3.1-flash-image',
    ratio: '1:1',
    size: '512',
    crop: [100, 200, 400, 400],
    attempt: 0,
    restrictive: false,
    holes: [[150, 150, 100, 100]],
    suggested: [],
    png: 'iVBORw0KGgo=',
    ...overrides,
  }
}

/** @param {Record<string, any>} [overrides] */
async function open(overrides = {}) {
  const backend = { answerNanoBananaCrop: vi.fn(async () => {}) }
  render(ModalHost)
  const event = cropEvent(overrides)
  const answered = showCrop(/** @type {any} */ (event), /** @type {any} */ (backend))
  await screen.findByRole('dialog')
  return { backend, answered }
}

/** The picture's surface, laid out at 400 by 400 screen pixels from (10, 20). */
function stage() {
  const surface = /** @type {HTMLElement} */ (document.querySelector('[data-stage]'))
  surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({ left: 10, top: 20, width: 400, height: 400, right: 410, bottom: 420, x: 10, y: 20, toJSON() {} })
  return surface
}

/** @param {HTMLElement} surface @param {number[]} from @param {number[]} to */
async function drag(surface, from, to) {
  await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: from[0], clientY: from[1] })
  await fireEvent.pointerMove(surface, { pointerId: 1, clientX: to[0], clientY: to[1] })
  await fireEvent.pointerUp(surface, { pointerId: 1, clientX: to[0], clientY: to[1] })
}

const button = (/** @type {string} */ key) => screen.getByRole('button', { name: t(key) })

describe('the Nano Banana crop window', () => {
  it('states the model, size and ratio, shows the crop, and Send answers the suggested areas', async () => {
    const { backend, answered } = await open()
    expect(screen.getByText(t('nanoBanana.crop.model', { model: t('nanoBanana.model.v2'), size: '512', ratio: '1:1' }))).toBeTruthy()
    const picture = /** @type {HTMLImageElement} */ (screen.getByRole('img'))
    expect(picture.getAttribute('src')).toBe('data:image/png;base64,iVBORw0KGgo=')
    expect(picture.alt).toBe(t('nanoBanana.crop.alt'))
    expect(document.querySelector('[data-attempt]')).toBeNull()
    // Not restrictive: no white, nothing to draw.
    expect(document.querySelector('[data-hidden]')).toBeNull()
    await fireEvent.click(button('nanoBanana.crop.send'))
    await answered
    expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer: 'send', keep: null })
    expect(app.modals).toEqual([])
  })

  it('says which attempt it is when Google refused an earlier crop', async () => {
    await open({ attempt: 1 })
    expect(screen.getByText(t('nanoBanana.crop.attempt', { attempt: 2 }))).toBeTruthy()
  })

  it('sends keep null for the suggested areas while nothing is drawn', async () => {
    const { backend, answered } = await open({ restrictive: true, suggested: [[126, 126, 148, 148]] })
    expect(document.querySelector('[data-hidden]')).toBeTruthy()
    expect(document.querySelectorAll('[data-kept]')).toHaveLength(1)
    expect(document.querySelector('[data-areas]')?.textContent).toBe(t('nanoBanana.crop.suggestedCount', { count: 1 }))
    await fireEvent.click(button('nanoBanana.crop.send'))
    await answered
    expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer: 'send', keep: null })
  })

  it('draws an area with a drag, in crop pixels, and sends it', async () => {
    const { backend, answered } = await open({ restrictive: true, suggested: [[126, 126, 148, 148]] })
    const surface = stage()
    // The 400 px crop is drawn at 400 screen pixels, from (10, 20).
    await drag(surface, [20, 40], [120, 90])
    expect(document.querySelector('[data-areas]')?.textContent).toBe(t('nanoBanana.crop.drawnCount', { count: 1 }))
    // A slip under 4 px draws nothing.
    await drag(surface, [200, 200], [202, 260])
    expect(document.querySelectorAll('[data-kept]')).toHaveLength(1)
    await fireEvent.click(button('nanoBanana.crop.send'))
    await answered
    expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer: 'send', keep: [[10, 20, 100, 50]] })
  })

  it('goes back to the suggested areas, and shows the hidden page faintly when asked', async () => {
    const { backend, answered } = await open({ restrictive: true, suggested: [[126, 126, 148, 148]] })
    await drag(stage(), [20, 40], [120, 90])
    const white = /** @type {SVGElement} */ (document.querySelector('[data-hidden]'))
    expect(white.getAttribute('fill-opacity')).toBe('1')
    await fireEvent.click(screen.getByLabelText(t('nanoBanana.crop.showHidden')))
    expect(white.getAttribute('fill-opacity')).toBe('0.8')
    await fireEvent.click(button('nanoBanana.crop.useSuggested'))
    expect(document.querySelector('[data-areas]')?.textContent).toBe(t('nanoBanana.crop.suggestedCount', { count: 1 }))
    await fireEvent.click(button('nanoBanana.crop.send'))
    await answered
    expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer: 'send', keep: null })
  })

  it('draws nothing when the crop is not restrictive', async () => {
    const { backend, answered } = await open()
    await drag(stage(), [20, 40], [120, 90])
    await fireEvent.click(button('nanoBanana.crop.send'))
    await answered
    expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer: 'send', keep: null })
  })

  for (const [key, answer] of [['nanoBanana.crop.skip', 'skip'], ['nanoBanana.crop.stop', 'stop']]) {
    it(`answers ${answer} with its button, and keeps nothing drawn`, async () => {
      const { backend, answered } = await open({ restrictive: true, suggested: [[126, 126, 148, 148]] })
      await drag(stage(), [20, 40], [120, 90])
      await fireEvent.click(button(key))
      await answered
      expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer, keep: null })
    })
  }

  it('treats Escape as Skip and does not close on the backdrop', async () => {
    const { backend, answered } = await open()
    await fireEvent.click(/** @type {HTMLElement} */ (screen.getByRole('dialog').parentElement))
    expect(screen.getByRole('dialog')).toBeTruthy()
    await fireEvent.keyDown(window, { key: 'Escape' })
    await answered
    expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-1', answer: 'skip', keep: null })
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('only closes when the answer is refused because the request ended', async () => {
    const backend = { answerNanoBananaCrop: vi.fn(async () => { throw new Error('decline.reason.nanoBananaStopped') }) }
    render(ModalHost)
    const answered = showCrop(/** @type {any} */ (cropEvent()), /** @type {any} */ (backend))
    await screen.findByRole('dialog')
    await fireEvent.click(button('nanoBanana.crop.send'))
    await expect(answered).resolves.toBeUndefined()
    expect(app.modals).toEqual([])
  })

  it('opens a window for each crop the event channel sends, and for nothing else', async () => {
    /** @type {Set<(event: any) => void>} */
    const handlers = new Set()
    const backend = {
      subscribe: (/** @type {any} */ handler) => { handlers.add(handler); return () => handlers.delete(handler) },
      answerNanoBananaCrop: vi.fn(async () => {}),
    }
    render(ModalHost)
    startNanoBananaCrops(/** @type {any} */ (backend))
    for (const handler of handlers) handler({ type: 'nano-banana-report', runId: 'r' })
    expect(app.modals).toEqual([])
    for (const handler of handlers) handler(cropEvent({ id: 'crop-9' }))
    await screen.findByRole('dialog')
    await fireEvent.click(button('nanoBanana.crop.skip'))
    await vi.waitFor(() => expect(backend.answerNanoBananaCrop).toHaveBeenCalledWith({ id: 'crop-9', answer: 'skip', keep: null }))
    stopNanoBananaCrops()
    expect(handlers.size).toBe(0)
  })
})
