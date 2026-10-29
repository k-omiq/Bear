/**
 * The Post-processing window: the selected layer's previews, one per
 * finishing method, the current one marked and a large preview of the one
 * the pointer is on; a pick that finishes the layer again as one undo step
 * the backend records; the window following the selection; and a line
 * instead of the grid for no selection, a layer with no post-processing, a
 * load in flight or failed, and every reason the backend gives for a layer
 * it cannot finish again.
 *
 * Against the browser mock with every delay zero. A local Fill result is made
 * a cloud one by putting it back with a cloud record (`restoreRegion`), which
 * is all the mock reads to call it one. The answers the mock does not make
 * (a reason it never gives, a load that never ends) are stubbed on it.
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { FINISHINGS, finishingKey } from '../model/model-names.js'
import { app, clearNotices, closeAllModals } from '../state/app.svelte.js'
import { editor, select } from '../state/editor.svelte.js'
import { session } from '../state/session.svelte.js'
import PostProcessingWindow from './PostProcessingWindow.svelte'
import RegionMenu from './RegionMenu.svelte'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'

/** Local storage for the test: the node running the suite has none. */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() {
      return values.size
    },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

/** @type {{open: boolean, fold: boolean}} */
let wasWindow

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  wasWindow = { open: session.windows.postprocessing.open, fold: session.windows.postprocessing.fold }
  session.windows.postprocessing.open = true
  session.windows.postprocessing.fold = false
})

afterEach(() => {
  cleanup()
  clearNotices()
  closeAllModals()
  select(null)
  editor.chapter = null
  session.windows.postprocessing.open = wasWindow.open
  session.windows.postprocessing.fold = wasWindow.fold
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/**
 * The chapter open on a page with two regions, each with a local Fill
 * result, made cloud results finished by Copy page dots when `cloud` is set.
 *
 * @param {{cloud?: boolean}} [options]
 */
async function opened({ cloud = true } = {}) {
  const backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  const indices = [0, 1, 2, 3, 4, 5]
  const first = await backend.loadPages({ chapterId: CHAPTER, indices })
  const page = /** @type {any} */ (first.find((candidate) => candidate.regions.length > 1))
  const ids = page.regions.slice(0, 2).map((/** @type {any} */ region) => region.id)
  for (const regionId of ids) {
    const applied = /** @type {any} */ (await backend.applyTool({
      tool: 'contentAwareFill',
      params: { engine: 'local' },
      chapterId: CHAPTER,
      pageIndex: page.index,
      regionId,
    }))
    if (cloud) {
      const region = structuredClone(applied.region)
      region.mask.provenance.engine = 'flux'
      region.mask.provenance.cloud = { provider: 'modal', request_id: 'req-1', attempt_id: 'att-0123456789abcdef01234567', model: 'unsloth/flux-3-image-klein', cost: null }
      region.mask.provenance.params_snapshot = { ...region.mask.provenance.params_snapshot, postprocess: { finishing: 'copy', grayscale: false } }
      await backend.restoreRegion({ regionId, region })
    }
  }
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: await backend.loadPages({ chapterId: CHAPTER, indices }),
  })
  return { backend, regionId: ids[0], otherId: ids[1], pageIndex: page.index }
}

/** @param {string} regionId @returns {any} */
function current(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const found = page.regions.find((candidate) => candidate.id === regionId)
    if (found) return found
  }
  return null
}

const tiles = () => document.querySelectorAll('[data-finishing]')
const tile = (/** @type {string} */ name) => /** @type {HTMLButtonElement} */ (document.querySelector(`[data-finishing="${name}"]`))
const stage = () => document.querySelector('[data-finishing-stage]')

describe('the Post-processing window', () => {
  it('names the layer and its method, and shows a preview per method with the current one marked and shown large', async () => {
    const { regionId, pageIndex } = await opened()
    select(regionId)
    render(PostProcessingWindow)
    expect(screen.getByRole('heading', { name: t('editor.panel.postprocessing') })).toBeTruthy()
    expect(document.querySelector('[data-finishing-layer]')?.textContent).toContain(t('masks.finishing.window.page', { page: pageIndex + 1 }))
    expect(document.body.textContent).toContain(t(finishingKey('option', 'copy')))
    await waitFor(() => expect(tiles().length).toBe(FINISHINGS.length))
    for (const name of FINISHINGS) {
      expect(tile(name).querySelector('img')?.getAttribute('src')).toMatch(/^data:image\//)
      expect(tile(name).textContent).toContain(t(finishingKey('option', name)))
    }
    expect(tile('copy').getAttribute('aria-pressed')).toBe('true')
    expect(tile('copy').textContent).toContain(t('masks.finishing.current'))
    expect(tile('poisson').getAttribute('aria-pressed')).toBe('false')
    expect(stage()?.getAttribute('data-finishing-stage')).toBe('copy')
    // The pointer over another method shows it large; leaving goes back.
    await fireEvent.pointerEnter(tile('poisson'))
    expect(stage()?.getAttribute('data-finishing-stage')).toBe('poisson')
    await fireEvent.pointerLeave(tile('poisson'))
    expect(stage()?.getAttribute('data-finishing-stage')).toBe('copy')
  })

  it('finishes the layer again by the method picked, as one undo step, and moves the mark without asking again', async () => {
    const { backend, regionId } = await opened()
    const previews = vi.spyOn(backend, 'regionFinishingPreviews')
    const spy = vi.spyOn(backend, 'setRegionFinishing')
    select(regionId)
    render(PostProcessingWindow)
    await waitFor(() => expect(tile('poisson')).toBeTruthy())
    await fireEvent.click(tile('poisson'))
    await waitFor(() => expect(tile('poisson').getAttribute('aria-pressed')).toBe('true'))
    expect(spy).toHaveBeenCalledWith({ regionId, finishing: 'poisson', grayscale: false })
    expect(current(regionId).mask.provenance.params_snapshot.postprocess.finishing).toBe('poisson')
    expect(tile('copy').getAttribute('aria-pressed')).toBe('false')
    const history = await backend.historyLoad({ chapterId: CHAPTER })
    expect(history.entries.at(-1)?.label).toBe('masks.finishing.command')
    // Picking the current one again sends nothing, and a pick is not a new result.
    await fireEvent.click(tile('poisson'))
    expect(spy).toHaveBeenCalledTimes(1)
    expect(previews).toHaveBeenCalledTimes(1)
  })

  it('marks the tile whose pick is in flight busy and holds the others', async () => {
    const { backend, regionId } = await opened()
    /** @type {(value: any) => void} */
    let finish = () => {}
    vi.spyOn(backend, 'setRegionFinishing').mockImplementation(() => new Promise((resolve) => { finish = resolve }))
    select(regionId)
    render(PostProcessingWindow)
    await waitFor(() => expect(tile('poisson')).toBeTruthy())
    await fireEvent.click(tile('poisson'))
    expect(tile('poisson').getAttribute('aria-busy')).toBe('true')
    expect(tile('poisson').textContent).toContain(t('masks.finishing.applying'))
    expect(tile('copy').disabled).toBe(true)
    finish(null)
    await waitFor(() => expect(tile('copy').disabled).toBe(false))
  })

  it('follows the selection to another layer', async () => {
    const { backend, regionId, otherId } = await opened()
    const previews = vi.spyOn(backend, 'regionFinishingPreviews')
    select(regionId)
    render(PostProcessingWindow)
    await waitFor(() => expect(tiles().length).toBe(FINISHINGS.length))
    select(otherId)
    await waitFor(() => expect(previews).toHaveBeenLastCalledWith({ regionId: otherId }))
    await waitFor(() => expect(tiles().length).toBe(FINISHINGS.length))
  })

  it('says how to pick a layer when none is selected, and opens Settings > Post-processing from its foot', async () => {
    await opened()
    render(PostProcessingWindow)
    expect(screen.getByText(t('masks.finishing.window.noLayer'))).toBeTruthy()
    expect(tiles().length).toBe(0)
    await fireEvent.click(/** @type {HTMLElement} */ (document.querySelector('[data-finishing-settings]')))
    const top = app.modals.at(-1)
    expect(top?.kind).toBe('settings')
    expect(top?.props?.tab).toBe('postprocessing')
  })

  it('says a layer with no post-processing has none, and asks the backend nothing', async () => {
    const { backend, regionId } = await opened({ cloud: false })
    const previews = vi.spyOn(backend, 'regionFinishingPreviews')
    const region = current(regionId)
    region.mask.provenance.engine = 'paint'
    select(regionId)
    render(PostProcessingWindow)
    expect(screen.getByText(t('masks.finishing.window.notOffered'))).toBeTruthy()
    expect(previews).not.toHaveBeenCalled()
  })

  it('says the previews are on their way, longer for a layer cleaned on this computer, and when they failed', async () => {
    const { backend, regionId } = await opened({ cloud: false })
    vi.spyOn(backend, 'regionFinishingPreviews').mockImplementation(() => new Promise(() => {}))
    select(regionId)
    render(PostProcessingWindow)
    expect(screen.getByText(t('masks.finishing.loadingLocal'))).toBeTruthy()
    cleanup()

    vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(backend, 'regionFinishingPreviews').mockRejectedValue(new Error('refinish_no_patch: gone'))
    render(PostProcessingWindow)
    await waitFor(() => expect(screen.getByText(t('masks.finishing.failed'))).toBeTruthy())
  })

  it.each(['notCloud', 'noResult', 'pageChanged', 'unsupported', 'modelChanged'])('says why a layer cannot be finished again: %s', async (reason) => {
    const { backend, regionId } = await opened()
    vi.spyOn(backend, 'regionFinishingPreviews').mockResolvedValue({ regionId, unavailable: /** @type {any} */ (reason), current: null, grayscale: false, previews: [] })
    select(regionId)
    render(PostProcessingWindow)
    await waitFor(() => expect(screen.getByText(t(`masks.finishing.unavailable.${reason}`))).toBeTruthy())
    expect(tiles().length).toBe(0)
  })

  it('says the previews were made grey first when the result was', async () => {
    const { backend, regionId } = await opened()
    const real = backend.regionFinishingPreviews.bind(backend)
    vi.spyOn(backend, 'regionFinishingPreviews').mockImplementation(async (spec) => ({ ...(await real(spec)), grayscale: true }))
    select(regionId)
    render(PostProcessingWindow)
    await waitFor(() => expect(screen.getByText(t('masks.finishing.introGray'))).toBeTruthy())
  })
})

describe('Post-processing… in the region menu', () => {
  it('is offered on a cleaned layer, selects it and opens the window', async () => {
    const { regionId } = await opened()
    session.windows.postprocessing.open = false
    render(RegionMenu, { props: { at: { x: 10, y: 10, region: current(regionId) }, onclose: () => {} } })
    const entry = screen.getByRole('menuitem', { name: new RegExp(t('masks.finishing.menu')) })
    await fireEvent.click(entry)
    expect(editor.selectionId).toBe(regionId)
    expect(session.windows.postprocessing.open).toBe(true)
  })

  it('is not offered on a paint stroke', async () => {
    const { regionId } = await opened({ cloud: false })
    const region = JSON.parse(JSON.stringify(current(regionId)))
    region.mask.provenance.engine = 'paint'
    render(RegionMenu, { props: { at: { x: 10, y: 10, region }, onclose: () => {} } })
    expect(screen.queryByRole('menuitem', { name: new RegExp(t('masks.finishing.menu')) })).toBeNull()
  })
})
