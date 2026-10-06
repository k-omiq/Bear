import { afterAll, afterEach, beforeAll, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { editor } from '../state/editor.svelte.js'
import { stopCloud } from '../state/cloud.svelte.js'
import { setCloudAllowed } from '../state/session.svelte.js'
import { createHistory } from '../model/history.js'
import WorkflowAnalysis from './WorkflowAnalysis.svelte'

const cpu = (selectable = true) => [{ id: 'ort-cpu', platform: 'macOS', qualified: true,
  available: selectable, selectable, note: 'Local CPU' }]

function capabilities(overrides = {}) {
  return { runtimeInstalled: true, rtInstalled: false, fullRtInstalled: false,
    fullRtManaged: false, fullRtRevision: 'pinned', fullRtFile: { name: 'detector.onnx', bytes: 168481531, sha256: 'sha' },
    samInstalled: false, samMemoryReady: true, samManaged: false, samRevision: 'pinned', samFiles: [],
    cooStatus: 'Rights unresolved', rtBackends: cpu(), samBackends: cpu(),
    samWriteQualified: false, samWriteNote: 'PNG parity unresolved', ...overrides }
}

/** The runtime loads: `diagnostics` finds ONNX Runtime and it is available. */
const LOADS = { diagnostics: async () => ({ components: [{ name: 'onnxruntime', available: true, detail: null, reasonKey: null }] }) }

/**
 * jsdom has no 2D canvas. The pending-correction layer draws into one, so the
 * tests hand it a recorder: each `putImageData` is kept with the canvas it
 * drew into, which is how a test sees a tint that exists before prepare.
 */
const drawn = []
const realGetContext = HTMLCanvasElement.prototype.getContext
/** Every raster a tint asked to decode. jsdom decodes none; `MaskTint.dom.test.js` covers drawing. */
const loadedImages = []
const RealImage = globalThis.Image
beforeAll(() => {
  globalThis.Image = /** @type {any} */ (class {
    set src(value) { loadedImages.push(value) }
  })
  HTMLCanvasElement.prototype.getContext = /** @type {any} */ (function () {
    const canvas = this
    return {
      createImageData: (w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(w * h * 4) }),
      putImageData: (image) => drawn.push({ canvas, image }),
      fillRect() {},
      fillStyle: '',
      globalCompositeOperation: 'source-over',
    }
  })
})
afterAll(() => {
  HTMLCanvasElement.prototype.getContext = realGetContext
  globalThis.Image = RealImage
})

afterEach(() => {
  cleanup()
  setBackend(null)
  stopCloud()
  setCloudAllowed(false)
  editor.chapter = null
  drawn.length = 0
  loadedImages.length = 0
  vi.clearAllMocks()
})

/** A chapter review whose analysis resolves with one page of `evidence`. */
function chapterReview({ evidence, result = {}, caps = {}, backend = {}, chapterId = 'c1' }) {
  editor.chapter = { id: chapterId, pages: [{ id: 'p1', index: 0, number: 1, status: 'unclean', regions: [], resident: true }], review: [] }
  const analysis = { analysisId: 'a1', sourceSha256: 'source', rtProfile: 'full-halves', rtBackend: 'ort-cpu',
    samBackend: 'ort-webgpu', samWriteEligible: true, maskSha256: 'mask', sourceDataUrl: 'data:image/png;base64,AA',
    maskDataUrl: 'data:image/png;base64,AA', timingsMs: { rtLoad: 0, samLoad: 0, samEncoder: 0, samHead: 0, rtPage: 0 },
    evidence: { width: 64, height: 48, components: [], regions: [], groupingSuggestions: [], ...evidence }, ...result }
  const api = {
    ...LOADS,
    listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true, samInstalled: true, samWriteQualified: true, ...caps }),
    verifySamTs: async () => true,
    analyzeChapterPage: vi.fn(async () => analysis),
    loadComponentCorrection: vi.fn(async () => null),
    prepareComponentWrite: vi.fn(async ({ paddingPx, correctionRevision, componentId }) => ({ planId: 'plan', componentId,
      bounds: { x: 30, y: 20, w: 2, h: 1 }, supportPixels: 2, supportDataUrl: 'data:image/png;base64,BB',
      supportSha256: 'support', planIdentitySha256: 'identity', paddingPx, correctionRevision,
      sourceSha256: 'source', underlaySha256: 'underlay', renderVersion: 'bounded-ring-median-v1' })),
    cancelCapabilityAnalysis: vi.fn(async () => true),
    ...backend,
  }
  setBackend(/** @type {any} */ (api))
  const screen = render(WorkflowAnalysis, { props: { chapterId, initialPageIndex: 0 } })
  return { screen, api }
}

async function analyze(screen) {
  const button = await screen.findByRole('button', { name: 'Analyze for review' })
  await waitFor(() => expect(button.disabled).toBe(false))
  await fireEvent.click(button)
}

const bubbled = { id: 'sam-00001', pixels: 2, bounds: { x: 30, y: 20, w: 2, h: 1 }, rtTextIds: ['text-1'], rtBubbleIds: ['bubble-1'] }
const outside = { id: 'sam-00002', pixels: 3, bounds: { x: 4, y: 4, w: 3, h: 1 }, rtTextIds: [], rtBubbleIds: [] }

it('allows mask-only analysis with no RT model or OCR and names the selected SAM backend', async () => {
  const analyze = vi.fn(async () => null)
  setBackend(/** @type {any} */ ({ ...LOADS, listWorkflowCapabilities: async () => capabilities({ samInstalled: true }),
    verifySamTs: async () => true, analyzeCapabilities: analyze }))
  const screen = render(WorkflowAnalysis)
  await waitFor(() => expect(screen.getByText('SHA-256 verified')).toBeTruthy())
  expect(screen.getByText('Independent model analysis')).toBeTruthy()
  await fireEvent.change(screen.getByLabelText('Workflow'), { target: { value: 'mask' } })
  await fireEvent.input(screen.getByLabelText('Source page'), { target: { value: '/tmp/page.png' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  await waitFor(() => expect(analyze).toHaveBeenCalledWith({ sourcePath: '/tmp/page.png', workflow: 'mask',
    rtProfile: 'full-halves', rtBackend: 'auto', samBackend: 'auto', requestId: expect.stringMatching(/^review-/) }))
})

it('enables regions without SAM and keeps COO unavailable', async () => {
  const analyze = vi.fn(async () => null)
  setBackend(/** @type {any} */ ({ ...LOADS, listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true }),
    analyzeCapabilities: analyze }))
  const screen = render(WorkflowAnalysis)
  await waitFor(() => expect(screen.getByText(/Full tiled graph: SHA-256 verified/)).toBeTruthy())
  expect(screen.getByText(/model rights are unresolved/)).toBeTruthy()
  await fireEvent.input(screen.getByLabelText('Source page'), { target: { value: '/tmp/page.png' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  await waitFor(() => expect(analyze).toHaveBeenCalledWith(expect.objectContaining({ workflow: 'regions', rtProfile: 'full-halves' })))
})

it('uses the selected RT model matrix to disable an incompatible explicit backend', async () => {
  setBackend(/** @type {any} */ ({
    ...LOADS,
    listWorkflowCapabilities: async () => capabilities({
      fullRtInstalled: true, rtInstalled: true,
      rtBackends: [...cpu(), { id: 'ort-webgpu', platform: 'macOS', qualified: false,
        available: true, selectable: true, note: 'Candidate provider' }],
    }),
    listAccelerators: async () => ({ models: [
      { id: 'rtFull', backendStatus: [{ id: 'webgpu', supported: false, available: false }] },
      { id: 'rtSmall', backendStatus: [{ id: 'webgpu', supported: true, available: true }] },
    ] }),
  }))
  const screen = render(WorkflowAnalysis)
  const profile = await screen.findByLabelText('Ogkalu comic text & bubble detector: profile and page layout')
  const backend = screen.getByLabelText('Ogkalu comic text & bubble detector backend')
  await waitFor(() => expect([...backend.options].find((option) => option.value === 'ort-webgpu')?.disabled).toBe(true))
  await fireEvent.change(profile, { target: { value: 'small-whole' } })
  await waitFor(() => expect([...backend.options].find((option) => option.value === 'ort-webgpu')?.disabled).toBe(false))
})

it('says what is missing instead of only disabling Analyze', async () => {
  setBackend(/** @type {any} */ ({ ...LOADS, listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true }),
    analyzeChapterPage: vi.fn() }))
  editor.chapter = { id: 'c1', pages: [{ id: 'p1', index: 0, number: 1 }] }
  const screen = render(WorkflowAnalysis, { props: { chapterId: 'c1' } })
  expect(await screen.findByText('Not ready to analyze')).toBeTruthy()
  expect(screen.getByText(/SAM-TS-L lettering mask is not installed/)).toBeTruthy()
  expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(true)
})

it('analyzes the selected chapter page and requires exact corrected-W approval for each successive revision', async () => {
  const chapterId = 'c1'
  const page = { id: 'p1', index: 0, number: 1, status: 'unclean', regions: [], resident: true }
  editor.chapter = { id: chapterId, pages: [page], review: [] }
  editor.history = createHistory()
  let currentRegion = null
  let currentStatus = 'unclean'
  let revision = 0
  let plan = 0
  const evidence = { width: 64, height: 48, components: [{ id: 'sam-00001', pixels: 2,
    bounds: { x: 30, y: 20, w: 2, h: 1 }, rtTextIds: ['text-1'], rtBubbleIds: ['bubble-1'] }], regions: [], groupingSuggestions: [] }
  const result = { analysisId: 'a1', sourceSha256: 'source', rtProfile: 'full-halves', rtBackend: 'ort-cpu',
    samBackend: 'ort-webgpu', samWriteEligible: true, maskSha256: 'mask', sourceDataUrl: 'data:image/png;base64,AA',
    maskDataUrl: 'data:image/png;base64,AA', timingsMs: { rtLoad: 0, samLoad: 0,
      samEncoder: 0, samHead: 0, rtPage: 0 }, evidence }
  const analyze = vi.fn(async () => result)
  const prepare = vi.fn(async ({ paddingPx, correctionRevision }) => {
    plan += 1
    return { planId: `plan-${plan}`, componentId: 'sam-00001', bounds: { x: 31, y: 20, w: 2, h: 1 },
      supportPixels: 2, supportDataUrl: `data:image/png;base64,BB${plan}`, supportSha256: `support-${plan}`,
      planIdentitySha256: `identity-${plan}`, paddingPx, correctionRevision,
      sourceSha256: 'source', underlaySha256: 'underlay', renderVersion: 'bounded-ring-median-v1' }
  })
  const apply = vi.fn(async () => {
    revision += 1
    currentStatus = 'cleaned'
    currentRegion = { id: 'p1-hreview-sam-00001', pageId: 'p1', outcome: 'cleaned',
      mask: { id: `m${revision}`, textShapePatchRevision: `revision-${revision}` } }
    return { regionId: currentRegion.id, region: currentRegion, pageStatus: currentStatus }
  })
  const historyPush = vi.fn(async ({ entry }) => ({ cursor: revision, entries: [{ seq: revision, label: entry.label }] }))
  const backend = /** @type {any} */ ({
    ...LOADS,
    listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true, samInstalled: true, samWriteQualified: true }),
    verifySamTs: async () => true,
    analyzeChapterPage: analyze,
    loadComponentCorrection: async () => null,
    prepareComponentWrite: prepare,
    applyComponentWrite: apply,
    loadPages: async () => [{ ...page, status: currentStatus, regions: currentRegion ? [currentRegion] : [] }],
    historyPush,
  })
  setBackend(backend)
  const screen = render(WorkflowAnalysis, { props: { chapterId, initialPageIndex: 0 } })
  await waitFor(() => expect(screen.getByText('SHA-256 verified')).toBeTruthy())
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  await waitFor(() => expect(analyze).toHaveBeenCalledWith({ chapterId, pageIndex: 0, workflow: 'text_shape',
    rtProfile: 'full-halves', rtBackend: 'auto', samBackend: 'auto', requestId: expect.stringMatching(/^review-/) }))
  await fireEvent.click(await screen.findByRole('button', { name: /sam-00001/ }))
  await fireEvent.change(screen.getByLabelText('Padding in source pixels'), { target: { value: '3' } })
  await fireEvent.change(screen.getByLabelText('Mask correction mode'), { target: { value: 'add' } })
  const canvas = screen.getByRole('button', { name: /Source page mask correction canvas/ })
  await fireEvent.keyDown(canvas, { key: 'Enter' })
  await waitFor(() => expect(prepare).toHaveBeenCalledTimes(1))
  expect(prepare).toHaveBeenLastCalledWith(expect.objectContaining({ analysisId: 'a1', chapterId, pageIndex: 0,
    componentId: 'sam-00001', allowOutsideBubbles: false, paddingPx: 3, correctionRevision: 1,
    additions: { bounds: { x: 30, y: 19, w: 3, h: 3 }, bits: [0, 255, 0, 255, 255, 255, 0, 255, 0] } }))
  expect(screen.getByRole('button', { name: 'Apply approved component' }).disabled).toBe(true)
  // W is a source-pixel canvas over exactly the plan's bounds, drawn from
  // the plan's own support raster; no CSS mask is involved.
  const overlay = screen.container.querySelector('.page-art canvas[data-tint="write"]')
  expect(overlay.style.left).toBe(`${31 / 64 * 100}%`)
  expect(overlay.style.width).toBe('3.125%')
  expect(overlay.style.height).toBe(`${1 / 48 * 100}%`)
  expect(loadedImages).toContain('data:image/png;base64,BB1')
  expect(screen.container.querySelector('[style*="mask-image"]')).toBeNull()
  await fireEvent.click(screen.getByLabelText(/I approve writing exactly the orange pixels/))
  await fireEvent.click(screen.getByRole('button', { name: 'Apply approved component' }))
  await waitFor(() => expect(historyPush).toHaveBeenCalledTimes(1))
  expect(apply).toHaveBeenNthCalledWith(1, { planId: 'plan-1', approvedSupportSha256: 'support-1' })
  expect(await screen.findByText('Written to page 1')).toBeTruthy()
  expect(screen.getByRole('button', { name: /sam-00001/ }).textContent).toContain('Written')
  // The applied corrections are in the written patch, not pending again.
  expect(screen.container.querySelector('.pending')).toBeNull()

  await fireEvent.keyDown(canvas, { key: 'ArrowRight' })
  await fireEvent.keyDown(canvas, { key: 'Enter' })
  await waitFor(() => expect(prepare).toHaveBeenCalledTimes(2))
  await fireEvent.click(screen.getByLabelText(/I approve writing exactly the orange pixels/))
  await fireEvent.click(screen.getByRole('button', { name: 'Apply approved component' }))
  await waitFor(() => expect(historyPush).toHaveBeenCalledTimes(2))
  expect(apply).toHaveBeenNthCalledWith(2, { planId: 'plan-2', approvedSupportSha256: 'support-2' })
  expect(historyPush.mock.calls[0][0].entry.before).toMatchObject({ present: false, pageStatus: 'unclean' })
  expect(historyPush.mock.calls[0][0].entry.after.region.mask.textShapePatchRevision).toBe('revision-1')
  expect(historyPush.mock.calls[1][0].entry.before.region.mask.textShapePatchRevision).toBe('revision-1')
  expect(historyPush.mock.calls[1][0].entry.after.region.mask.textShapePatchRevision).toBe('revision-2')
})

it('keeps a CPU analysis read-only on a WebGPU-qualified host', async () => {
  editor.chapter = { id: 'c1', pages: [{ id: 'p1', index: 0, number: 1, status: 'unclean', regions: [], resident: true }] }
  const prepare = vi.fn()
  const result = { analysisId: 'cpu-analysis', sourceSha256: 'source', rtProfile: 'full-halves', rtBackend: 'ort-cpu',
    samBackend: 'ort-cpu', samWriteEligible: false, maskSha256: 'mask', sourceDataUrl: 'data:image/png;base64,AA',
    maskDataUrl: 'data:image/png;base64,AA', timingsMs: { rtLoad: 0, samLoad: 0, samEncoder: 0, samHead: 0, rtPage: 0 },
    evidence: { width: 64, height: 48, components: [{ id: 'sam-00001', pixels: 2,
      bounds: { x: 30, y: 20, w: 2, h: 1 }, rtTextIds: ['text-1'], rtBubbleIds: ['bubble-1'] }], regions: [], groupingSuggestions: [] } }
  setBackend(/** @type {any} */ ({ ...LOADS, listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true, samInstalled: true, samWriteQualified: true }),
    verifySamTs: async () => true, analyzeChapterPage: async () => result, loadComponentCorrection: async () => null, prepareComponentWrite: prepare }))
  const screen = render(WorkflowAnalysis, { props: { chapterId: 'c1', initialPageIndex: 0 } })
  await waitFor(() => expect(screen.getByText('SHA-256 verified')).toBeTruthy())
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  await fireEvent.click(await screen.findByRole('button', { name: /sam-00001/ }))
  expect(screen.getByRole('button', { name: 'Prepare write preview' }).disabled).toBe(true)
  expect(screen.getByText('Review only')).toBeTruthy()
  expect(screen.getByText(/Analyzed on the CPU/)).toBeTruthy()

  // A correction on a review-only result is still drawn, as pending, and never prepared.
  await fireEvent.change(screen.getByLabelText('Mask correction mode'), { target: { value: 'add' } })
  const canvas = screen.getByRole('button', { name: /Source page mask correction canvas/ })
  await fireEvent.keyDown(canvas, { key: 'Enter' })
  expect(prepare).not.toHaveBeenCalled()
  expect(screen.getByText(/Pending: 5 px to add, 0 px to remove/)).toBeTruthy()
  const pendingAdd = screen.container.querySelector('canvas.pending.add')
  expect(pendingAdd.style.left).toBe(`${30 / 64 * 100}%`)
  const addDraw = drawn.filter((entry) => entry.canvas === pendingAdd).at(-1)
  expect(addDraw.image.width).toBe(3)
  expect([...addDraw.image.data].filter((value, index) => index % 4 === 3 && value === 255)).toHaveLength(5)
})

it('maps correction pointers through CSS zoom at a non-unit device pixel ratio', async () => {
  editor.chapter = { id: 'c-zoom', pages: [{ id: 'p-zoom', index: 0, number: 1, status: 'unclean', regions: [], resident: true }] }
  const evidence = { width: 64, height: 48, components: [{ id: 'sam-00001', pixels: 1,
    bounds: { x: 32, y: 24, w: 1, h: 1 }, rtTextIds: [], rtBubbleIds: ['bubble-1'] }],
    regions: [], groupingSuggestions: [] }
  const result = { analysisId: 'zoom-analysis', sourceSha256: 'source', rtProfile: 'full-halves', rtBackend: 'ort-cpu',
    samBackend: 'ort-webgpu', samWriteEligible: true, maskSha256: 'mask', sourceDataUrl: 'data:image/png;base64,AA',
    maskDataUrl: 'data:image/png;base64,AA', timingsMs: { rtLoad: 0, samLoad: 0,
      samEncoder: 0, samHead: 0, rtPage: 0 }, evidence }
  const prepare = vi.fn(async () => null)
  setBackend(/** @type {any} */ ({ ...LOADS, listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true, samInstalled: true, samWriteQualified: true }),
    verifySamTs: async () => true, analyzeChapterPage: async () => result,
    loadComponentCorrection: async () => null, prepareComponentWrite: prepare }))
  const screen = render(WorkflowAnalysis, { props: { chapterId: 'c-zoom', initialPageIndex: 0 } })
  await waitFor(() => expect(screen.getByText('SHA-256 verified')).toBeTruthy())
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  await fireEvent.click(await screen.findByRole('button', { name: /sam-00001/ }))
  await fireEvent.change(screen.getByLabelText('Mask correction mode'), { target: { value: 'add' } })
  await fireEvent.input(screen.getByLabelText('Preview zoom'), { target: { value: '200' } })
  const canvas = screen.getByRole('button', { name: /Source page mask correction canvas/ })
  const image = canvas.querySelector('img')
  expect(image.style.width).toBe('128px')
  Object.defineProperty(window, 'devicePixelRatio', { configurable: true, value: 2 })
  image.getBoundingClientRect = () => ({ left: 10, top: 20, width: 128, height: 96 })
  await fireEvent.pointerDown(canvas, { pointerId: 1, clientX: 74, clientY: 68 })
  await fireEvent.pointerUp(canvas, { pointerId: 1, clientX: 74, clientY: 68 })
  await waitFor(() => expect(prepare).toHaveBeenCalledTimes(1))
  expect(prepare).toHaveBeenLastCalledWith(expect.objectContaining({
    additions: { bounds: { x: 31, y: 23, w: 3, h: 3 }, bits: [0, 255, 0, 255, 255, 255, 0, 255, 0] },
  }))
  Object.defineProperty(window, 'devicePixelRatio', { configurable: true, value: 1 })
})

it('restores persisted source-coordinate corrections before preparing the next revision', async () => {
  editor.chapter = { id: 'c-saved', pages: [{ id: 'p-saved', index: 0, number: 1, status: 'unclean', regions: [], resident: true }] }
  const evidence = { width: 64, height: 48, components: [{ id: 'sam-00001', pixels: 2,
    bounds: { x: 30, y: 20, w: 2, h: 1 }, rtTextIds: ['text-1'], rtBubbleIds: ['bubble-1'] }], regions: [], groupingSuggestions: [] }
  const result = { analysisId: 'saved-analysis', sourceSha256: 'source', rtProfile: 'full-halves', rtBackend: 'ort-cpu',
    samBackend: 'ort-webgpu', samWriteEligible: true, maskSha256: 'mask', sourceDataUrl: 'data:image/png;base64,AA',
    maskDataUrl: 'data:image/png;base64,AA', timingsMs: { rtLoad: 0, samLoad: 0, samEncoder: 0, samHead: 0, rtPage: 0 }, evidence }
  const load = vi.fn(async () => ({ regionId: 'p-saved-hreview-sam-00001',
    additions: { bounds: { x: 30, y: 20, w: 1, h: 1 }, bits: [255] },
    removals: { bounds: { x: 0, y: 0, w: 0, h: 0 }, bits: [] },
    paddingPx: 7, correctionRevision: 4, planRevision: 9 }))
  const prepare = vi.fn(async ({ paddingPx, correctionRevision }) => ({ planId: 'next-plan', componentId: 'sam-00001',
    bounds: { x: 30, y: 20, w: 2, h: 1 }, supportPixels: 2, supportDataUrl: 'data:image/png;base64,BB',
    supportSha256: 'support', planIdentitySha256: 'identity', paddingPx, correctionRevision,
    sourceSha256: 'source', underlaySha256: 'underlay', renderVersion: 'bounded-ring-median-v1' }))
  setBackend(/** @type {any} */ ({ ...LOADS, listWorkflowCapabilities: async () => capabilities({ fullRtInstalled: true, samInstalled: true, samWriteQualified: true }),
    verifySamTs: async () => true, analyzeChapterPage: async () => result, loadComponentCorrection: load, prepareComponentWrite: prepare }))
  const screen = render(WorkflowAnalysis, { props: { chapterId: 'c-saved', initialPageIndex: 0 } })
  await waitFor(() => expect(screen.getByText('SHA-256 verified')).toBeTruthy())
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  await screen.findByRole('button', { name: /sam-00001/ })
  await fireEvent.click(screen.getByRole('button', { name: /sam-00001/ }))
  await waitFor(() => expect(prepare).toHaveBeenCalledTimes(1))
  expect(load).toHaveBeenCalledWith({ analysisId: 'saved-analysis', chapterId: 'c-saved', pageIndex: 0, componentId: 'sam-00001' })
  expect(prepare).toHaveBeenLastCalledWith(expect.objectContaining({
    paddingPx: 7, correctionRevision: 4,
    additions: { bounds: { x: 30, y: 20, w: 1, h: 1 }, bits: [255] },
  }))

  const canvas = screen.getByRole('button', { name: /Source page mask correction canvas/ })
  await fireEvent.change(screen.getByLabelText('Mask correction mode'), { target: { value: 'add' } })
  await fireEvent.keyDown(canvas, { key: 'ArrowRight' })
  await fireEvent.keyDown(canvas, { key: 'Enter' })
  await waitFor(() => expect(prepare).toHaveBeenCalledTimes(2))
  expect(prepare).toHaveBeenLastCalledWith(expect.objectContaining({
    paddingPx: 7, correctionRevision: 5,
    additions: { bounds: { x: 30, y: 19, w: 4, h: 3 }, bits: [0, 0, 255, 0, 255, 255, 255, 255, 0, 0, 255, 0] },
  }))
})

it('cancels a running analysis by its request id and names the outcome', async () => {
  let reject
  const { screen, api } = chapterReview({
    evidence: {},
    backend: {
      analyzeChapterPage: vi.fn(() => new Promise((_resolve, fail) => { reject = fail })),
      cancelCapabilityAnalysis: vi.fn(async () => { reject('analysis cancelled'); return true }),
    },
  })
  await analyze(screen)
  const cancel = await screen.findByRole('button', { name: /Cancel analysis/ })
  expect(screen.getByText('Analyzing page 1 locally…')).toBeTruthy()
  expect(screen.getByLabelText('Chapter page').disabled).toBe(true)
  await fireEvent.click(cancel)
  const requestId = api.analyzeChapterPage.mock.calls[0][0].requestId
  expect(api.cancelCapabilityAnalysis).toHaveBeenCalledWith(requestId)
  expect(await screen.findByText('Analysis cancelled')).toBeTruthy()
  expect(screen.getByText(/Nothing was saved/)).toBeTruthy()
  expect(screen.queryByRole('button', { name: /Cancel analysis/ })).toBeNull()
  expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(false)
  // A second run mints a fresh request id.
  await fireEvent.click(screen.getByRole('button', { name: 'Analyze for review' }))
  expect(api.analyzeChapterPage.mock.calls[1][0].requestId).not.toBe(requestId)
})

it('cancels a running analysis when the review closes', async () => {
  const { screen, api } = chapterReview({ evidence: {}, backend: { analyzeChapterPage: vi.fn(() => new Promise(() => {})) } })
  await analyze(screen)
  await screen.findByRole('button', { name: /Cancel analysis/ })
  const requestId = api.analyzeChapterPage.mock.calls[0][0].requestId
  screen.unmount()
  expect(api.cancelCapabilityAnalysis).toHaveBeenCalledWith(requestId)
})

it('names model and page outcomes instead of printing the native error', async () => {
  const { screen } = chapterReview({ evidence: {}, backend: {
    analyzeChapterPage: vi.fn(async () => { throw 'The SAM-TS-L lettering mask graphs are not installed' }),
  } })
  await analyze(screen)
  expect(await screen.findByText('Model missing')).toBeTruthy()
  // The native sentence is kept for support, inside the collapsed detail only.
  expect(screen.getByText('The SAM-TS-L lettering mask graphs are not installed').closest('details')).toBeTruthy()
})

it('reports nothing found as its own outcome', async () => {
  const { screen } = chapterReview({ evidence: {} })
  await analyze(screen)
  expect(await screen.findByText('Nothing found')).toBeTruthy()
})

it('keeps its own outside-bubble permission, off by default, and leaves Auto clean alone', async () => {
  editor.toolParams.autoClean = { ...editor.toolParams.autoClean, outsideBubbles: 'review' }
  const { screen, api } = chapterReview({ evidence: { components: [outside] } })
  await analyze(screen)
  const row = await screen.findByRole('button', { name: /sam-00002/ })
  expect(row.textContent).toContain('Held')
  await fireEvent.click(row)
  expect(screen.getByText('Held by policy')).toBeTruthy()
  const permission = screen.getByLabelText('Allow components outside speech bubbles')
  expect(permission.checked).toBe(false)
  expect(screen.getByRole('button', { name: 'Prepare write preview' }).disabled).toBe(true)

  await fireEvent.click(permission)
  expect(editor.toolParams.autoClean.outsideBubbles).toBe('review')
  expect(screen.queryByText('Held by policy')).toBeNull()
  const prepare = screen.getByRole('button', { name: 'Prepare write preview' })
  expect(prepare.disabled).toBe(false)
  await fireEvent.click(prepare)
  await waitFor(() => expect(api.prepareComponentWrite).toHaveBeenCalledWith(expect.objectContaining({
    componentId: 'sam-00002', allowOutsideBubbles: true })))
})

it('offers Rebuild preview for a stale plan and reports a failed fill after apply', async () => {
  const applyComponentWrite = vi.fn(async () => { throw 'No surrounding pixels available for bounded fill' })
  let stale = true
  const { screen, api } = chapterReview({
    evidence: { components: [bubbled] },
    backend: {
      applyComponentWrite,
      loadPages: async () => [{ index: 0, status: 'unclean', regions: [] }],
    },
  })
  api.prepareComponentWrite.mockImplementationOnce(async () => {
    if (stale) { stale = false; throw 'Visible page changed since preview; prepare the component again' }
  })
  await analyze(screen)
  await fireEvent.click(await screen.findByRole('button', { name: /sam-00001/ }))
  await fireEvent.click(screen.getByRole('button', { name: 'Prepare write preview' }))
  expect(await screen.findByText('Preview out of date')).toBeTruthy()
  await fireEvent.click(screen.getByRole('button', { name: 'Rebuild preview' }))
  await waitFor(() => expect(api.prepareComponentWrite).toHaveBeenCalledTimes(2))
  expect(await screen.findByText(/W: 2 source px/)).toBeTruthy()
  await fireEvent.click(screen.getByLabelText(/I approve writing exactly the orange pixels/))
  await fireEvent.click(screen.getByRole('button', { name: 'Apply approved component' }))
  expect(await screen.findByText('Fill could not be rebuilt')).toBeTruthy()
})

it('names the evidence reason, keeps plain candidates unflagged, and selects tiny ones from the list or the page', async () => {
  const tiny = { id: 'sam-00003', pixels: 1, bounds: { x: 50, y: 40, w: 1, h: 1 }, rtTextIds: [], rtBubbleIds: ['b'],
    reviewReasons: ['unassignedComponent'], groupId: 'tg-000000000002' }
  const groups = [
    { id: 'tg-000000000001', order: 0, disposition: 'clean', componentIds: ['sam-00001'], reasons: [] },
    { id: 'tg-000000000002', order: 1, disposition: 'candidate', componentIds: ['sam-00003'], reasons: ['unassignedComponent'] },
  ]
  const { screen } = chapterReview({ evidence: { components: [{ ...bubbled, reviewReasons: [], groupId: 'tg-000000000001' }, tiny],
    regions: [{ id: 'rt-0000', kind: 'text_free', bounds: { x: 28, y: 18, w: 8, h: 6 }, componentIds: ['sam-00001'], detectorOnly: false },
      { id: 'ctd-0000', kind: 'ctd_text', bounds: { x: 2, y: 2, w: 8, h: 6 }, componentIds: [], detectorOnly: true }],
    groups } })
  await analyze(screen)
  const list = await screen.findByRole('group', { name: 'Review candidates' })
  const buttons = [...list.querySelectorAll('button')]
  expect(buttons.map((button) => button.tabIndex)).toEqual([0, -1, -1, -1])
  // A candidate awaiting selection is not a problem: only a reason flags it.
  expect(screen.getByText('Components awaiting selection')).toBeTruthy()
  expect(buttons[0].querySelector('.tag.reason')).toBeNull()
  expect(buttons[1].querySelector('.tag.reason').textContent).toBe('No text box claims it')
  expect(screen.getByText('1 flagged with a reason')).toBeTruthy()
  expect(screen.getByText('2 text groups')).toBeTruthy()
  expect(buttons[3].textContent).toContain('CTD text box')
  expect(screen.queryByText(/uncertain/i)).toBeNull()

  buttons[0].focus()
  await fireEvent.keyDown(buttons[0], { key: 'ArrowDown' })
  expect(document.activeElement).toBe(buttons[1])
  await fireEvent.keyDown(buttons[1], { key: 'End' })
  expect(document.activeElement).toBe(buttons[3])
  await fireEvent.click(buttons[1])
  expect(buttons[1].getAttribute('aria-pressed')).toBe('true')
  expect(screen.getByText(/No text box claims this lettering, so it is a candidate, not a detected problem/)).toBeTruthy()
  expect(screen.getByText('Text group tg-000000000002: 1 component. Detection lists the group and leaves it untouched.')).toBeTruthy()
  await fireEvent.click(buttons[0])
  expect(screen.getByText('Text group tg-000000000001: 1 component. Detection cleans the group as one layer.')).toBeTruthy()
  expect(screen.queryByText(/No text box claims this lettering/)).toBeNull()
  await fireEvent.click(buttons[1])

  // Inspect mode: a click on the page picks the smallest box under it.
  const canvas = screen.getByRole('button', { name: /Source page mask correction canvas/ })
  canvas.querySelector('img').getBoundingClientRect = () => ({ left: 0, top: 0, width: 64, height: 48 })
  await fireEvent.pointerDown(canvas, { pointerId: 1, clientX: 31.5, clientY: 20.5 })
  await waitFor(() => expect(buttons[0].getAttribute('aria-pressed')).toBe('true'))
  expect(screen.container.querySelector('.locator').classList.contains('detector')).toBe(false)
  await fireEvent.click(buttons[2])
  expect(screen.container.querySelector('.locator').classList.contains('detector')).toBe(true)
  expect(screen.getByText(/Detector boxes only locate text/)).toBeTruthy()
})

it('reaches 1:1 source pixels on a large page and fits it by default', async () => {
  const { screen } = chapterReview({ evidence: { width: 4000, height: 6000, components: [bubbled] } })
  await analyze(screen)
  const canvas = await screen.findByRole('button', { name: /Source page mask correction canvas/ })
  const image = canvas.querySelector('img')
  expect(image.style.width).toBe('600px')
  expect(image.classList.contains('crisp')).toBe(false)
  await fireEvent.click(screen.getByRole('button', { name: /one source pixel per screen point/ }))
  expect(image.style.width).toBe('4000px')
  await fireEvent.input(screen.getByLabelText('Preview zoom'), { target: { value: '400' } })
  expect(image.style.width).toBe('16000px')
  expect(image.classList.contains('crisp')).toBe(true)
  await fireEvent.click(screen.getByRole('button', { name: /Fit the page to the view/ }))
  expect(image.style.width).toBe('600px')
})

it('names the declined reason for a JPEG analysis on the qualified backend', async () => {
  const { screen } = chapterReview({ evidence: { components: [bubbled] },
    result: { samWriteEligible: false, sourceDataUrl: 'data:image/jpeg;base64,AA' } })
  await analyze(screen)
  expect(await screen.findByText(/This page is a JPEG/)).toBeTruthy()
})

/* ------------------------------------------------------------------ */
/* Cloud evidence                                                      */
/* ------------------------------------------------------------------ */

/** The cloud half of a backend: a Modal endpoint selected with its key stored, one proposal, one result. */
function cloudBackend(analysis) {
  setCloudAllowed(true)
  return {
    readSettings: async () => ({ cloudEngines: 'allowed' }),
    readInferenceConfig: async () => ({ selectedTarget: { type: 'modal', profile_id: 'p1' },
      modalProfiles: { p1: { name: 'Studio A100' } }, beamProfiles: {} }),
    getCloudSecretSummary: async () => ({ present: true }),
    onRemoteAnalysis: async () => () => {},
    listRemoteAnalysisCapabilities: async () => ({ capabilities: [{ capability: 'text_mask_sam_ts@1',
      graph_sha256s: ['a'.repeat(64)], model_revision: 'c'.repeat(40) }] }),
    proposeRemoteAnalysis: vi.fn(async () => ({ proposalId: 'prop-1', provider: 'modal', profileId: 'p1',
      profileName: 'Studio A100', capability: 'text_mask_sam_ts@1', graphSha256s: ['a'.repeat(64)],
      modelRevision: 'c'.repeat(40), pages: 1, totalTilePixels: 3072, totalEncodedBytes: 900,
      includesSurroundingArt: true, costEstimateUsd: null, expiresAtMs: Date.now() + 300000,
      tiles: [{ rect: { x: 0, y: 0, width: 64, height: 48 } }] })),
    confirmRemoteAnalysis: vi.fn(async () => analysis),
    cancelRemoteAnalysis: vi.fn(async () => true),
    getRemoteAnalysisStatus: async () => null,
  }
}

function remoteAnalysis(evidence, extra = {}) {
  return { analysisId: 'remote:modal:text_mask_sam_ts@1:abc', sourceSha256: 'source', rtProfile: null,
    rtBackend: null, samBackend: 'remote', remoteSource: 'remote:modal:text_mask_sam_ts@1',
    workflow: 'text_mask_sam_ts@1', samWriteEligible: false, maskSha256: 'mask',
    sourceDataUrl: 'data:image/png;base64,AA', maskDataUrl: 'data:image/png;base64,CC',
    timingsMs: { rtLoad: 0, rtPage: 0, samLoad: 0, samEncoder: 0, samHead: 0 },
    evidence: { width: 64, height: 48, components: [], regions: [], groupingSuggestions: [], ...evidence }, ...extra }
}

it('routes Analyze to the cloud GPU from the backend choice, with RT-DETR as the companion of SAM', async () => {
  const backend = cloudBackend(remoteAnalysis({}))
  backend.listRemoteAnalysisCapabilities = async () => ({ capabilities: ['text_mask_sam_ts@1', 'text_regions_rt@1']
    .map((capability) => ({ capability, graph_sha256s: ['a'.repeat(64)], model_revision: 'c'.repeat(40) })) })
  const { screen, api } = chapterReview({ evidence: {}, backend })
  const sam = /** @type {HTMLSelectElement} */ (await screen.findByLabelText('SAM-TS-L lettering mask backend'))
  const cloudOption = /** @type {HTMLOptionElement} */ (await waitFor(() => {
    const option = sam.querySelector('option[value="cloud"]')
    expect(option?.disabled).toBe(false)
    return option
  }))
  expect(cloudOption.textContent).toBe('☁ Cloud GPU · review only')
  await fireEvent.change(sam, { target: { value: 'cloud' } })
  // One review sends to one place: RT-DETR goes with it.
  expect(/** @type {HTMLSelectElement} */ (screen.getByLabelText('Ogkalu comic text & bubble detector backend')).value).toBe('cloud')
  await analyze(screen)
  await screen.findByRole('region', { name: /Send page 1 to your cloud GPU/ })
  expect(api.analyzeChapterPage).not.toHaveBeenCalled()
  expect(api.proposeRemoteAnalysis).toHaveBeenCalledWith(expect.objectContaining({
    capability: 'text_mask_sam_ts@1', companion: 'text_regions_rt@1' }))

  // A CTD workflow cannot go to the cloud: the choice leaves both stages.
  await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  await fireEvent.change(screen.getByLabelText('Workflow'), { target: { value: 'ctd_mask' } })
  await waitFor(() => expect(sam.value).toBe('auto'))
  expect(/** @type {HTMLOptionElement} */ (sam.querySelector('option[value="cloud"]')).disabled).toBe(true)
  expect(screen.getByLabelText('Workflow').textContent).toContain('CTD with SAM-TS-L lettering mask pixels')
})

it('offers the cloud choice disabled, with the reason in its name, while cloud engines are off', async () => {
  const { screen } = chapterReview({ evidence: {} })
  const sam = /** @type {HTMLSelectElement} */ (await screen.findByLabelText('SAM-TS-L lettering mask backend'))
  const option = /** @type {HTMLOptionElement} */ (sam.querySelector('option[value="cloud"]'))
  expect(option.disabled).toBe(true)
  expect(option.textContent).toBe('☁ Cloud GPU · not set up')
})

async function analyzeInCloud(screen) {
  await fireEvent.click(await screen.findByRole('button', { name: 'Analyze with cloud GPU' }))
  await fireEvent.click(await screen.findByLabelText(/I have the rights/))
  await fireEvent.click(screen.getByLabelText(/I have reviewed and accept/))
  await fireEvent.click(screen.getByRole('button', { name: 'Send to cloud GPU' }))
}

it('shows cloud evidence in the same review, labelled, and never prepares or applies from it', async () => {
  const analysis = remoteAnalysis({ components: [bubbled, outside] })
  const { screen, api } = chapterReview({ evidence: {}, backend: cloudBackend(analysis) })
  await analyzeInCloud(screen)
  expect(await screen.findByText('Cloud evidence from Studio A100')).toBeTruthy()
  const declined = screen.container.querySelector('[data-outcome="declined"]')
  expect(declined.dataset.reason).toBe('remote')
  expect(declined.textContent).toContain('Review only')
  expect(declined.textContent).toContain('Cloud results are for review. They cannot prepare a component write.')
  expect(screen.queryByText('Components can be prepared for writing.', { exact: false })).toBeNull()

  // No write controls: no outside-bubble permission, nothing held by it.
  expect(screen.queryByLabelText('Allow components outside speech bubbles')).toBeNull()
  expect(screen.getByRole('button', { name: /sam-00002/ }).textContent).not.toContain('Held')

  await fireEvent.click(screen.getByRole('button', { name: /sam-00001/ }))
  const prepare = screen.getByRole('button', { name: 'Prepare write preview' })
  const apply = screen.getByRole('button', { name: 'Apply approved component' })
  expect(prepare.disabled).toBe(true)
  expect(apply.disabled).toBe(true)
  // The reason is text beside the controls, tied to them, not a colour.
  for (const button of [prepare, apply]) {
    const reason = document.getElementById(button.getAttribute('aria-describedby'))
    expect(reason.textContent).toBe('Cloud results are for review. They cannot prepare a component write.')
  }
  expect(screen.queryByLabelText('Mask correction mode')).toBeNull()
  expect(api.loadComponentCorrection).not.toHaveBeenCalled()
  expect(api.prepareComponentWrite).not.toHaveBeenCalled()

  // The evidence mask is a source-pixel canvas tint; the W legend is absent.
  expect(screen.container.querySelector('canvas[data-tint="evidence"]')).toBeTruthy()
  expect(loadedImages).toContain('data:image/png;base64,CC')
  expect(screen.queryByText('W, will be written')).toBeNull()
  expect(screen.getByText('☁ SAM-TS-L lettering mask on Studio A100, Modal')).toBeTruthy()
})

it('renders cloud evidence with no components and no page image', async () => {
  const analysis = remoteAnalysis({}, { sourceDataUrl: null, maskDataUrl: null, maskSha256: null, timingsMs: {} })
  const { screen } = chapterReview({ evidence: {}, backend: cloudBackend(analysis) })
  await analyzeInCloud(screen)
  expect(await screen.findByText('Nothing found')).toBeTruthy()
  expect(screen.getByText(/This result has no page image/)).toBeTruthy()
  const canvas = screen.getByRole('button', { name: /Source page mask correction canvas/ })
  expect(canvas.querySelector('img')).toBeNull()
  const surface = canvas.querySelector('[data-surface]')
  expect(surface.style.width).toBe('64px')
  expect(surface.style.height).toBe('48px')
  expect(screen.container.querySelector('canvas[data-tint]')).toBeNull()
  surface.getBoundingClientRect = () => ({ left: 0, top: 0, width: 64, height: 48 })
  await fireEvent.pointerDown(canvas, { pointerId: 1, clientX: 10, clientY: 10 })
})

it('holds the page still while a cloud proposal is open, and local analysis works after it', async () => {
  const { screen, api } = chapterReview({ evidence: { components: [bubbled] }, backend: cloudBackend(remoteAnalysis({})) })
  await waitFor(() => expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(false))
  await fireEvent.click(await screen.findByRole('button', { name: 'Analyze with cloud GPU' }))
  await screen.findByRole('region', { name: /Send page 1 to your cloud GPU/ })
  expect(screen.getByLabelText('Chapter page').disabled).toBe(true)
  expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(true)
  await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  await waitFor(() => expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(false))
  expect(screen.getByLabelText('Chapter page').disabled).toBe(false)
  await analyze(screen)
  expect(await screen.findByRole('button', { name: /sam-00001/ })).toBeTruthy()
  expect(api.analyzeChapterPage).toHaveBeenCalledTimes(1)
})

it('reads an installed runtime that does not load as not ready, and names why', async () => {
  const diagnostics = vi.fn(async () => ({ components: [{ name: 'onnxruntime', available: false,
    detail: 'dlopen failed', reasonKey: 'diagnostics.runtime.quarantined' }] }))
  const { screen, api } = chapterReview({ evidence: {}, backend: { diagnostics } })
  expect(await screen.findByText('Not ready to analyze')).toBeTruthy()
  expect(screen.getByText('ONNX Runtime is installed but does not load. The ONNX Runtime is quarantined, so the system refused to load it.')).toBeTruthy()
  expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(true)
  expect(api.analyzeChapterPage).not.toHaveBeenCalled()

  // Once it loads, a refresh makes the page analyzable; a loaded runtime is not asked again.
  diagnostics.mockImplementation(LOADS.diagnostics)
  await fireEvent.click(screen.getByRole('button', { name: 'Refresh readiness' }))
  await waitFor(() => expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(false))
  expect(screen.queryByText('Not ready to analyze')).toBeNull()
  await fireEvent.click(screen.getByRole('button', { name: 'Refresh readiness' }))
  await waitFor(() => expect(screen.getByRole('button', { name: 'Refresh readiness' }).disabled).toBe(false))
  expect(diagnostics).toHaveBeenCalledTimes(2)
})

it('does not call a runtime ready when its load could not be checked', async () => {
  const { screen } = chapterReview({ evidence: {}, backend: { diagnostics: vi.fn(async () => { throw 'not allowed' }) } })
  expect(await screen.findByText(/Could not check that ONNX Runtime loads on this computer/)).toBeTruthy()
  expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(true)
})

it('says it is checking the runtime while that answer is out, without a warning', async () => {
  let answer
  const { screen } = chapterReview({ evidence: {}, backend: { diagnostics: () => new Promise((resolve) => { answer = resolve }) } })
  expect(await screen.findByText('Checking that ONNX Runtime loads on this computer…')).toBeTruthy()
  expect(screen.queryByText('Not ready to analyze')).toBeNull()
  expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(true)
  answer(await LOADS.diagnostics())
  await waitFor(() => expect(screen.getByRole('button', { name: 'Analyze for review' }).disabled).toBe(false))
})

// Settings asks for the small RT-DETR only while the full graph is not
// imported, so a review opened after following Settings must start on the
// profile that is here rather than refuse with "not installed".
it('starts on the small RT profile when only the small graph is here', async () => {
  const { screen, api } = chapterReview({ evidence: {}, caps: { fullRtInstalled: false, rtInstalled: true } })
  const profile = /** @type {HTMLSelectElement} */ (await screen.findByLabelText('Ogkalu comic text & bubble detector: profile and page layout'))
  await waitFor(() => expect(profile.value).toBe('small-whole'))
  await analyze(screen)
  await waitFor(() => expect(api.analyzeChapterPage).toHaveBeenCalledWith(expect.objectContaining({ rtProfile: 'small-whole' })))
})

it('keeps the RT profile the user chose across a readiness refresh', async () => {
  let caps = { fullRtInstalled: true, rtInstalled: true }
  const { screen } = chapterReview({ evidence: {}, backend: {
    listWorkflowCapabilities: async () => capabilities({ samInstalled: true, samWriteQualified: true, ...caps }) } })
  const profile = /** @type {HTMLSelectElement} */ (await screen.findByLabelText('Ogkalu comic text & bubble detector: profile and page layout'))
  const refresh = await screen.findByRole('button', { name: 'Refresh readiness' })
  await waitFor(() => expect(refresh.disabled).toBe(false))
  expect(profile.value).toBe('full-halves')

  // Not chosen yet: the profile follows what is installed.
  caps = { fullRtInstalled: false, rtInstalled: true }
  await fireEvent.click(refresh)
  await waitFor(() => expect(profile.value).toBe('small-whole'))

  // Chosen: a refresh leaves it alone, even where the other one would run.
  await fireEvent.change(profile, { target: { value: 'full-halves' } })
  await fireEvent.click(refresh)
  await waitFor(() => expect(refresh.disabled).toBe(false))
  expect(profile.value).toBe('full-halves')
  expect(await screen.findByText('Not ready to analyze')).toBeTruthy()
})
