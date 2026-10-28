/**
 * A Layers row's Post-processing panel: one preview per finishing method from
 * a cloud result's saved answer, the current one marked, a pick that
 * finishes the layer again as one undo step the backend records, and a note
 * instead of the grid for a layer that kept no answer.
 *
 * Against the browser mock with every delay zero. A local Fill result is made
 * a cloud one by putting it back with a cloud record (`restoreRegion`), which
 * is all the mock reads to call it one.
 */
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { FINISHINGS, finishingKey } from '../model/model-names.js'
import { clearNotices } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import MaskList from './MaskList.svelte'
import MaskRow from './MaskRow.svelte'
import { finishingRequest, requestFinishingPanel } from './maskactions.svelte.js'
import { maskRow } from './maskrows.js'

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

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  // jsdom has no `CSS.escape`, which the list's reveal of the selection uses.
  vi.stubGlobal('CSS', { escape: (/** @type {string} */ value) => value })
})

afterEach(() => {
  cleanup()
  clearNotices()
  finishingRequest.id = null
  editor.chapter = null
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/**
 * The chapter open on a region with a local Fill result, made a cloud
 * result finished by Copy page dots when `cloud` is set.
 *
 * @param {{cloud?: boolean}} [options]
 */
async function opened({ cloud = true } = {}) {
  const backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  const indices = [0, 1, 2, 3, 4, 5]
  const first = await backend.loadPages({ chapterId: CHAPTER, indices })
  const page = /** @type {any} */ (first.find((candidate) => candidate.regions.length > 0))
  const regionId = page.regions[0].id
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
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: await backend.loadPages({ chapterId: CHAPTER, indices }),
  })
  return { backend, regionId }
}

/** @param {string} regionId @returns {any} */
function current(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const found = page.regions.find((candidate) => candidate.id === regionId)
    if (found) return found
  }
  return null
}

/** @param {any} region */
function show(region) {
  return render(MaskRow, { props: { row: maskRow(region, false), region, open: true, ontoggle: () => {} } })
}

const trigger = () => /** @type {HTMLButtonElement} */ (document.querySelector('[data-finishing-open]'))
const panel = () => document.querySelector('[data-finishing-panel]')
const tile = (/** @type {string} */ name) => /** @type {HTMLButtonElement} */ (document.querySelector(`[data-finishing="${name}"]`))

describe('the Post-processing panel on a Layers row', () => {
  it('names the current method on the row and shows a preview per method with it marked', async () => {
    const { regionId } = await opened()
    show(current(regionId))
    expect(trigger().textContent?.trim()).toBe(t(finishingKey('option', 'copy')))
    await fireEvent.click(trigger())
    expect(trigger().getAttribute('aria-expanded')).toBe('true')
    await waitFor(() => expect(document.querySelectorAll('[data-finishing]').length).toBe(FINISHINGS.length))
    for (const name of FINISHINGS) {
      expect(tile(name).querySelector('img')?.getAttribute('src')).toMatch(/^data:image\//)
      expect(tile(name).textContent).toContain(t(finishingKey('option', name)))
    }
    expect(tile('copy').getAttribute('aria-pressed')).toBe('true')
    expect(tile('copy').textContent).toContain(t('masks.finishing.current'))
    expect(tile('poisson').getAttribute('aria-pressed')).toBe('false')
  })

  it('finishes the layer again by the method picked, as one undo step, and moves the mark', async () => {
    const { backend, regionId } = await opened()
    const spy = vi.spyOn(backend, 'setRegionFinishing')
    show(current(regionId))
    await fireEvent.click(trigger())
    await waitFor(() => expect(tile('poisson')).toBeTruthy())
    await fireEvent.click(tile('poisson'))
    await waitFor(() => expect(tile('poisson').getAttribute('aria-pressed')).toBe('true'))
    expect(spy).toHaveBeenCalledWith({ regionId, finishing: 'poisson', grayscale: false })
    expect(current(regionId).mask.provenance.params_snapshot.postprocess.finishing).toBe('poisson')
    expect(tile('copy').getAttribute('aria-pressed')).toBe('false')
    const history = await backend.historyLoad({ chapterId: CHAPTER })
    expect(history.entries.at(-1)?.label).toBe('masks.finishing.command')
    // Picking the current one again sends nothing.
    await fireEvent.click(tile('poisson'))
    expect(spy).toHaveBeenCalledTimes(1)
    // The panel stays open for the next comparison.
    expect(panel()).toBeTruthy()
  })

  it('says a layer cleaned on this computer must be cleaned again, with no grid', async () => {
    const { regionId } = await opened({ cloud: false })
    show(current(regionId))
    expect(trigger().textContent?.trim()).toBe(t('masks.finishing.open'))
    await fireEvent.click(trigger())
    await waitFor(() => expect(screen.getByText(t('masks.finishing.unavailable.notCloud'))).toBeTruthy())
    expect(document.querySelectorAll('[data-finishing]').length).toBe(0)
  })

  it('closes on Escape with the focus on the page, on a press outside, and on its trigger', async () => {
    const { regionId } = await opened()
    show(current(regionId))
    await fireEvent.click(trigger())
    await waitFor(() => expect(tile('copy')).toBeTruthy())
    // WebKit leaves a clicked button unfocused: Escape still reaches the panel.
    const focused = document.activeElement
    if (focused instanceof HTMLElement) focused.blur()
    const later = vi.fn()
    globalThis.addEventListener('keydown', later)
    await fireEvent.keyDown(globalThis, { key: 'Escape' })
    globalThis.removeEventListener('keydown', later)
    expect(panel()).toBeNull()
    // The editor's own Escape never heard it.
    expect(later).not.toHaveBeenCalled()

    await fireEvent.click(trigger())
    await waitFor(() => expect(panel()).toBeTruthy())
    await fireEvent.pointerDown(document.body)
    expect(panel()).toBeNull()

    await fireEvent.click(trigger())
    await waitFor(() => expect(panel()).toBeTruthy())
    await fireEvent.pointerDown(trigger())
    await fireEvent.click(trigger())
    expect(panel()).toBeNull()
  })

  it('opens from the keyboard\'s request: the list expands the row and the row opens its panel', async () => {
    const { regionId } = await opened()
    editor.pageIndex = /** @type {any} */ (editor.chapter).pages.findIndex((/** @type {any} */ page) => page.regions.some((/** @type {any} */ r) => r.id === regionId))
    render(MaskList)
    expect(panel()).toBeNull()
    requestFinishingPanel(regionId)
    await waitFor(() => expect(panel()).toBeTruthy())
    expect(finishingRequest.id).toBe(null)
    await waitFor(() => expect(document.querySelectorAll('[data-finishing]').length).toBe(FINISHINGS.length))
  })

  it('drops a request for a row the list cannot show', async () => {
    await opened()
    editor.pageIndex = 0
    render(MaskList)
    requestFinishingPanel('no-such-region')
    await waitFor(() => expect(finishingRequest.id).toBe(null))
    expect(panel()).toBeNull()
  })

  it('offers nothing on a paint stroke', async () => {
    const { regionId } = await opened({ cloud: false })
    const region = JSON.parse(JSON.stringify(current(regionId)))
    region.mask.provenance.engine = 'paint'
    show(region)
    expect(trigger()).toBeNull()
  })
})
