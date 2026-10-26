/**
 * A Layers row's engine picker and the cloud: Cloud is offered last while a
 * cloud endpoint is ready and allowed, and left out otherwise; a mask the
 * cloud rendered reads as Cloud; and a pick that changes nothing, a cloud
 * consent cancelled, leaves the picker on the engine the mask has.
 *
 * Then what the row says about Try again - that it replaces the layer, where a
 * new stroke refines it; that it is off for a paint or clone stroke, and why -
 * and the three answers a layer in Needs review offers.
 *
 * Against the browser mock with every delay zero, the chapter open on a
 * region with a local mask. The consent dialog is answered through the modal
 * stack, which is what its buttons do. The editor's undo and the two mask
 * actions the review controls call are spies over the real modules, so the
 * picker's cases still run through the real re-run.
 */
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const undoState = vi.hoisted(() => ({ offered: false }))

vi.mock('../state/editor.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return {
    ...actual,
    undo: vi.fn(),
    undoAvailable: vi.fn(() => undoState.offered),
    undoLabelKey: vi.fn(() => (undoState.offered ? 'masks.command.deleteMask' : null)),
    recordRegionEdit: vi.fn(actual.recordRegionEdit),
  }
})

vi.mock('./maskactions.svelte.js', async (importOriginal) => {
  const actual = /** @type {any} */ (await importOriginal())
  return {
    ...actual,
    keepDependencyResult: vi.fn(actual.keepDependencyResult),
    setDetectedMaskPadding: vi.fn(actual.setDetectedMaskPadding),
    requestDetectedMaskPadding: vi.fn(actual.requestDetectedMaskPadding),
    rerunMask: vi.fn(actual.rerunMask),
  }
})

import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { app, clearNotices, closeAllModals, closeModal } from '../state/app.svelte.js'
import { refreshCloudReadiness, stopCloud } from '../state/cloud.svelte.js'
import { applyRegionDelta, applyRegionState, editor, recordRegionEdit, replaceRegion, select, undo } from '../state/editor.svelte.js'
import { pageVersion } from '../api/tile.js'
import { setCloudAllowed } from '../state/session.svelte.js'
import MaskRow from './MaskRow.svelte'
import { keepDependencyResult, requestDetectedMaskPadding, rerunMask, stepMaskPadding } from './maskactions.svelte.js'
import { maskRow } from './maskrows.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const PROFILE = {
  id: 'mc-abc123',
  name: 'Studio GPU',
  endpointUrl: 'https://ws--mc-abc123-gateway.modal.run/mc/v1',
}

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
})

afterEach(() => {
  cleanup()
  closeAllModals()
  stopCloud()
  setCloudAllowed(false)
  clearNotices()
  editor.chapter = null
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/**
 * The chapter open on a region with a local Fill mask, on a mock whose one
 * Modal endpoint is the default, with the cloud permission as given.
 *
 * @param {{allowed?: boolean}} [options]
 */
async function opened({ allowed = true } = {}) {
  const backend = createMockBackend({ timing: ZERO })
  await backend.writeSettings({ cloudEngines: allowed ? 'allowed' : 'blocked' })
  setCloudAllowed(allowed)
  await backend.writeInferenceConfig({
    config: {
      schemaVersion: 1,
      selectedTarget: { type: 'modal', profile_id: PROFILE.id },
      beamProfiles: {},
      modalProfiles: {
        [PROFILE.id]: {
          ...PROFILE,
          canonicalOrigin: new URL(PROFILE.endpointUrl).origin,
          canonicalOriginFingerprint: '',
          createdAtMs: 1,
          updatedAtMs: 1,
        },
      },
    },
  })
  await backend.storeCloudSecret({
    provider: 'modal',
    profileId: PROFILE.id,
    role: 'runtime',
    secret: 'rt-secret',
    tokenId: 'rt-id',
  })
  setBackend(backend)
  await refreshCloudReadiness(backend)

  const indices = [0, 1, 2, 3, 4, 5]
  const first = await backend.loadPages({ chapterId: CHAPTER, indices })
  const page = /** @type {any} */ (first.find((candidate) => candidate.regions.length > 0))
  const regionId = page.regions[0].id
  await backend.applyTool({
    tool: 'contentAwareFill',
    params: { engine: 'local' },
    chapterId: CHAPTER,
    pageIndex: page.index,
    regionId,
  })
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: await backend.loadPages({ chapterId: CHAPTER, indices }),
  })
  return { backend, region: current(regionId) }
}

/**
 * The region as the open chapter holds it now.
 *
 * @param {string} regionId
 * @returns {any}
 */
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

const picker = () => /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('masks.action.engine')))
const offered = () => Array.from(picker().options, (option) => option.value)

describe('the engine picker on a Layers row', () => {
  it('shows a marked generated texture result with a concrete check', async () => {
    const { region } = await opened({ allowed: false })
    const flagged = withMask(region, { generatedTextureReview: true })
    const view = show(flagged)
    expect(view.container.textContent).toContain(t('review.reason.checkGeneratedTexture'))
    expect(view.container.textContent).toContain(t('review.detail.generatedTexture'))
    expect(view.container.textContent).toContain(t('review.fact.detail'))
  })

  it('offers Cloud last while a cloud endpoint is ready and allowed', async () => {
    const { region } = await opened()
    show(region)
    expect(offered()).toEqual(['fill', 'screentone', 'lama', 'cloud'])
    expect(picker().value).toBe('fill')
  })

  it('leaves Cloud out while the cloud is off', async () => {
    const { region } = await opened({ allowed: false })
    show(region)
    expect(offered()).toEqual(['fill', 'screentone', 'lama'])
  })

  // Denoise fill is gone. A patch saved by it reads as Fill, so the picker
  // shows Fill selected rather than a missing name.
  it('shows a patch saved as the retired denoise rung as Fill', async () => {
    const { region } = await opened({ allowed: false })
    show(strokeBy(region, 'denoise'))
    expect(offered()).toEqual(['fill', 'screentone', 'lama'])
    expect(picker().value).toBe('fill')
    expect(document.body.textContent).not.toContain('denoise')
  })

  it('names a mask the cloud rendered Cloud, and still shows it selected while the cloud is off', async () => {
    const { region } = await opened({ allowed: false })
    const cloudPatch = {
      ...region,
      mask: {
        ...region.mask,
        provenance: {
          ...region.mask.provenance,
          engine: 'flux',
          cloud: { provider: 'modal', profile_id: PROFILE.id, request_id: 'req-1', model: 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32', cost: null },
        },
      },
    }
    show(cloudPatch)
    expect(screen.getByText('☁ FLUX.2 Klein 9B · Cloud', { selector: '.title' })).toBeTruthy()
    expect(offered()).toEqual(['cloud', 'fill', 'screentone', 'lama'])
    expect(picker().value).toBe('cloud')
  })

  it('goes back to the engine the mask has when the cloud consent is cancelled', async () => {
    const { backend, region } = await opened()
    const rerun = vi.spyOn(backend, 'rerunMask')
    show(region)

    await fireEvent.change(picker(), { target: { value: 'cloud' } })
    // The pick shows while its consent is asked.
    expect(picker().value).toBe('cloud')
    await waitFor(() => expect(app.modals.map((modal) => modal.kind)).toEqual(['cloudConsent']))
    closeModal('cancel')

    await waitFor(() => expect(picker().value).toBe('fill'))
    expect(rerun).not.toHaveBeenCalled()
  })
})

/**
 * The region as the row would get it with its mask changed by `patch`.
 *
 * @param {any} region
 * @param {Record<string, unknown>} patch
 * @returns {any}
 */
function withMask(region, patch) {
  return { ...region, mask: { ...region.mask, ...patch } }
}

/** @param {any} region @param {string} engine @returns {any} */
function strokeBy(region, engine) {
  return withMask(region, { provenance: { ...region.mask.provenance, engine } })
}

/**
 * The row's own focusable controls, in tab order: native buttons and selects
 * that are neither disabled nor taken out of the order.
 *
 * @returns {HTMLElement[]}
 */
function tabStops() {
  const row = /** @type {HTMLElement} */ (document.querySelector('[data-mask-row]'))
  return /** @type {HTMLElement[]} */ (Array.from(row.querySelectorAll('button, select')))
    .filter((element) => !(/** @type {any} */ (element).disabled) && element.tabIndex >= 0)
}

/**
 * Activate a focused native button from the keyboard the way a browser does:
 * Enter on keydown, Space on keyup, each followed by the click the browser
 * synthesises unless a handler cancelled the key. jsdom synthesises no click,
 * so without this a test that only sends the key never reaches the button's
 * click path at all.
 *
 * @param {HTMLElement} element
 * @param {'Enter'|' '} key
 */
async function pressKey(element, key) {
  const init = { key, code: key === ' ' ? 'Space' : 'Enter', bubbles: true, cancelable: true }
  const down = await fireEvent.keyDown(element, init)
  const up = await fireEvent.keyUp(element, init)
  const activates = key === 'Enter' ? down : down && up
  if (activates) await fireEvent.click(element)
}

describe('what a row says about Try again', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    undoState.offered = false
  })

  it('says Try again replaces this layer, where a new stroke refines it', async () => {
    const { region } = await opened({ allowed: false })
    show(region)
    const retry = screen.getByRole('button', { name: t('masks.action.retry') })
    expect(retry.getAttribute('title')).toBe(t('masks.action.retryHint'))
    expect(retry.hasAttribute('aria-disabled')).toBe(false)
    expect(picker().getAttribute('title')).toBe(t('masks.action.engineHint'))
    // Visible under the picker, so it is read without a hover.
    expect(screen.getByText(t('masks.action.rerunNote'))).toBeTruthy()

    await fireEvent.click(retry)
    expect(rerunMask).toHaveBeenCalledWith(region, 'retry')

    // The keyboard reaches the same press, which is what makes the blocked
    // case below a real check rather than a key nothing listens to.
    vi.mocked(rerunMask).mockClear()
    retry.focus()
    await pressKey(retry, 'Enter')
    await pressKey(retry, ' ')
    expect(rerunMask).toHaveBeenCalledTimes(2)
    expect(rerunMask).toHaveBeenNthCalledWith(2, region, 'retry')
  })

  for (const engine of ['paint', 'clone']) {
    it(`keeps Try again in place but off for a ${engine} stroke, and says why`, async () => {
      const { backend, region } = await opened({ allowed: false })
      const rerun = vi.spyOn(backend, 'rerunMask')
      const stroke = strokeBy(region, engine)
      show(stroke)

      const retry = screen.getByRole('button', { name: t('masks.action.retry') })
      expect(retry.getAttribute('aria-disabled')).toBe('true')
      expect(retry.getAttribute('title')).toBe(t('masks.action.retryBlocked'))
      // Still in the tab order, which a native `disabled` would take it out of,
      // so the reason can be reached from the keyboard.
      expect(/** @type {HTMLButtonElement} */ (retry).disabled).toBe(false)
      expect(tabStops()).toContain(retry)
      retry.focus()
      expect(document.activeElement).toBe(retry)

      // The same reason in the open row, where the picker would be; no picker,
      // since there is no engine to swap a stroke to.
      expect(screen.getByText(t('masks.action.retryBlocked'), { selector: 'p' })).toBeTruthy()
      expect(screen.queryByLabelText(t('masks.action.engine'))).toBe(null)
      expect(screen.queryByText(t('masks.action.rerunNote'))).toBe(null)

      await fireEvent.click(retry)
      expect(rerunMask).not.toHaveBeenCalled()
      expect(rerun).not.toHaveBeenCalled()

      // Reachable from the keyboard, and still nothing from it: Enter and
      // Space on the focused control run nothing, and focus stays put so
      // the reason is still what is read.
      await pressKey(retry, 'Enter')
      await pressKey(retry, ' ')
      expect(rerunMask).not.toHaveBeenCalled()
      expect(rerun).not.toHaveBeenCalled()
      expect(document.activeElement).toBe(retry)
      // Delete is still the live control beside it.
      expect(screen.getByRole('button', { name: t('masks.action.delete') }).hasAttribute('aria-disabled')).toBe(false)
    })
  }

  it('shows no Try again at all for an approved text-shape component', async () => {
    const { region } = await opened({ allowed: false })
    show({ ...region, id: 'c1-p001-hreview-sam-00001-deadbeef', mask: { ...region.mask, id: 'c1-p001-hreview-sam-00001-deadbeef-m1' } })
    expect(screen.queryByRole('button', { name: t('masks.action.retry') })).toBe(null)
    expect(screen.queryByText(t('masks.action.retryBlocked'))).toBe(null)
  })
})

describe('a row in Needs review', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    undoState.offered = true
  })

  afterEach(() => {
    undoState.offered = false
  })

  it('offers Keep, Rebuild and Undo, labelled, in the tab order, and each calls its own action', async () => {
    const { region } = await opened({ allowed: false })
    const flagged = withMask(region, { dependencyReview: 'changed' })
    show(flagged)

    const group = screen.getByRole('group', { name: t('masks.status.needsReview') })
    const keep = within(group).getByRole('button', { name: t('masks.dependency.keep') })
    const rebuild = within(group).getByRole('button', { name: t('masks.dependency.rebuild') })
    const undoButton = within(group).getByRole('button', { name: t('masks.dependency.undoLast') })

    // What each does, beyond its label. Rebuild is Try again and says so;
    // Undo names the action it would reverse, since it is the editor's undo
    // and not scoped to the edit that flagged this layer.
    expect(keep.getAttribute('title')).toBe(t('masks.dependency.keepHint'))
    expect(rebuild.getAttribute('title')).toBe(t('masks.action.retryHint'))
    expect(undoButton.getAttribute('title')).toBe(
      t('editor.action.undoCommand', { commandKey: 'masks.command.deleteMask' }),
    )

    // Native, enabled buttons, reached in this order by Tab.
    const stops = tabStops()
    const order = [keep, rebuild, undoButton].map((button) => stops.indexOf(button))
    expect(order.every((at) => at >= 0)).toBe(true)
    expect(order).toEqual([...order].sort((a, b) => a - b))
    expect(order[2] - order[0]).toBe(2)
    for (const button of [keep, rebuild, undoButton]) {
      button.focus()
      expect(document.activeElement).toBe(button)
    }

    vi.mocked(keepDependencyResult).mockResolvedValueOnce(true)
    await fireEvent.click(keep)
    expect(keepDependencyResult).toHaveBeenCalledTimes(1)
    expect(keepDependencyResult).toHaveBeenCalledWith(flagged)

    vi.mocked(rerunMask).mockResolvedValueOnce(false)
    await fireEvent.click(rebuild)
    expect(rerunMask).toHaveBeenCalledTimes(1)
    expect(rerunMask).toHaveBeenCalledWith(flagged, 'retry')

    await fireEvent.click(undoButton)
    expect(undo).toHaveBeenCalledTimes(1)
    // Each press is its own action and nothing else.
    expect(keepDependencyResult).toHaveBeenCalledTimes(1)
    expect(rerunMask).toHaveBeenCalledTimes(1)
  })

  it('leaves out Undo with nothing to undo, and Rebuild for a stroke that cannot be run again', async () => {
    undoState.offered = false
    const { region } = await opened({ allowed: false })
    show(withMask(strokeBy(region, 'paint'), { dependencyReview: 'unknown' }))

    const group = screen.getByRole('group', { name: t('masks.status.needsReview') })
    expect(within(group).getAllByRole('button').map((button) => button.textContent?.trim()))
      .toEqual([t('masks.dependency.keep')])
  })
})

/**
 * A stored detection's row: Detected, with Clean here, Clean on cloud GPU
 * while the cloud is usable, and Delete, and nothing about layer appearance
 * (there are no pixels yet to style).
 */
describe('a detected row', () => {
  /**
   * The open chapter after a Detect run on the first page with regions.
   *
   * @param {{allowed?: boolean}} [options]
   */
  async function detected(options) {
    const { backend } = await opened(options)
    const indices = [0, 1, 2, 3, 4, 5]
    const pages = await backend.loadPages({ chapterId: CHAPTER, indices })
    const page = /** @type {any} */ (pages.find((candidate) => candidate.regions.some((region) => region.outcome === 'pending')))
    let done = () => {}
    const finished = new Promise((resolve) => { done = resolve })
    const stop = backend.subscribe((event) => { if (event.type === 'run-finished') done() })
    await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: page.index, mode: 'detect' })
    await finished
    stop()
    editor.chapter = /** @type {any} */ ({ id: CHAPTER, review: [], pages: await backend.loadPages({ chapterId: CHAPTER, indices }) })
    const region = current(page.regions.find((/** @type {any} */ candidate) => candidate.outcome === 'pending').id)
    expect(region.outcome).toBe('detected')
    return { backend, region }
  }

  const acts = () => within(/** @type {HTMLElement} */ (document.querySelector('[data-acts]')))

  it('has no output for the seam to style: opacity and placement are refused by name', async () => {
    const { backend, region } = await detected({ allowed: false })
    await expect(backend.setLayerStyle({ regionId: region.id, layer: { opacity: 50 } })).rejects.toThrow('masks.refused.noOutput')
    await expect(backend.setLayerStyle({ regionId: region.id, layer: { offsetX: 4 } })).rejects.toThrow('masks.refused.noOutput')
    show(region)
    expect(document.querySelector('[data-layer-style]')).toBeNull()
  })

  it('reads Detected, offers Clean, Clean on cloud GPU and Delete, and no layer style', async () => {
    const { region } = await detected()
    const view = show(region)
    expect(view.container.textContent).toContain(t('masks.status.detected'))
    expect(acts().getByRole('button', { name: t('masks.action.cleanDetected') })).toBeTruthy()
    expect(acts().getByRole('button', { name: t('masks.action.cleanDetectedCloud') })).toBeTruthy()
    expect(screen.getByRole('button', { name: t('masks.action.deleteRegion') })).toBeTruthy()
    expect(view.container.querySelector('.layer-style')).toBeNull()
  })

  it('leaves Clean on cloud GPU out while the cloud is off', async () => {
    const { region } = await detected({ allowed: false })
    show(region)
    expect(acts().queryByRole('button', { name: t('masks.action.cleanDetectedCloud') })).toBeNull()
    expect(acts().getByRole('button', { name: t('masks.action.cleanDetected') })).toBeTruthy()
  })

  it('stays a detection when flagged for repair: its reason leads, Clean stays on its line, and cloud is still left out while off', async () => {
    const { region } = await detected({ allowed: false })
    const flagged = { ...region, attention: 'review.reason.repairNeeded' }
    const view = show(flagged)
    expect(view.container.querySelector('.dot')?.classList.contains('detected')).toBe(true)
    expect(view.container.querySelector('.sub')?.textContent).toMatch(new RegExp(`^${t('review.reason.repairNeeded')}`))
    expect(view.container.textContent).toContain(t('masks.status.detected'))
    expect(acts().queryByRole('button', { name: t('masks.action.cleanDetectedCloud') })).toBeNull()
    // Once on the collapsed line, once among the actions.
    expect(screen.getAllByRole('button', { name: t('masks.action.cleanDetected') })).toHaveLength(2)
  })

  it('cleans the region here from its stored mask', async () => {
    const { backend, region } = await detected()
    const applyTool = vi.spyOn(backend, 'applyTool')
    show(region)
    await fireEvent.click(acts().getByRole('button', { name: t('masks.action.cleanDetected') }))
    await waitFor(() => expect(current(region.id).outcome).toBe('cleaned'))
    expect(applyTool).toHaveBeenCalledWith(expect.objectContaining({ tool: 'autoClean', regionId: region.id }))
  })

  it('asks for the cloud consent for this region before anything is sent, and never cleans it here', async () => {
    const { backend, region } = await detected()
    const consent = vi.spyOn(backend, 'prepareCloudConsent')
    const prepare = vi.spyOn(backend, 'prepareCloudClean')
    const applyTool = vi.spyOn(backend, 'applyTool')
    show(region)
    await fireEvent.click(acts().getByRole('button', { name: t('masks.action.cleanDetectedCloud') }))
    await waitFor(() => expect(app.modals.at(-1)?.kind).toBe('cloudConsent'))
    expect(consent).toHaveBeenCalledWith(expect.objectContaining({ regionId: region.id,
      intent: { action: 'applyTool', tool: 'autoClean', params: { engine: 'cloud' } } }))
    // Not the run's batch plan, which keeps a LaMa pick on this computer.
    expect(prepare).not.toHaveBeenCalled()
    closeModal('cancel')
    await waitFor(() => expect(app.modals).toEqual([]))
    expect(applyTool).not.toHaveBeenCalled()
    expect(current(region.id).outcome).toBe('detected')
  })

  it('deletes the detection for good, with no undo offered', async () => {
    const { backend, region } = await detected()
    const deleteMask = vi.spyOn(backend, 'deleteMask')
    show(region)
    await fireEvent.click(screen.getByRole('button', { name: t('masks.action.deleteRegion') }))
    await waitFor(() => expect(current(region.id)).toBeNull())
    expect(deleteMask).toHaveBeenCalledWith({ maskId: region.mask.id })
    expect(recordRegionEdit).not.toHaveBeenCalled()
  })
})

/**
 * What a row offers for the layer's look, by what the layer is: opacity on
 * every cleaned layer, the lock and a way back to where it was made only on
 * one that moves, and never the numeric move and rotation fields - moving and
 * turning happen on the page. The opacity slider previews through coalesced
 * writes and leaves one undo entry, and undo and redo bring back both the
 * value and the tile identity.
 */
describe('the layer controls on a row', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  // Every undo entry reaches this test's backend before the next one's.
  afterEach(async () => {
    await editor.history.running
  })

  const numberFields = () => document.querySelectorAll('input[type="number"]')
  const slider = () => {
    const found = document.querySelector('[data-layer-style] input[type="range"]')
    if (!found) throw new Error('no opacity slider')
    expect(found).toBe(screen.getByRole('slider', { name: t('masks.action.opacity') }))
    return /** @type {HTMLInputElement} */ (found)
  }

  it('offers a fill opacity, the lock and how to move it, and no numeric fields', async () => {
    const { region } = await opened({ allowed: false })
    const view = show(region)
    expect(numberFields()).toHaveLength(0)
    expect(slider().value).toBe('100')
    expect(screen.getByRole('checkbox', { name: t('masks.action.locked') })).toBeTruthy()
    expect(view.container.textContent).toContain(t('masks.layer.moveHint'))
    expect(view.container.textContent).not.toContain(t('masks.layer.fixedNote'))
    expect(screen.queryByRole('button', { name: t('masks.layer.resetPosition') })).toBeNull()
  })

  it('offers a redraw its opacity and nothing that moves it: no lock, no numeric fields', async () => {
    const { region } = await opened({ allowed: false })
    for (const engine of ['lama', 'flux', 'cloud']) {
      const view = show(strokeBy(region, engine))
      expect(numberFields()).toHaveLength(0)
      expect(slider()).toBeTruthy()
      expect(screen.queryByRole('checkbox')).toBeNull()
      expect(view.container.textContent).toContain(t('masks.layer.fixedNote'))
      expect(view.container.textContent).not.toContain(t('masks.layer.moveHint'))
      cleanup()
    }
  })

  it('reads the native capabilities over anything the engine would suggest', async () => {
    const { region } = await opened({ allowed: false })
    show(withMask(region, { capabilities: { transform: 'fixed', lock: false, opacity: true } }))
    expect(screen.queryByRole('checkbox')).toBeNull()
  })

  it('previews a slider drag through coalesced writes and commits one undo entry on release', async () => {
    const { backend, region } = await opened({ allowed: false })
    const write = vi.spyOn(backend, 'setLayerStyle')
    const pageOf = () => editor.chapter.pages.find((page) => page.regions.some((candidate) => candidate.id === region.id))
    const untouched = pageVersion(pageOf(), 'cleaned')
    show(region)

    for (const value of ['80', '60', '40']) await fireEvent.input(slider(), { target: { value } })
    expect(slider().value).toBe('40')
    expect(recordRegionEdit).not.toHaveBeenCalled()
    await fireEvent.pointerUp(slider())
    await fireEvent.change(slider(), { target: { value: '40' } })

    await waitFor(() => expect(recordRegionEdit).toHaveBeenCalledTimes(1))
    expect(write.mock.calls.length).toBeLessThanOrEqual(3)
    expect(write.mock.calls.at(-1)?.[0].layer.opacity).toBe(40)
    const [label, regionId, before, after] = /** @type {any} */ (recordRegionEdit).mock.calls[0]
    expect(label).toBe('masks.command.layerOpacity')
    expect(before.region.mask.layer?.opacity ?? 100).toBe(100)
    expect(after.region.mask.layer.opacity).toBe(40)
    expect(current(regionId).mask.layer.opacity).toBe(40)
    const faded = pageVersion(pageOf(), 'cleaned')
    expect(faded).not.toBe(untouched)

    // Undo and redo are the two sides of that one entry, through the backend.
    await applyRegionDelta(regionId, { present: true, region: before.region, pageStatus: before.pageStatus })
    expect(current(regionId).mask.layer?.opacity ?? 100).toBe(100)
    expect(pageVersion(pageOf(), 'cleaned')).toBe(untouched)
    await applyRegionDelta(regionId, { present: true, region: after.region, pageStatus: after.pageStatus })
    expect(current(regionId).mask.layer.opacity).toBe(40)
    expect(pageVersion(pageOf(), 'cleaned')).toBe(faded)
  })

  it('commits arrow-key steps on the slider once the presses stop, five percent a press', async () => {
    const { region } = await opened({ allowed: false })
    show(region)
    for (let i = 0; i < 3; i += 1) await fireEvent.keyDown(slider(), { key: 'ArrowLeft' })
    expect(slider().value).toBe('85')
    expect(recordRegionEdit).not.toHaveBeenCalled()
    await waitFor(() => expect(recordRegionEdit).toHaveBeenCalledTimes(1))
    expect(/** @type {any} */ (recordRegionEdit).mock.calls[0][3].region.mask.layer.opacity).toBe(85)
  })

  it('commits a typed opacity as one undo entry', async () => {
    const { region } = await opened({ allowed: false })
    show(region)
    const box = screen.getByLabelText(t('tools.param.exactValue', { name: t('masks.action.opacity') }))
    await fireEvent.input(box, { target: { value: '30' } })
    await fireEvent.keyDown(box, { key: 'Enter' })
    await waitFor(() => expect(recordRegionEdit).toHaveBeenCalledTimes(1))
    expect(/** @type {any} */ (recordRegionEdit).mock.calls[0][3].region.mask.layer.opacity).toBe(30)
  })

  it('locks a movable layer, and puts a moved one back where it was made', async () => {
    const { backend, region } = await opened({ allowed: false })
    const write = vi.spyOn(backend, 'setLayerStyle')
    const moved = await backend.setLayerStyle({ regionId: region.id, layer: { opacity: 100, offsetX: 12, offsetY: 4, rotation: 30, locked: false } })
    replaceRegion(moved)
    write.mockClear()
    const view = show(current(region.id))
    await fireEvent.click(screen.getByRole('button', { name: t('masks.layer.resetPosition') }))
    await waitFor(() => expect(write).toHaveBeenCalledTimes(1))
    expect(write.mock.calls[0][0].layer).toMatchObject({ offsetX: 0, offsetY: 0, rotation: 0 })
    await waitFor(() => expect(recordRegionEdit).toHaveBeenCalledTimes(1))
    expect(/** @type {any} */ (recordRegionEdit).mock.calls[0][0]).toBe('masks.command.layerMove')
    view.unmount()

    show(current(region.id))
    await fireEvent.click(screen.getByRole('checkbox', { name: t('masks.action.locked') }))
    await waitFor(() => expect(write).toHaveBeenCalledTimes(2))
    expect(write.mock.calls[1][0].layer.locked).toBe(true)
    await waitFor(() => expect(recordRegionEdit).toHaveBeenCalledTimes(2))
    expect(/** @type {any} */ (recordRegionEdit).mock.calls[1][0]).toBe('masks.command.layerLock')
  })

  it('leaves no undo entry, and writes no more, for a layer deleted before the slider settled', async () => {
    const { backend, region } = await opened({ allowed: false })
    const write = vi.spyOn(backend, 'setLayerStyle')
    const view = show(region)
    await fireEvent.input(slider(), { target: { value: '60' } })
    await waitFor(() => expect(write).toHaveBeenCalledTimes(1))
    // The row's trash, or Cmd-Backspace, lands before the drag is let go.
    applyRegionState(region.id, null)
    await fireEvent.input(slider(), { target: { value: '50' } })
    await fireEvent.pointerUp(slider())
    view.unmount()
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(write).toHaveBeenCalledTimes(1)
    // An entry here would pair "shown" with "shown", and undoing it would put
    // the deleted layer back.
    expect(recordRegionEdit).not.toHaveBeenCalled()
  })

  it('reports a refused change by name and leaves the layer as it was', async () => {
    const { backend, region } = await opened({ allowed: false })
    // The backend has since redrawn it with LaMa; the row still shows the fill.
    await backend.rerunMask({ maskId: region.mask.id, kind: 'engine', engine: 'lama' })
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    show(region)
    await fireEvent.click(screen.getByRole('checkbox', { name: t('masks.action.locked') }))
    await waitFor(() => expect(app.notices.at(-1)?.key).toBe('masks.notice.layerRefused'))
    expect(app.notices.at(-1)?.params).toEqual({ reasonKey: 'masks.refused.noLock' })
    expect(error).toHaveBeenCalled()
    expect(recordRegionEdit).not.toHaveBeenCalled()
    expect(current(region.id).mask.layer?.locked ?? false).toBe(false)
  })
})


describe('padding on an individual detected mask', () => {
  /** The opened chapter's region as a detection with `paddingPx`, held by the editor too. */
  async function detection(paddingPx) {
    const opening = await opened({ allowed: false })
    const detected = { ...opening.region, outcome: 'detected', detected: true, paddingPx }
    replaceRegion(detected)
    return { ...opening, detected: current(detected.id) }
  }
  const slider = (/** @type {any} */ root = screen) =>
    /** @type {HTMLInputElement} */ (root.getByRole('slider', { name: t('tools.param.maskPadding') }))
  const box = (/** @type {any} */ root = screen) =>
    /** @type {HTMLInputElement} */ (root.getByLabelText(t('tools.param.exactValue', { name: t('tools.param.maskPadding') })))
  const request = () => vi.mocked(requestDetectedMaskPadding).mockResolvedValue(undefined)

  // The region menu's slider is the row's: the same call, the menu left open.
  it('offers the same padding in the region menu, and keeps the menu open', async () => {
    const { detected } = await detection(3)
    const save = request()
    const view = show(detected)
    await fireEvent.contextMenu(/** @type {HTMLElement} */ (view.container.querySelector('[data-mask-row]')), { clientX: 20, clientY: 20 })
    const menu = within(screen.getByRole('menu', { name: t('masks.menu.label') }))
    expect(slider(menu).value).toBe('3')
    await fireEvent.input(slider(menu), { target: { value: '9' } })
    expect(slider(menu).getAttribute('aria-valuetext')).toBe('9 px')
    expect(save).not.toHaveBeenCalled()
    await fireEvent.change(slider(menu), { target: { value: '9' } })
    expect(save).toHaveBeenCalledWith(expect.objectContaining({ id: detected.id }), 9)
    // The box takes a typed value, and the menu stays open for another.
    await fireEvent.input(box(menu), { target: { value: '14' } })
    await fireEvent.keyDown(box(menu), { key: 'Enter' })
    expect(save).toHaveBeenLastCalledWith(expect.objectContaining({ id: detected.id }), 14)
    expect(screen.getByRole('menu', { name: t('masks.menu.label') })).toBeTruthy()
  })

  it('shows the saved padding and saves this row when the slider is released or a value typed', async () => {
    const { detected } = await detection(3)
    const save = request()
    show(detected)
    expect(slider().value).toBe('3')
    expect(slider().max).toBe('32')
    await fireEvent.input(slider(), { target: { value: '6' } })
    expect(slider().getAttribute('aria-valuetext')).toBe('6 px')
    expect(save).not.toHaveBeenCalled()
    await fireEvent.change(slider(), { target: { value: '6' } })
    expect(save).toHaveBeenCalledWith(detected, 6)
    // Past the end is the end.
    await fireEvent.input(box(), { target: { value: '99' } })
    await fireEvent.blur(box())
    expect(save).toHaveBeenLastCalledWith(detected, 32)
  })

  it('saves a change made while another is saving, and only the newest one waiting', async () => {
    const { backend, detected } = await detection(2)
    /** @type {Array<() => void>} */
    const pending = []
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockImplementation((spec) => new Promise((resolve) => {
      pending.push(() => resolve({ changed: [spec.regionId], removed: [], pages: [] }))
    }))
    const first = requestDetectedMaskPadding(detected, 4)
    void requestDetectedMaskPadding(detected, 5)
    void requestDetectedMaskPadding(detected, 7)
    await waitFor(() => expect(pending).toHaveLength(1))
    pending.shift()?.()
    await waitFor(() => expect(pending).toHaveLength(1))
    pending.shift()?.()
    await first
    expect(sent.mock.calls.map(([spec]) => spec.paddingPx)).toEqual([4, 7])
  })

  it('holds the control while a run is on the page, and keeps it while the run is on another', async () => {
    const { detected } = await detection(2)
    const save = request()
    show(detected)
    const pageIndex = editor.chapter.pages.find((page) => page.regions.some((entry) => entry.id === detected.id)).index
    editor.run.active = true
    try {
      editor.run.busyPages = [pageIndex]
      await waitFor(() => expect(slider().disabled).toBe(true))
      editor.run.busyPages = [pageIndex + 1]
      await waitFor(() => expect(slider().disabled).toBe(false))
      await fireEvent.change(slider(), { target: { value: '6' } })
      expect(save).toHaveBeenCalledWith(expect.objectContaining({ id: detected.id }), 6)
    } finally {
      editor.run.active = false
      editor.run.busyPages = []
    }
  })

  it('keeps the control off cleaned layers', async () => {
    const { region } = await opened({ allowed: false })
    show(region)
    expect(screen.queryByRole('slider', { name: t('tools.param.maskPadding') })).toBeNull()
  })

  // `;` and `'`: a run of presses shows at once and is saved once.
  it('steps the selected detection a pixel a press, and saves the run of presses once', async () => {
    const { backend, detected } = await detection(3)
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [detected.id], removed: [], pages: [] })
    show(detected)
    select(detected.id)
    try {
      stepMaskPadding(1)
      stepMaskPadding(1)
      stepMaskPadding(1)
      stepMaskPadding(-1)
      await waitFor(() => expect(slider().value).toBe('5'))
      expect(sent).not.toHaveBeenCalled()
      await waitFor(() => expect(sent).toHaveBeenCalledTimes(1))
      expect(sent).toHaveBeenCalledWith(expect.objectContaining({ regionId: detected.id, paddingPx: 5 }))
    } finally {
      select(null)
    }
  })

  it('steps the page\'s padding when no detection is selected', async () => {
    const { backend } = await detection(0)
    const applied = vi.spyOn(backend, 'setDetectionPadding')
    select(null)
    const before = Number(editor.toolParams.autoClean?.maskPadding ?? 0)
    stepMaskPadding(1)
    stepMaskPadding(1)
    expect(editor.toolParams.autoClean.maskPadding).toBe(before + 2)
    await waitFor(() => expect(applied).toHaveBeenCalledTimes(1))
    expect(applied).toHaveBeenCalledWith(expect.objectContaining({ paddingPx: before + 2 }))
    expect(applied.mock.calls[0][0].regionId).toBeUndefined()
  })

  it('steps the page from the padding its detections share, not the panel\'s', async () => {
    const { backend } = await detection(6)
    const applied = vi.spyOn(backend, 'setDetectionPadding')
    select(null)
    editor.toolParams.autoClean = { ...(editor.toolParams.autoClean ?? {}), maskPadding: 0 }
    stepMaskPadding(1)
    expect(editor.toolParams.autoClean.maskPadding).toBe(7)
    await waitFor(() => expect(applied).toHaveBeenCalledTimes(1))
    expect(applied).toHaveBeenCalledWith(expect.objectContaining({ paddingPx: 7 }))
  })

  it('saves a run of page presses to the page it started on', async () => {
    const { backend } = await detection(2)
    const applied = vi.spyOn(backend, 'setDetectionPadding')
    select(null)
    const start = editor.pageIndex
    stepMaskPadding(1)
    editor.pageIndex = start + 1
    try {
      await waitFor(() => expect(applied).toHaveBeenCalledTimes(1))
      expect(applied).toHaveBeenCalledWith(expect.objectContaining({ pageIndex: start, paddingPx: 3 }))
    } finally {
      editor.pageIndex = start
    }
  })

  it('drops a run of presses still waiting when Undo is pressed', async () => {
    const { undo: realUndo } = /** @type {any} */ (await vi.importActual('../state/editor.svelte.js'))
    const { backend, detected } = await detection(3)
    const sent = vi.spyOn(backend, 'setDetectionPadding')
    show(detected)
    select(detected.id)
    try {
      stepMaskPadding(1)
      stepMaskPadding(1)
      await waitFor(() => expect(slider().value).toBe('5'))
      realUndo()
      await waitFor(() => expect(slider().value).toBe('3'))
      await new Promise((resolve) => setTimeout(resolve, 300))
      expect(sent).not.toHaveBeenCalled()
    } finally {
      select(null)
    }
  })

  it('shows a page run of presses on the page\'s rows while it waits', async () => {
    const { backend, detected } = await detection(4)
    vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [], removed: [], pages: [] })
    select(null)
    show(detected)
    stepMaskPadding(1)
    stepMaskPadding(1)
    await waitFor(() => expect(slider().value).toBe('6'))
    // Saved and read back, the rows show the page's own value again.
    await waitFor(() => expect(slider().value).toBe('4'))
  })

  it('saves a waiting page run before a value set on one of its detections, so that value stands', async () => {
    const { backend, detected } = await detection(2)
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [detected.id], removed: [], pages: [] })
    select(null)
    stepMaskPadding(1)
    await requestDetectedMaskPadding(detected, 9)
    await new Promise((resolve) => setTimeout(resolve, 300))
    expect(sent.mock.calls.map(([spec]) => [spec.regionId ?? 'page', spec.paddingPx])).toEqual([['page', 3], [detected.id, 9]])
  })

  it('lands a page save before sending one of its detections\' own, so that value is the last written', async () => {
    const { backend, detected } = await detection(2)
    /** @type {Array<() => void>} */
    const pages = []
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockImplementation((spec) => spec.regionId
      ? Promise.resolve({ changed: [spec.regionId], removed: [], pages: [] })
      : new Promise((resolve) => { pages.push(() => resolve({ changed: [], removed: [], pages: [] })) }))
    select(null)
    const at = editor.pageIndex
    editor.pageIndex = editor.chapter.pages.find((page) => page.regions.some((entry) => entry.id === detected.id)).index
    try {
      // A waiting run, flushed by the row's value.
      stepMaskPadding(1)
      const row = requestDetectedMaskPadding(detected, 9)
      await new Promise((resolve) => setTimeout(resolve, 20))
      expect(sent.mock.calls.map(([spec]) => spec.regionId ?? 'page')).toEqual(['page'])
      pages.shift()?.()
      await row
      expect(sent.mock.calls.map(([spec]) => [spec.regionId ?? 'page', spec.paddingPx])).toEqual([['page', 3], [detected.id, 9]])

      // A page save already on its way when the row's value comes.
      sent.mockClear()
      // The page read back from the mock has it cleaned: a detection again.
      replaceRegion({ ...current(detected.id), outcome: 'detected', detected: true, paddingPx: 9 })
      stepMaskPadding(1)
      await waitFor(() => expect(pages).toHaveLength(1))
      const again = requestDetectedMaskPadding(detected, 11)
      await new Promise((resolve) => setTimeout(resolve, 20))
      expect(sent.mock.calls.map(([spec]) => spec.regionId ?? 'page')).toEqual(['page'])
      pages.shift()?.()
      await again
      expect(sent.mock.calls.at(-1)?.[0]).toEqual(expect.objectContaining({ regionId: detected.id, paddingPx: 11 }))
    } finally {
      editor.pageIndex = at
    }
  })

  it('refuses a page run of presses while a run holds the page, and leaves the panel as the page is', async () => {
    const { backend, detected } = await detection(2)
    const sent = vi.spyOn(backend, 'setDetectionPadding')
    select(null)
    const at = editor.pageIndex
    editor.pageIndex = editor.chapter.pages.find((page) => page.regions.some((entry) => entry.id === detected.id)).index
    editor.toolParams.autoClean = { ...(editor.toolParams.autoClean ?? {}), maskPadding: 2 }
    try {
      editor.run.active = true
      editor.run.busyPages = [editor.pageIndex]
      stepMaskPadding(1)
      expect(editor.toolParams.autoClean.maskPadding).toBe(2)
      expect(app.notices.at(-1)?.key).toBe('notice.run.pageBusy')

      // A run that takes the page while the presses wait: the same refusal,
      // and the panel back where the page still is.
      clearNotices()
      editor.run.active = false
      editor.run.busyPages = []
      stepMaskPadding(1)
      expect(editor.toolParams.autoClean.maskPadding).toBe(3)
      editor.run.active = true
      editor.run.busyPages = [editor.pageIndex]
      await waitFor(() => expect(app.notices.at(-1)?.key).toBe('notice.run.pageBusy'))
      expect(editor.toolParams.autoClean.maskPadding).toBe(2)
      expect(sent).not.toHaveBeenCalled()
    } finally {
      editor.run.active = false
      editor.run.busyPages = []
      editor.pageIndex = at
    }
  })

  it('saves a run of presses on a detection to the chapter it began in, after a switch', async () => {
    const { backend, detected } = await detection(3)
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [detected.id], removed: [], pages: [] })
    const pageIndex = editor.chapter.pages.find((page) => page.regions.some((entry) => entry.id === detected.id)).index
    select(detected.id)
    stepMaskPadding(1)
    select(null)
    editor.chapter = /** @type {any} */ ({ id: 'another-chapter', review: [], pages: [] })
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1))
    expect(sent).toHaveBeenCalledWith(expect.objectContaining({ chapterId: CHAPTER, pageIndex, regionId: detected.id, paddingPx: 4 }))
  })

  it('saves a run of presses still waiting at once when Redo is pressed', async () => {
    const { redo: realRedo } = /** @type {any} */ (await vi.importActual('../state/editor.svelte.js'))
    const { backend, detected } = await detection(3)
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [detected.id], removed: [], pages: [] })
    select(detected.id)
    try {
      stepMaskPadding(1)
      realRedo()
      await waitFor(() => expect(sent).toHaveBeenCalledTimes(1))
      expect(sent).toHaveBeenCalledWith(expect.objectContaining({ regionId: detected.id, paddingPx: 4 }))
      await new Promise((resolve) => setTimeout(resolve, 300))
      expect(sent).toHaveBeenCalledTimes(1)
    } finally {
      select(null)
    }
  })

  it('lets a value set on the slider replace a run of presses still waiting', async () => {
    const { backend, detected } = await detection(3)
    const sent = vi.spyOn(backend, 'setDetectionPadding').mockResolvedValue({ changed: [detected.id], removed: [], pages: [] })
    select(detected.id)
    try {
      stepMaskPadding(1)
      await requestDetectedMaskPadding(detected, 10)
      await new Promise((resolve) => setTimeout(resolve, 300))
      expect(sent.mock.calls.map(([spec]) => spec.paddingPx)).toEqual([10])
    } finally {
      select(null)
    }
  })
})
