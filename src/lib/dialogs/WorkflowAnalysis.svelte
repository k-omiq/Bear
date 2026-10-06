<script>
  /**
   * The text-shaped review (with a chapter), and the same analysis without one
   * in Settings.
   *
   * With a chapter, a selected SAM component can be written, one at a time,
   * after the user approves its exact write support W. The preview is the
   * source image with three raster layers in source pixels: the SAM evidence
   * mask, the prepared W, and the pending add/remove corrections that are not
   * in W yet. Boxes are locators only and never grant a pixel.
   *
   * Every refusal is shown as a named outcome from `workflowoutcome.js`,
   * never as the backend's own sentence. The outside-bubble permission is this
   * dialog's own and is not saved: it does not touch the legacy Auto clean
   * setting of the same name.
   *
   * A chapter review can also send the current page to the user's own cloud
   * GPU (`CloudAnalysis.svelte`), after consent. That evidence is shown in
   * this same view and is review-only: nothing prepares or applies from it.
   *
   * The two rasters from the backend (the SAM evidence mask and a prepared W)
   * are drawn by `MaskTint.svelte` into source-pixel canvases, not with CSS
   * masking, which the macOS WebKit the app ships on lacks unprefixed.
   */
  import { onDestroy, onMount, tick, untrack } from 'svelte'
  import { getBackend } from '../api/backend.js'
  import { chooseFolder, chooseImage, chooseOnnx } from '../api/folder.js'
  import { WORKFLOW_PRESETS } from '../model/pipelines.js'
  import { editor, applyRegionState, recordRegionEdit, recordBackgroundRegionEdit } from '../state/editor.svelte.js'
  import { cloudUsable } from '../state/cloud.svelte.js'
  import { t } from '../i18n/index.js'
  import { Button } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import CloudAnalysis, { capabilityKeyOf, providerKeyOf } from './CloudAnalysis.svelte'
  import MaskTint from './MaskTint.svelte'
  import WorkflowOutcome from './WorkflowOutcome.svelte'
  import { declineReasonOf, hitTest, isRemoteAnalysis, outcomeOf, readinessKeyOf, runtimeLoadOf } from './workflowoutcome.js'

  let { chapterId = null, initialPageIndex = 0, initialWorkflow = null, initialRtProfile = null, initialCloud = false } = $props()
  const uid = $props.id()

  const PRESET_KEYS = {
    ctd: 'workflow.preset.ctd',
    ctd_regions: 'workflow.preset.ctdRegions',
    ctd_mask: 'workflow.preset.ctdMask',
    ctd_text_shape: 'workflow.preset.ctdTextShape',
    regions: 'workflow.preset.regions',
    mask: 'workflow.preset.mask',
    text_shape: 'workflow.preset.textShape',
  }
  const REGION_KIND_KEYS = {
    bubble_context: 'workflow.regionKind.bubbleContext',
    text_bubble: 'workflow.regionKind.textBubble',
    text_free: 'workflow.regionKind.textFree',
    ctd_text: 'workflow.regionKind.ctdText',
  }
  /**
   * A component's review reasons (`cleaner_core::text_groups::ReviewReason`),
   * each a statement about the evidence. A component with none is a candidate
   * awaiting selection, not a problem. `maskMissingUnderTextBox` belongs to a
   * box, which the region list already tags as having no SAM pixels.
   */
  const REASON_KEYS = {
    unassignedComponent: { tag: 'workflow.tag.unassigned', note: 'workflow.panel.unassigned' },
    isolatedComponent: { tag: 'workflow.tag.isolated', note: 'workflow.panel.isolated' },
    crossesBalloon: { tag: 'workflow.tag.crossesBalloon', note: 'workflow.panel.crossesBalloon' },
  }
  /** @param {{ reviewReasons?: string[] }} component */
  const reasonsOf = (component) => (component.reviewReasons ?? []).filter((reason) => REASON_KEYS[reason])
  /** Percent of source pixels. 800% is eight screen points per source pixel. */
  const MAX_ZOOM = 800
  /** Display-only click tolerance around a component's box, in screen points. */
  const HIT_TOLERANCE = 4
  /**
   * A cloud GPU runs RT-DETR v2 full and SAM-TS-L, never CTD. Opened on the
   * cloud choice, a CTD workflow starts on the same stages without it.
   */
  const CLOUD_WORKFLOW = { ctd: 'text_shape', ctd_regions: 'regions', ctd_mask: 'mask', ctd_text_shape: 'text_shape' }
  /** Mirrors the backend's correction-raster bound; a larger pending tint is not drawn. */
  const MAX_TINT_PIXELS = 16 * 1024 * 1024

  let capabilities = $state(null)
  let modelBackendRows = $state(null)
  /**
   * Whether the installed runtime loads, as `diagnostics` answers it. Asked
   * after every readiness read that finds the runtime, until it has loaded:
   * a loaded library stays loaded for the life of the process.
   *
   * @type {{state: 'checking'|'loaded'|'unchecked', reasonKey?: undefined}|{state: 'failed', reasonKey: string}}
   */
  let runtimeLoad = $state({ state: 'checking' })
  /** Whether the review opens with the cloud GPU chosen: only for a chapter, and only when it is usable. */
  const startInCloud = untrack(() => Boolean(initialCloud && chapterId && cloudUsable()))
  let workflow = $state(untrack(() => {
    const chosen = initialWorkflow ?? (chapterId ? 'text_shape' : 'regions')
    return startInCloud ? (CLOUD_WORKFLOW[chosen] ?? chosen) : chosen
  }))
  let rtProfile = $state(untrack(() => startInCloud ? 'full-halves' : (initialRtProfile ?? 'full-halves')))
  /** Set once the user picks a profile; readiness answers never move it after that. */
  let rtProfileChosen = untrack(() => initialRtProfile !== null)
  let sourcePath = $state('')
  // Native Review resolves `auto` against the saved preference for the
  // selected RT graph or SAM model. A choice here overrides that run only.
  // `cloud` is the review's cloud choice (chapter reviews only): Analyze then
  // proposes the page to the cloud GPU instead of running it here.
  let rtBackend = $state(startInCloud ? 'cloud' : 'auto')
  let samBackend = $state(startInCloud ? 'cloud' : 'auto')
  let result = $state(null)
  let focus = $state(null)
  let busy = $state(false)
  let verified = $state(null)
  let prepared = $state(null)
  let approved = $state(false)
  let pageIndex = $state(untrack(() => initialPageIndex))
  let paddingPx = $state(0)
  let brushRadius = $state(1)
  let correctionMode = $state('inspect')
  let correctionRevision = $state(0)
  let correctionCursor = $state(null)
  let previewImage = $state(null)
  let viewport = $state(null)
  let runActions = $state(null)
  let addCanvas = $state(null)
  let removeCanvas = $state(null)
  let viewportWidth = $state(0)
  /** Percent of source pixels, or null for fit-to-width. */
  let zoomPct = $state(null)
  /** The running analysis: its request id, so Cancel can name it. */
  let analyzing = $state(null)
  /** Outcome of the last run, readiness or model action: shown by the Analyze row. */
  let analysisOutcome = $state(null)
  /** Outcome of the last prepare, apply or load: shown in the component panel. */
  let componentOutcome = $state(null)
  /** This review's own outside-bubble permission. Default off, never saved. */
  let allowOutside = $state(false)
  /** A cloud analysis is between its entry and its outcome: the page is held still. */
  let cloudActive = $state(false)
  /** Where the shown cloud evidence came from: provider, endpoint name, model. */
  let cloudMeta = $state(null)
  let written = $state(new Set())
  let pendingBox = $state(null)
  /** The correction revision already in a written patch: not pending, though still in the sets. */
  let settledRevision = $state(null)
  let pendingVersion = $state(0)
  let addedCount = $state(0)
  let removedCount = $state(0)
  let additionPixels = new Set()
  let removalPixels = new Set()
  let selectionRequest = 0
  let prepareSeq = 0
  let runToken = 0
  let destroyed = false
  let drawingCorrection = false
  let lastCorrectionPoint = null

  const preset = $derived(WORKFLOW_PRESETS.find((entry) => entry.id === workflow))
  const selectedComponent = $derived(result?.evidence.components.find((entry) => entry.id === focus) ?? null)
  const selectedRegion = $derived(result?.evidence.regions.find((entry) => entry.id === focus) ?? null)
  const selectedBounds = $derived(selectedComponent?.bounds ?? selectedRegion?.bounds ?? null)
  const chapterPages = $derived(editor.chapter?.id === chapterId ? (editor.chapter.pages ?? []) : [])
  const pageNumber = $derived(chapterPages.find((page) => page.index === pageIndex)?.number ?? pageIndex + 1)
  /** A CTD workflow cannot go to the cloud GPU; the others can, from a chapter. */
  const cloudWorkflow = $derived(Boolean(chapterId && preset && !preset.needs.includes('ctd')))
  /**
   * Analyze goes to the cloud GPU. One review sends to one place, so the
   * choice holds for every stage the workflow needs: with both, SAM-TS-L is
   * the capability and RT-DETR v2 full its companion, in one consent.
   */
  const cloudRoute = $derived(Boolean(cloudWorkflow && (
    (preset.needs.includes('rt') && rtBackend === 'cloud') || (preset.needs.includes('sam') && samBackend === 'cloud'))))
  const cloudRequest = $derived(cloudRoute ? {
    capability: preset.needs.includes('sam') ? 'text_mask_sam_ts@1' : 'text_regions_rt@1',
    companion: preset.needs.includes('sam') && preset.needs.includes('rt') ? 'text_regions_rt@1' : null,
  } : null)
  const notReadyKey = $derived(cloudRoute ? (cloudUsable() ? null : 'workflow.ready.cloud') : (
    readinessKeyOf(capabilities, preset, { rtProfile, rtBackend, samBackend, verified, load: runtimeLoad.state }) ??
    (preset?.needs.includes('rt') && rtBackend !== 'auto' && !rtBackendSelectable(rtBackend) ? 'workflow.ready.backend' : null)
  ))
  const canAnalyze = $derived(Boolean(capabilities && !busy && !analyzing && !cloudActive && !notReadyKey && (chapterId || sourcePath.trim())))
  /** @type {any} */
  let cloudPanel = $state(null)
  const isRemote = $derived(isRemoteAnalysis(result))
  let fullRtDownloading = $state(false)
  const declineReason = $derived(chapterId && result ? declineReasonOf(result, capabilities) : null)
  const componentHeld = $derived(Boolean(selectedComponent && !isRemote && !allowOutside && !selectedComponent.rtBubbleIds?.length))
  const componentCanWrite = $derived(Boolean(chapterId && selectedComponent && !isRemote && !componentHeld))
  const canPrepare = $derived(Boolean(componentCanWrite && focus?.startsWith('sam-') &&
    capabilities?.samWriteQualified && result?.samWriteEligible && result?.analysisId))
  const pendingVisible = $derived(addedCount + removedCount > 0 &&
    correctionRevision !== (prepared?.correctionRevision ?? settledRevision))
  const sourceWidth = $derived(result?.evidence.width ?? 0)
  const fitScale = $derived(sourceWidth ? Math.min(1, Math.max(1, (viewportWidth || 624) - 24) / sourceWidth) : 1)
  const scale = $derived(zoomPct == null ? fitScale : zoomPct / 100)
  const zoomMin = $derived(Math.max(5, Math.floor(Math.min(100, fitScale * 100) / 5) * 5))
  const flaggedCount = $derived(result?.evidence.components.filter((entry) => reasonsOf(entry).length > 0).length ?? 0)
  const groupsById = $derived(new Map((result?.evidence.groups ?? []).map((group) => [group.id, group])))
  const selectedGroup = $derived(selectedComponent?.groupId ? groupsById.get(selectedComponent.groupId) ?? null : null)
  const rovingId = $derived(focus ?? result?.evidence.components[0]?.id ?? result?.evidence.regions[0]?.id ?? null)
  const shownAnalysisOutcome = $derived(
    analysisOutcome ??
    (!analyzing && capabilities && notReadyKey && notReadyKey !== 'workflow.ready.runtimeChecking'
      ? { kind: 'notReady', reason: notReadyKey, params: { reasonKey: runtimeLoad.reasonKey ?? 'diagnostics.runtime.unloadable' } }
      : null) ??
    (result && !result.evidence.components.length && !result.evidence.regions.length ? { kind: 'undiscovered' } : null),
  )
  const shownComponentOutcome = $derived(componentOutcome ?? (componentHeld && chapterId ? { kind: 'held' } : null))

  /* ---------------------------------------------------------------- */
  /* Readiness and model files                                         */
  /* ---------------------------------------------------------------- */

  async function refresh() {
    const next = await getBackend().listWorkflowCapabilities()
    capabilities = next
    // The workflow capability list spans both RT graphs. The per-model matrix
    // decides whether an explicit backend can run the selected graph.
    modelBackendRows = await getBackend().listAccelerators?.().then((value) => value.models ?? null).catch(() => null) ?? null
    // Settings asks for the small RT-DETR only while the full graph is not
    // imported, so until the user picks, start on the profile that is here.
    if (!rtProfileChosen) rtProfile = !next.fullRtInstalled && next.rtInstalled ? 'small-whole' : 'full-halves'
    if (!next.runtimeInstalled) runtimeLoad = { state: 'checking' }
    else if (runtimeLoad.state !== 'loaded') {
      runtimeLoad = { state: 'checking' }
      runtimeLoad = await checkRuntimeLoad()
    }
    verified = next.samInstalled ? await getBackend().verifySamTs().catch(() => false) : null
  }

  /** The same load an analysis makes; a refused question is `unchecked`, not loaded. */
  async function checkRuntimeLoad() {
    try {
      return runtimeLoadOf(await getBackend().diagnostics())
    } catch {
      return { state: 'unchecked' }
    }
  }

  onMount(() => {
    refresh().catch((cause) => { if (!destroyed) analysisOutcome = outcomeOf(cause, 'refresh') })
    return getBackend().subscribe?.((event) => {
      if (event.type === 'model-progress' && event.id === 'fullRt' && event.done) {
        fullRtDownloading = false
        refresh().catch((cause) => { if (!destroyed) analysisOutcome = outcomeOf(cause, 'refresh') })
      }
    })
  })

  onDestroy(() => {
    destroyed = true
    // Closing the dialog is not a reason to keep a page's inference running.
    if (analyzing) Promise.resolve(getBackend().cancelCapabilityAnalysis?.(analyzing.requestId)).catch(() => {})
  })

  /**
   * @param {() => Promise<unknown>} task
   * @param {'models'|'refresh'} phase
   */
  async function modelTask(task, phase = 'models') {
    busy = true
    analysisOutcome = null
    try { await task() } catch (cause) { analysisOutcome = outcomeOf(cause, phase) }
    finally { busy = false }
  }

  async function importGraphs() {
    const sourceDir = await chooseFolder({ title: t('workflow.dialog.samFolder') })
    if (!sourceDir) return
    await modelTask(async () => {
      await getBackend().importSamTs({ sourceDir })
      verified = true
      await refresh()
    })
  }

  async function importFullRt() {
    const path = await chooseOnnx({ title: t('workflow.dialog.rtFile') })
    if (!path) return
    await modelTask(async () => { await getBackend().importFullRt({ sourcePath: path }); await refresh() })
  }

  async function downloadFullRt() {
    await modelTask(async () => {
      fullRtDownloading = true
      try {
        const status = await getBackend().downloadModel({ id: 'fullRt' })
        if (status === 'alreadyInstalled') {
          fullRtDownloading = false
          await refresh()
        }
      } catch (error) {
        fullRtDownloading = false
        throw error
      }
    })
  }

  async function installSam() {
    await modelTask(async () => {
      await getBackend().installSamTs()
      await refresh()
    })
  }

  async function choosePage() {
    const path = await chooseImage({ title: t('workflow.dialog.chooseImage') })
    if (path) sourcePath = path
  }

  /* ---------------------------------------------------------------- */
  /* Analysis and cancellation                                         */
  /* ---------------------------------------------------------------- */

  function newRequestId() {
    const random = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`
    return `review-${random}`
  }

  function clearReview() {
    selectionRequest += 1
    result = null
    focus = null
    prepared = null
    approved = false
    componentOutcome = null
    written = new Set()
    resetCorrections(0)
    correctionCursor = null
    zoomPct = null
    cloudMeta = null
  }

  async function run() {
    if (!canAnalyze) return
    if (cloudRoute) {
      analysisOutcome = null
      cloudPanel?.begin()
      return
    }
    const token = ++runToken
    const requestId = newRequestId()
    clearReview()
    analysisOutcome = null
    analyzing = { requestId, cancelling: false, page: pageNumber }
    await tick()
    runActions?.querySelector('[data-role="cancel"]')?.focus()
    try {
      const spec = { workflow, rtProfile, rtBackend, samBackend, requestId }
      const next = chapterId
        ? await getBackend().analyzeChapterPage({ chapterId, pageIndex, ...spec })
        : await getBackend().analyzeCapabilities({ sourcePath, ...spec })
      if (token !== runToken || destroyed) return
      result = next ?? null
    } catch (cause) {
      if (token !== runToken || destroyed) return
      analysisOutcome = outcomeOf(cause, 'analyze')
    } finally {
      if (token === runToken && !destroyed) {
        // Cancel leaves the page with the run. If it held focus, focus
        // returns to Analyze instead of falling to the document body.
        const cancelFocused = runActions?.querySelector('[data-role="cancel"]') === document.activeElement
        analyzing = null
        await tick()
        if (cancelFocused && (!document.activeElement || document.activeElement === document.body)) {
          runActions?.querySelector('[data-role="analyze"]')?.focus()
        }
        measureViewport()
      }
    }
  }

  async function cancelAnalysis() {
    if (!analyzing || analyzing.cancelling) return
    const { requestId } = analyzing
    analyzing = { ...analyzing, cancelling: true }
    // The run's own rejection names the outcome. A cancel that arrives after
    // the page finished answers false, and the result simply lands.
    try { await getBackend().cancelCapabilityAnalysis(requestId) } catch { /* see above */ }
  }

  function changePage() {
    clearReview()
    analysisOutcome = null
  }

  /**
   * Cloud evidence replaces the review, as it replaces the native side's one
   * stored analysis: a plan prepared from the previous analysis is gone.
   *
   * @param {any} analysis
   * @param {{ provider: string, profileName: string, capability: string }} meta
   */
  async function showCloudResult(analysis, meta) {
    if (destroyed || !analysis) return
    runToken += 1
    clearReview()
    analysisOutcome = null
    result = analysis
    cloudMeta = meta
    await tick()
    measureViewport()
  }

  /* ---------------------------------------------------------------- */
  /* Selection, preparation and apply                                  */
  /* ---------------------------------------------------------------- */

  function pixelsFromRaster(raster, pageWidth, pageHeight) {
    const bounds = raster?.bounds
    if (!bounds || ![bounds.x, bounds.y, bounds.w, bounds.h].every(Number.isInteger) ||
        bounds.x < 0 || bounds.y < 0 || bounds.w < 0 || bounds.h < 0 ||
        bounds.x + bounds.w > pageWidth || bounds.y + bounds.h > pageHeight ||
        bounds.w * bounds.h > MAX_TINT_PIXELS || !Array.isArray(raster.bits) || raster.bits.length !== bounds.w * bounds.h) {
      throw new Error('Saved correction raster is outside the analyzed page bounds')
    }
    const pixels = new Set()
    for (let y = 0; y < bounds.h; y += 1) {
      for (let x = 0; x < bounds.w; x += 1) {
        if (raster.bits[y * bounds.w + x] > 0) pixels.add((bounds.y + y) * pageWidth + bounds.x + x)
      }
    }
    return pixels
  }

  function writeSpec(componentId, analysisId = result?.analysisId) {
    if (!chapterId || !analysisId || !result) return null
    return {
      analysisId,
      chapterId,
      pageIndex,
      componentId,
      allowOutsideBubbles: allowOutside,
      paddingPx: Number(paddingPx),
      additions: rasterFromPixels(additionPixels, result.evidence.width),
      removals: rasterFromPixels(removalPixels, result.evidence.width),
      correctionRevision,
    }
  }

  function resetCorrections(revision) {
    additionPixels = new Set()
    removalPixels = new Set()
    correctionRevision = revision
    settledRevision = null
    syncPending(null)
  }

  async function selectCandidate(id) {
    const request = ++selectionRequest
    focus = id
    prepared = null
    approved = false
    componentOutcome = null
    resetCorrections(0)
    correctionCursor = null
    await tick()
    const entry = result?.evidence.components.find((item) => item.id === id) ?? result?.evidence.regions.find((item) => item.id === id)
    if (entry) revealBounds(entry.bounds, false)
    if (!chapterId || !id.startsWith('sam-') || !result?.analysisId || isRemoteAnalysis(result)) return
    const analysisId = result.analysisId
    const selectedPage = pageIndex
    busy = true
    let phase = 'load'
    try {
      const saved = await getBackend().loadComponentCorrection({ analysisId, chapterId, pageIndex: selectedPage, componentId: id })
      if (request !== selectionRequest || focus !== id || pageIndex !== selectedPage || result?.analysisId !== analysisId) return
      if (!saved) return
      additionPixels = pixelsFromRaster(saved.additions, result.evidence.width, result.evidence.height)
      removalPixels = pixelsFromRaster(saved.removals, result.evidence.width, result.evidence.height)
      paddingPx = saved.paddingPx
      correctionRevision = saved.correctionRevision
      settledRevision = saved.correctionRevision
      syncPending(boxOfPixels([...additionPixels, ...removalPixels], result.evidence.width))
      if (canPrepare) {
        phase = 'prepare'
        const spec = writeSpec(id, analysisId)
        if (!spec) return
        const seq = ++prepareSeq
        const next = await getBackend().prepareComponentWrite(spec)
        if (seq !== prepareSeq || request !== selectionRequest || focus !== id || pageIndex !== selectedPage || result?.analysisId !== analysisId) return
        prepared = next
      }
    } catch (cause) {
      if (request === selectionRequest && focus === id) componentOutcome = outcomeOf(cause, phase)
    } finally {
      if (request === selectionRequest) busy = false
    }
  }

  async function prepare() {
    if (!canPrepare) return
    const componentId = focus
    const analysisId = result?.analysisId
    const request = selectionRequest
    const seq = ++prepareSeq
    busy = true
    approved = false
    componentOutcome = null
    try {
      const spec = writeSpec(componentId, analysisId)
      if (!spec) return
      const next = await getBackend().prepareComponentWrite(spec)
      if (seq !== prepareSeq || request !== selectionRequest || focus !== componentId || result?.analysisId !== analysisId) return
      prepared = next
      if (next && next.supportPixels === 0) componentOutcome = { kind: 'needsCorrection', reason: 'empty' }
    } catch (cause) {
      if (seq === prepareSeq && request === selectionRequest && focus === componentId) componentOutcome = outcomeOf(cause, 'prepare')
    } finally {
      if (seq === prepareSeq) busy = false
    }
  }

  async function applyPrepared() {
    if (!chapterId || !prepared || !approved || !componentCanWrite) return
    const target = { chapterId, pageIndex }
    const plan = prepared
    const page = pageNumber
    busy = true
    componentOutcome = null
    try {
      // Read the page before the native commit. A repeated M5 write replaces
      // the same stable region id with a new immutable patch revision, so its
      // undo side must be the exact revision currently on disk.
      const beforePages = await getBackend().loadPages({ chapterId: target.chapterId, indices: [target.pageIndex] })
      const beforePage = beforePages?.find((entry) => entry.index === target.pageIndex)
      if (!beforePage) throw new Error('Could not load the page state needed for Undo')
      const applied = await getBackend().applyComponentWrite({
        planId: plan.planId, approvedSupportSha256: plan.supportSha256,
      })
      const previousRegion = beforePage.regions.find((region) => region.id === applied.regionId) ?? null
      const before = {
        region: previousRegion ? $state.snapshot(previousRegion) : null,
        pageStatus: beforePage.status ?? null,
      }
      const after = { region: applied.region, pageStatus: applied.pageStatus }
      if (editor.chapter?.id === target.chapterId) {
        applyRegionState(applied.regionId, applied.region, applied.pageStatus)
        recordRegionEdit('canvas.command.applyTool', applied.regionId, before, after)
      } else {
        await recordBackgroundRegionEdit(target.chapterId, 'canvas.command.applyTool', applied.regionId, before, after)
      }
      written = new Set([...written, plan.componentId])
      settledRevision = plan.correctionRevision ?? correctionRevision
      prepared = null
      approved = false
      componentOutcome = { kind: 'applied', params: { page } }
    } catch (cause) {
      componentOutcome = outcomeOf(cause, 'apply')
    } finally {
      busy = false
    }
  }

  function toggleOutsidePermission(event) {
    allowOutside = event.currentTarget.checked
    prepared = null
    approved = false
    componentOutcome = null
  }

  function changePadding(event) {
    paddingPx = Math.max(0, Math.min(64, Math.trunc(Number(event.currentTarget.value) || 0)))
    prepared = null
    approved = false
  }

  function clearCorrections() {
    resetCorrections(correctionRevision + 1)
    prepared = null
    approved = false
    void prepare()
  }

  /* ---------------------------------------------------------------- */
  /* Corrections                                                       */
  /* ---------------------------------------------------------------- */

  function rasterFromPixels(pixels, width) {
    if (!pixels.size) return { bounds: { x: 0, y: 0, w: 0, h: 0 }, bits: [] }
    const box = boxOfPixels(pixels, width)
    if (box.w * box.h > MAX_TINT_PIXELS) throw new Error('Correction raster exceeds the 16 megapixel plan limit')
    const bits = new Array(box.w * box.h).fill(0)
    for (const at of pixels) bits[(Math.floor(at / width) - box.y) * box.w + (at % width) - box.x] = 255
    return { bounds: box, bits }
  }

  /** @param {Iterable<number>} pixels @param {number} width */
  function boxOfPixels(pixels, width) {
    let left = Infinity, top = Infinity, right = -Infinity, bottom = -Infinity
    for (const at of pixels) {
      const x = at % width
      const y = Math.floor(at / width)
      left = Math.min(left, x)
      top = Math.min(top, y)
      right = Math.max(right, x + 1)
      bottom = Math.max(bottom, y + 1)
    }
    return left === Infinity ? null : { x: left, y: top, w: right - left, h: bottom - top }
  }

  /** @param {{x: number, y: number, w: number, h: number}|null} box */
  function syncPending(box) {
    pendingBox = box
    addedCount = additionPixels.size
    removedCount = removalPixels.size
    pendingVersion += 1
  }

  function pointFromPointer(event) {
    const surface = event.currentTarget.querySelector('[data-surface]')
    const rect = surface?.getBoundingClientRect()
    if (!rect || rect.width <= 0 || rect.height <= 0 || !result) return null
    const x = Math.floor((event.clientX - rect.left) / rect.width * result.evidence.width)
    const y = Math.floor((event.clientY - rect.top) / rect.height * result.evidence.height)
    if (x < 0 || y < 0 || x >= result.evidence.width || y >= result.evidence.height) return null
    return { x, y }
  }

  function paintCorrection(point) {
    if (!result || !point || !['add', 'remove'].includes(correctionMode)) return
    const { width, height } = result.evidence
    const r = Math.max(1, Math.trunc(Number(brushRadius) || 1))
    for (let dy = -r; dy <= r; dy += 1) {
      for (let dx = -r; dx <= r; dx += 1) {
        if (dx * dx + dy * dy > r * r) continue
        const x = point.x + dx
        const y = point.y + dy
        if (x < 0 || y < 0 || x >= width || y >= height) continue
        const index = y * width + x
        if (correctionMode === 'add') {
          removalPixels.delete(index)
          additionPixels.add(index)
        } else {
          additionPixels.delete(index)
          removalPixels.add(index)
        }
      }
    }
    const left = Math.max(0, point.x - r)
    const top = Math.max(0, point.y - r)
    const stroke = { x: left, y: top, w: Math.min(width, point.x + r + 1) - left, h: Math.min(height, point.y + r + 1) - top }
    syncPending(pendingBox ? unionBox(pendingBox, stroke) : stroke)
    correctionCursor = point
    prepared = null
    approved = false
  }

  function unionBox(a, b) {
    const x = Math.min(a.x, b.x)
    const y = Math.min(a.y, b.y)
    return { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y }
  }

  function paintable() {
    return Boolean(selectedComponent && focus?.startsWith('sam-') && chapterId && !isRemote && correctionMode !== 'inspect')
  }

  function onCorrectionPointerDown(event) {
    if (!result) return
    if (correctionMode === 'inspect' || !selectedComponent) {
      const point = pointFromPointer(event)
      const id = point && hitTest(point, result.evidence, HIT_TOLERANCE / scale)
      if (id) void selectCandidate(id)
      return
    }
    if (!paintable()) return
    event.preventDefault()
    // Capture keeps a stroke that leaves the page; a pointer the engine no
    // longer tracks refuses it, and the stroke still paints without it.
    try { event.currentTarget.setPointerCapture?.(event.pointerId) } catch { /* see above */ }
    drawingCorrection = true
    lastCorrectionPoint = pointFromPointer(event)
    paintCorrection(lastCorrectionPoint)
  }

  function onCorrectionPointerMove(event) {
    const point = pointFromPointer(event)
    correctionCursor = point
    if (!drawingCorrection || !point) return
    const last = lastCorrectionPoint ?? point
    const distance = Math.max(Math.abs(point.x - last.x), Math.abs(point.y - last.y))
    const stride = Math.max(1, Math.floor(Number(brushRadius) || 1))
    for (let step = stride; step <= distance; step += stride) {
      paintCorrection({
        x: Math.round(last.x + (point.x - last.x) * step / distance),
        y: Math.round(last.y + (point.y - last.y) * step / distance),
      })
    }
    paintCorrection(point)
    lastCorrectionPoint = point
  }

  async function finishCorrection() {
    if (!drawingCorrection) return
    drawingCorrection = false
    lastCorrectionPoint = null
    correctionRevision += 1
    await prepare()
  }

  function moveKeyboardCursor(event) {
    if (!result) return
    const { width, height } = result.evidence
    const bounds = selectedBounds
    const current = correctionCursor ?? (bounds
      ? { x: Math.floor(bounds.x + bounds.w / 2), y: Math.floor(bounds.y + bounds.h / 2) }
      : { x: Math.floor(width / 2), y: Math.floor(height / 2) })
    // One screen point per press at the current zoom, ten with Shift.
    const step = Math.max(1, Math.round(1 / scale)) * (event.shiftKey ? 10 : 1)
    const next = { ...current }
    if (event.key === 'ArrowLeft') next.x -= step
    else if (event.key === 'ArrowRight') next.x += step
    else if (event.key === 'ArrowUp') next.y -= step
    else if (event.key === 'ArrowDown') next.y += step
    else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      if (correctionMode === 'inspect' || !selectedComponent) {
        const id = correctionCursor && hitTest(correctionCursor, result.evidence, HIT_TOLERANCE / scale)
        if (id) void selectCandidate(id)
        return
      }
      if (!paintable()) return
      paintCorrection(current)
      correctionRevision += 1
      void prepare()
      return
    } else return
    event.preventDefault()
    correctionCursor = {
      x: Math.max(0, Math.min(width - 1, next.x)),
      y: Math.max(0, Math.min(height - 1, next.y)),
    }
    revealBounds({ ...correctionCursor, w: 1, h: 1 }, false)
  }

  /**
   * Draw one pending set into its canvas, in source pixels, so CSS scales it
   * with `image-rendering: pixelated` and a stroke is visible before prepare.
   * Removals are drawn on every other pixel: at any zoom they read as a
   * hatched layer, not only as a second hue.
   */
  function drawPixels(canvas, pixels, box, width, hatched) {
    const context = canvas?.getContext?.('2d')
    if (!context) return
    canvas.width = box.w
    canvas.height = box.h
    if (!pixels.size) return
    const image = context.createImageData(box.w, box.h)
    for (const at of pixels) {
      const x = (at % width) - box.x
      const y = Math.floor(at / width) - box.y
      if (x < 0 || y < 0 || x >= box.w || y >= box.h) continue
      if (hatched && (x + y + box.x + box.y) % 2) continue
      image.data[(y * box.w + x) * 4 + 3] = 255
    }
    context.putImageData(image, 0, 0)
    context.globalCompositeOperation = 'source-in'
    context.fillStyle = getComputedStyle(canvas).color
    context.fillRect(0, 0, box.w, box.h)
    context.globalCompositeOperation = 'source-over'
  }

  $effect(() => {
    void pendingVersion
    const box = pendingBox
    const width = result?.evidence.width
    if (!pendingVisible || !box || !width || box.w * box.h > MAX_TINT_PIXELS) return
    drawPixels(addCanvas, additionPixels, box, width, false)
    drawPixels(removeCanvas, removalPixels, box, width, true)
  })

  /* ---------------------------------------------------------------- */
  /* Zoom and scroll: the page stays where it is                        */
  /* ---------------------------------------------------------------- */

  function measureViewport() {
    viewportWidth = viewport?.clientWidth ?? 0
  }

  $effect(() => {
    if (viewport) untrack(measureViewport)
  })

  function viewCenterFraction() {
    if (!viewport || !previewImage) return null
    const view = viewport.getBoundingClientRect()
    const page = previewImage.getBoundingClientRect()
    if (!page.width || !page.height) return null
    return {
      x: (view.left + view.width / 2 - page.left) / page.width,
      y: (view.top + view.height / 2 - page.top) / page.height,
    }
  }

  function keepCentered(anchor) {
    if (!anchor || !viewport || !previewImage) return
    const view = viewport.getBoundingClientRect()
    const page = previewImage.getBoundingClientRect()
    viewport.scrollLeft += page.left + anchor.x * page.width - (view.left + view.width / 2)
    viewport.scrollTop += page.top + anchor.y * page.height - (view.top + view.height / 2)
  }

  /** @param {number|null} next - percent of source pixels, or null to fit */
  async function setZoom(next) {
    const anchor = viewCenterFraction()
    zoomPct = next == null ? null : Math.max(zoomMin, Math.min(MAX_ZOOM, Math.round(next)))
    await tick()
    keepCentered(anchor)
  }

  /** Scroll a source rectangle into view, centring it, only when it is not already visible. */
  function revealBounds(bounds, force) {
    if (!viewport || !previewImage || !result || !bounds) return
    const view = viewport.getBoundingClientRect()
    const page = previewImage.getBoundingClientRect()
    if (!page.width || !page.height) return
    const sx = page.width / result.evidence.width
    const sy = page.height / result.evidence.height
    const left = page.left + bounds.x * sx
    const top = page.top + bounds.y * sy
    const right = left + Math.max(1, bounds.w * sx)
    const bottom = top + Math.max(1, bounds.h * sy)
    if (!force && left >= view.left && right <= view.right && top >= view.top && bottom <= view.bottom) return
    viewport.scrollLeft += (left + right) / 2 - (view.left + view.width / 2)
    viewport.scrollTop += (top + bottom) / 2 - (view.top + view.height / 2)
  }

  async function zoomToSelection() {
    const bounds = selectedBounds
    if (!bounds || !viewport) return
    const width = viewport.clientWidth || 600
    const height = viewport.clientHeight || 400
    const fit = 0.45 * Math.min(width / Math.max(1, bounds.w), height / Math.max(1, bounds.h))
    zoomPct = Math.max(zoomMin, Math.min(MAX_ZOOM, Math.round(fit * 100)))
    await tick()
    revealBounds(bounds, true)
  }

  /* ---------------------------------------------------------------- */
  /* Candidate list keyboard                                           */
  /* ---------------------------------------------------------------- */

  function onListKeydown(event) {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
    const buttons = [...event.currentTarget.querySelectorAll('button[data-candidate]')]
    if (!buttons.length) return
    const index = buttons.indexOf(/** @type {any} */ (document.activeElement))
    let next = event.key === 'Home' ? 0
      : event.key === 'End' ? buttons.length - 1
      : index < 0 ? 0
      : index + (event.key === 'ArrowDown' ? 1 : -1)
    next = Math.max(0, Math.min(buttons.length - 1, next))
    event.preventDefault()
    buttons[next].focus()
  }

  /* ---------------------------------------------------------------- */
  /* Labels                                                            */
  /* ---------------------------------------------------------------- */

  function percent(value, total) { return `${value / total * 100}%` }

  function backendName(id) {
    if (id === 'auto') return t('settings.accel.auto')
    if (id === 'ort-cpu') return t('workflow.backend.cpu')
    if (id === 'ort-webgpu') return t('workflow.backend.webgpu')
    if (id === 'ort-coreml') return t('accel.coreml')
    if (id === 'ort-directml') return t('accel.directml')
    if (id === 'ort-cuda') return t('accel.cuda')
    return id
  }

  /**
   * A backend choice. Cloud is chosen for both stages at once and left for
   * both at once, and cloud RT-DETR is the full model.
   *
   * @param {'rt'|'sam'} stage
   * @param {string} value
   */
  function chooseBackend(stage, value) {
    if (value === 'cloud') {
      rtBackend = 'cloud'
      samBackend = 'cloud'
      rtProfile = 'full-halves'
      return
    }
    if (stage === 'rt') rtBackend = value
    else samBackend = value
    if (rtBackend === 'cloud') rtBackend = 'auto'
    if (samBackend === 'cloud') samBackend = 'auto'
  }

  // A workflow that cannot go to the cloud leaves it for both stages.
  $effect(() => {
    if (cloudWorkflow) return
    untrack(() => {
      if (rtBackend === 'cloud') rtBackend = 'auto'
      if (samBackend === 'cloud') samBackend = 'auto'
    })
  })

  function cloudOptionLabel() {
    if (!cloudWorkflow) return t('workflow.backend.cloudCtd')
    return cloudUsable() ? t('workflow.backend.cloud') : t('workflow.backend.cloudOff')
  }

  function rtBackendSelectable(id) {
    const modelId = rtProfile === 'small-whole' ? 'rtSmall' : 'rtFull'
    const providerId = id.startsWith('ort-') ? id.slice(4) : id
    const status = modelBackendRows?.find((row) => row.id === modelId)?.backendStatus?.find((entry) => entry.id === providerId)
    if (status) return status.supported && status.available
    // Older native builds may lack the per-model matrix. Neither shipped RT
    // graph has a strict WebGPU session, so do not invite that selection.
    if (id === 'ort-webgpu') return false
    return capabilities?.rtBackends?.some((entry) => entry.id === id && entry.selectable) ?? false
  }

  function samOptionLabel(option) {
    const name = backendName(option.id)
    if (!option.selectable) return t('workflow.backend.unavailable', { name })
    return option.id === 'ort-webgpu' && capabilities?.samWriteQualified
      ? t('workflow.backend.canWrite', { name })
      : t('workflow.backend.reviewOnly', { name })
  }

  function rtOptionLabel(option) {
    const name = backendName(option.id)
    if (!option.selectable) return t('workflow.backend.unavailable', { name })
    return t(option.qualified ? 'workflow.backend.qualified' : 'workflow.backend.unqualified', { name })
  }

  function mb(bytes) { return Math.round(bytes / 1_000_000) }
</script>

<svelte:window onresize={measureViewport} />

{#snippet rebuildAction()}
  <Button size="sm" disabled={busy} onclick={prepare}>{t('workflow.action.rebuild')}</Button>
{/snippet}

{#snippet reanalyzeAction()}
  <Button size="sm" onclick={run}>{t('workflow.action.reanalyze')}</Button>
{/snippet}

{#snippet outcomeView(outcome)}
  <WorkflowOutcome
    {outcome}
    action={outcome.kind === 'stale' && canPrepare ? rebuildAction
      : outcome.kind === 'expired' && canAnalyze ? reanalyzeAction
      : null}
  />
{/snippet}

{#snippet models()}
  <div class="capabilities">
    <div>
      <strong>{t('workflow.model.rt')}</strong>
      <span>
        {t('workflow.cap.rtFull', { statusKey: capabilities.fullRtInstalled ? 'workflow.state.verified' : 'workflow.state.missing' })}
        <br />
        {t('workflow.cap.rtSmall', { statusKey: capabilities.rtInstalled ? 'workflow.state.verified' : 'workflow.state.installLater' })}
      </span>
    </div>
    <div>
      <strong>{t('workflow.model.sam')}</strong>
      <span>{t('workflow.cap.samState', {
        statusKey: !capabilities.samInstalled ? 'workflow.state.samMissing'
          : verified === true ? 'workflow.state.verified' : 'workflow.state.unverified',
        memoryKey: capabilities.samMemoryReady ? 'workflow.state.memoryReady' : 'workflow.state.memoryShort',
      })}</span>
    </div>
    <div>
      <strong>{t('workflow.model.write')}</strong>
      <span>{t(capabilities.samWriteQualified ? 'workflow.cap.writeQualified' : 'workflow.cap.writeReviewOnly')}</span>
    </div>
    <div>
      <strong>{t('workflow.model.coo')}</strong>
      <span>{t('workflow.cap.cooAbsent')}</span>
    </div>
    <div>
      <strong>{t('workflow.model.runtime')}</strong>
      <span>{t(!capabilities.runtimeInstalled ? 'workflow.cap.runtimeMissing'
        : runtimeLoad.state === 'failed' ? runtimeLoad.reasonKey
        : 'workflow.cap.runtimeInstalled')}</span>
    </div>
  </div>

  <div class="actions">
    <Button disabled={busy || !!analyzing || fullRtDownloading || capabilities.fullRtInstalled} onclick={downloadFullRt}>{t('workflow.action.downloadFullRt')}</Button>
    <Button disabled={busy || !!analyzing} onclick={importFullRt}>{t('workflow.action.importFullRt')}</Button>
    {#if capabilities.fullRtManaged}
      <Button disabled={busy || !!analyzing} onclick={() => modelTask(async () => { await getBackend().removeFullRt(); await refresh() })}>{t('workflow.action.removeFullRt')}</Button>
    {/if}
    <Button disabled={busy || !!analyzing || capabilities.samInstalled} onclick={installSam}>{t('workflow.action.installSam')}</Button>
    <Button disabled={busy || !!analyzing} onclick={importGraphs}>{t('workflow.action.importSam')}</Button>
    <Button disabled={busy || !!analyzing} onclick={() => modelTask(refresh, 'refresh')}>{t('workflow.action.refresh')}</Button>
    <Button disabled={busy || !!analyzing || !capabilities.samInstalled} onclick={() => modelTask(async () => { verified = await getBackend().verifySamTs() })}>{t('workflow.action.verifySam')}</Button>
    {#if capabilities.samManaged}
      <Button disabled={busy || !!analyzing} onclick={() => modelTask(async () => { await getBackend().removeSamTs(); verified = null; await refresh() })}>{t('workflow.action.removeSam')}</Button>
    {/if}
    {#if verified === true}<span class="verified"><Icon name="check" size={12} />{t('workflow.state.verified')}</span>{/if}
  </div>

  <details>
    <summary>{t('workflow.detail.rtIdentity')}</summary>
    <p>{t('workflow.detail.rtFile', { revision: capabilities.fullRtRevision, name: capabilities.fullRtFile.name, size: mb(capabilities.fullRtFile.bytes), sha: capabilities.fullRtFile.sha256 })}</p>
  </details>

  <details>
    <summary>{t('workflow.detail.samIdentity')}</summary>
    <p>{t('workflow.detail.samRevision', { revision: capabilities.samRevision })}</p>
    <ul>
      {#each capabilities.samFiles as file (file.name)}
        <li>{t('workflow.detail.samFile', { name: file.name, size: mb(file.bytes), sha: file.sha256 })}</li>
      {/each}
    </ul>
  </details>

  <details>
    <summary>{t('workflow.detail.backends')}</summary>
    <ul>
      {#each capabilities.rtBackends as option (option.id)}
        <li>{t('workflow.detail.backendRow', { family: t('workflow.model.rt'), name: backendName(option.id), platform: option.platform, statusKey: option.qualified ? 'workflow.detail.qualified' : 'workflow.detail.unqualified', note: option.note })}</li>
      {/each}
      {#each capabilities.samBackends as option (option.id)}
        <li>{t('workflow.detail.backendRow', { family: t('workflow.model.sam'), name: backendName(option.id), platform: option.platform, statusKey: option.qualified ? 'workflow.detail.qualified' : 'workflow.detail.unqualified', note: option.note })}</li>
      {/each}
    </ul>
  </details>
{/snippet}

{#snippet rtProfileChoice()}
  <label class="field">
    <span>{t('workflow.field.rtProfile')}</span>
    <select bind:value={rtProfile} onchange={() => (rtProfileChosen = true)} disabled={!preset?.needs.includes('rt') || !!analyzing || cloudRoute}>
      <option value="full-halves">{t('workflow.rtProfile.full')}</option>
      <option value="small-whole">{t('workflow.rtProfile.small')}</option>
    </select>
  </label>
{/snippet}

{#snippet rtBackendChoice()}
  <label class="field">
    <span>{t('workflow.field.rtBackend')}</span>
    <select value={rtBackend} onchange={(event) => chooseBackend('rt', event.currentTarget.value)}
      disabled={!preset?.needs.includes('rt') || !!analyzing || cloudActive}>
      <option value="auto">{t('workflow.backend.autoSettings')}</option>
      {#each capabilities.rtBackends as option (option.id)}
        <option value={option.id} disabled={!rtBackendSelectable(option.id)}>{rtOptionLabel({ ...option, selectable: rtBackendSelectable(option.id) })}</option>
      {/each}
      {#if chapterId}
        <option value="cloud" disabled={!cloudWorkflow || !cloudUsable()}>{cloudOptionLabel()}</option>
      {/if}
    </select>
  </label>
{/snippet}

<section
  class="workflow-analysis"
  class:embedded={!chapterId}
  aria-label={t(chapterId ? 'workflow.title.review' : 'workflow.title.independent')}
>
  {#if !chapterId}<h3>{t('workflow.title.independent')}</h3>{/if}
  <p class="intro">{t(chapterId ? 'workflow.intro.review' : 'workflow.intro.independent')}</p>

  {#if capabilities}
    {#if !chapterId}{@render models()}{/if}

    <div class="setup">
      {#if chapterId}
        <label class="field">
          <span>{t('workflow.field.page')}</span>
          <select bind:value={pageIndex} onchange={changePage} disabled={!!analyzing || cloudActive}>
            {#each chapterPages as page (page.index)}
              <option value={page.index}>{t('workflow.field.pageOption', { number: page.number ?? page.index + 1 })}</option>
            {/each}
          </select>
        </label>
      {/if}
      <label class="field">
        <span>{t('workflow.field.workflow')}</span>
        <select bind:value={workflow} disabled={!!analyzing || cloudActive}>
          {#each WORKFLOW_PRESETS as entry (entry.id)}
            <option value={entry.id}>{PRESET_KEYS[entry.id] ? t(PRESET_KEYS[entry.id]) : entry.name}</option>
          {/each}
        </select>
      </label>
      {#if !chapterId}{@render rtProfileChoice()}{/if}
      {@render rtBackendChoice()}
      <label class="field">
        <span>{t('workflow.field.samBackend')}</span>
        <select value={samBackend} onchange={(event) => chooseBackend('sam', event.currentTarget.value)}
          disabled={!preset?.needs.includes('sam') || !!analyzing || cloudActive}>
          <option value="auto">{t('workflow.backend.autoSettings')}</option>
          {#each capabilities.samBackends as option (option.id)}
            <option value={option.id} disabled={!option.selectable}>{samOptionLabel(option)}</option>
          {/each}
          {#if chapterId}
            <option value="cloud" disabled={!cloudWorkflow || !cloudUsable()}>{cloudOptionLabel()}</option>
          {/if}
        </select>
      </label>
      {#if !chapterId}
        <label class="field source">
          <span>{t('workflow.field.source')}</span>
          <input type="text" bind:value={sourcePath} placeholder={t('workflow.field.sourcePlaceholder')} disabled={!!analyzing} />
        </label>
        <Button disabled={busy || !!analyzing} onclick={choosePage}>{t('workflow.action.choosePage')}</Button>
      {/if}
      <!-- Cancel takes Analyze's place while a page runs, so the row keeps its
           shape; focus moves between the two with the swap. -->
      <span class="run-actions" bind:this={runActions}>
        {#if analyzing}
          <Button data-role="cancel" disabled={analyzing.cancelling} onclick={cancelAnalysis}>
            <Icon name="stop" size={12} />{t('workflow.action.cancel')}
          </Button>
        {:else}
          <Button data-role="analyze" variant="primary" disabled={!canAnalyze} onclick={run}>{t('workflow.action.analyze')}</Button>
        {/if}
      </span>
    </div>

  {/if}

  <div class="slot" role="status">
    {#if analyzing}
      <span class="run-status">
        {analyzing.cancelling
          ? t('workflow.status.cancelling')
          : chapterId ? t('workflow.status.analyzingPage', { page: analyzing.page }) : t('workflow.status.analyzing')}
      </span>
    {:else if shownAnalysisOutcome}
      {@render outcomeView(shownAnalysisOutcome)}
    {:else if capabilities && notReadyKey === 'workflow.ready.runtimeChecking'}
      <span class="run-status">{t('workflow.ready.runtimeChecking')}</span>
    {/if}
    {#if busy && !result && !analyzing}<span class="busy">{t('workflow.status.working')}</span>{/if}
  </div>

  {#if chapterId}
    <CloudAnalysis
      bind:this={cloudPanel}
      {chapterId}
      {pageIndex}
      {pageNumber}
      request={cloudRequest}
      locked={!!analyzing}
      onactive={(value) => { cloudActive = value }}
      onresult={showCloudResult}
    />
  {/if}

  {#if capabilities && chapterId}
    <details class="models">
      <summary>{t('workflow.model.section')}</summary>
      <div class="models-body">
        <div class="setup">{@render rtProfileChoice()}</div>
        {@render models()}
      </div>
    </details>
  {/if}

  {#if result}
    <div class="summary">
      <span>{t('workflow.result.components', { count: result.evidence.components.length })}</span>
      <span>{t('workflow.result.regions', { count: result.evidence.regions.length })}</span>
      {#if result.evidence.groups?.length}<span>{t('workflow.result.groups', { count: result.evidence.groups.length })}</span>{/if}
      {#if result.samBackend}<span>{t('workflow.result.flagged', { count: flaggedCount })}</span>{/if}
      {#if chapterId && !declineReason}<span class="can-write"><Icon name="check" size={12} />{t('workflow.result.canWrite')}</span>{/if}
      {#if isRemote}
        <span class="cloud-source"><Icon name="cloud" size={12} />{t('cloud.analysis.source', { name: cloudMeta?.profileName ?? result.remoteSource ?? '' })}</span>
      {/if}
    </div>
    {#if declineReason}{@render outcomeView({ kind: 'declined', reason: declineReason })}{/if}
    <p class="note"><Icon name="info" size={12} /><span><strong>{t('workflow.outcome.cooAbsent')}</strong> {t('workflow.explain.cooAbsent')}</span></p>
    {#if !result.sourceDataUrl}
      <p class="note"><Icon name="info" size={12} /><span>{t('cloud.analysis.noImage')}</span></p>
    {/if}

    <div class="review">
      <div class="stage-col">
        <div class="zoombar" role="group" aria-label={t('workflow.field.zoom')}>
          <Button size="sm" aria-pressed={zoomPct == null} title={t('workflow.zoom.fitLabel')} aria-label={t('workflow.zoom.fitLabel')} onclick={() => setZoom(null)}>
            <Icon name="zoom-fit" size={12} />{t('workflow.action.zoomFit')}
          </Button>
          <Button size="sm" aria-pressed={zoomPct === 100} title={t('workflow.zoom.actualLabel')} aria-label={t('workflow.zoom.actualLabel')} onclick={() => setZoom(100)}>
            {t('workflow.action.zoomActual')}
          </Button>
          <input
            class="zoom-range"
            aria-label={t('workflow.aria.zoom')}
            aria-valuetext={t('workflow.zoom.value', { percent: Math.round(scale * 100) })}
            type="range"
            min={zoomMin}
            max={MAX_ZOOM}
            step="5"
            value={Math.round(scale * 100)}
            oninput={(event) => setZoom(Number(event.currentTarget.value))}
          />
          <output class="zoom-value">{t('workflow.zoom.value', { percent: Math.round(scale * 100) })}</output>
          <Button size="sm" disabled={!selectedBounds} onclick={zoomToSelection}>
            <Icon name="zoom-in" size={12} />{t('workflow.action.zoomComponent')}
          </Button>
        </div>

        <div class="viewport" bind:this={viewport}>
          <div class="stage">
            <div class="page-preview" role="group" aria-label={t('workflow.aria.preview')}>
              <button
                class="page-art"
                class:painting={correctionMode !== 'inspect' && !!selectedComponent}
                type="button"
                aria-label={t('workflow.aria.canvas')}
                aria-keyshortcuts="ArrowUp ArrowDown ArrowLeft ArrowRight Enter"
                onkeydown={moveKeyboardCursor}
                onpointerdown={onCorrectionPointerDown}
                onpointermove={onCorrectionPointerMove}
                onpointerup={finishCorrection}
                onpointercancel={finishCorrection}
              >
                {#if result.sourceDataUrl}
                  <img
                    bind:this={previewImage}
                    data-surface
                    class:crisp={scale > 1}
                    src={result.sourceDataUrl}
                    alt={t('workflow.aria.image')}
                    draggable="false"
                    style:width="{result.evidence.width * scale}px"
                  />
                {:else}
                  <div
                    class="blank"
                    bind:this={previewImage}
                    data-surface
                    aria-hidden="true"
                    style:width="{result.evidence.width * scale}px"
                    style:height="{result.evidence.height * scale}px"
                  ></div>
                {/if}
                {#if result.maskDataUrl}
                  <MaskTint
                    src={result.maskDataUrl}
                    bounds={{ x: 0, y: 0, w: result.evidence.width, h: result.evidence.height }}
                    pageWidth={result.evidence.width}
                    pageHeight={result.evidence.height}
                    tone="evidence"
                  />
                {/if}
                {#if prepared}
                  <MaskTint
                    src={prepared.supportDataUrl}
                    bounds={prepared.bounds}
                    pageWidth={result.evidence.width}
                    pageHeight={result.evidence.height}
                    tone="write"
                  />
                {/if}
                {#if pendingVisible && pendingBox}
                  <canvas class="pending add" bind:this={addCanvas} aria-hidden="true" style:left={percent(pendingBox.x, result.evidence.width)} style:top={percent(pendingBox.y, result.evidence.height)} style:width={percent(pendingBox.w, result.evidence.width)} style:height={percent(pendingBox.h, result.evidence.height)}></canvas>
                  <canvas class="pending remove" bind:this={removeCanvas} aria-hidden="true" style:left={percent(pendingBox.x, result.evidence.width)} style:top={percent(pendingBox.y, result.evidence.height)} style:width={percent(pendingBox.w, result.evidence.width)} style:height={percent(pendingBox.h, result.evidence.height)}></canvas>
                {/if}
                {#if selectedBounds}
                  <div class="locator" class:detector={!!selectedRegion} aria-hidden="true" style:left={percent(selectedBounds.x, result.evidence.width)} style:top={percent(selectedBounds.y, result.evidence.height)} style:width={percent(selectedBounds.w, result.evidence.width)} style:height={percent(selectedBounds.h, result.evidence.height)}></div>
                {/if}
                {#if correctionCursor}
                  <div
                    class="cursor"
                    aria-hidden="true"
                    style:left={percent(correctionCursor.x + 0.5, result.evidence.width)}
                    style:top={percent(correctionCursor.y + 0.5, result.evidence.height)}
                    style:--size="{correctionMode === 'inspect' ? 8 : Math.max(8, (2 * brushRadius + 1) * scale)}px"
                  ></div>
                {/if}
              </button>
            </div>
          </div>
        </div>

        <ul class="legend" aria-label={t('workflow.aria.legend')}>
          {#if result.maskDataUrl}<li><span class="swatch evidence"></span>{t('workflow.legend.evidence')}</li>{/if}
          {#if chapterId && !isRemote}
            <li><span class="swatch write"></span>{t('workflow.legend.write')}</li>
            <li><span class="swatch add"></span>{t('workflow.legend.add')}</li>
            <li><span class="swatch remove"></span>{t('workflow.legend.remove')}</li>
          {/if}
          <li><span class="swatch box"></span>{t('workflow.legend.locator')}</li>
        </ul>
      </div>

      <div class="side">
        {#if chapterId && result.analysisId && !isRemote}
          <div class="permission">
            <input id="{uid}-permission" type="checkbox" checked={allowOutside} onchange={toggleOutsidePermission} aria-describedby="{uid}-permission-help" />
            <div>
              <label class="permission-label" for="{uid}-permission">{t('workflow.permission.label')}</label>
              <p class="help" id="{uid}-permission-help">{t('workflow.permission.help')}</p>
            </div>
          </div>
        {/if}

        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div class="candidate-list" role="group" aria-label={t('workflow.aria.candidates')} onkeydown={onListKeydown}>
          {#if result.evidence.components.length}
            <h4 class="list-head">{t('workflow.list.components')}</h4>
            <ul>
              {#each result.evidence.components as component (component.id)}
                {@const held = chapterId && !isRemote && !allowOutside && !component.rtBubbleIds?.length}
                <li>
                  <button
                    type="button"
                    data-candidate
                    tabindex={component.id === rovingId ? 0 : -1}
                    aria-pressed={focus === component.id}
                    onclick={() => selectCandidate(component.id)}
                  >
                    <span class="id">{component.id}</span>
                    <span class="px">{t('workflow.list.pixels', { count: component.pixels })}</span>
                    <span class="tags">
                      {#if component.rtBubbleIds?.length}<span class="tag">{t('workflow.tag.bubble')}</span>
                      {:else if component.rtTextIds?.length}<span class="tag">{t('workflow.tag.text')}</span>
                      {:else}<span class="tag muted">{t('workflow.tag.noBox')}</span>{/if}
                      {#if held}<span class="tag held">{t('workflow.tag.held')}</span>{/if}
                      {#each reasonsOf(component) as reason (reason)}<span class="tag reason">{t(REASON_KEYS[reason].tag)}</span>{/each}
                      {#if written.has(component.id)}<span class="tag written"><Icon name="check" size={10} />{t('workflow.tag.written')}</span>{/if}
                    </span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
          {#if result.evidence.regions.length}
            <h4 class="list-head">{t('workflow.list.regions')}</h4>
            <ul>
              {#each result.evidence.regions as region (region.id)}
                <li>
                  <button
                    type="button"
                    data-candidate
                    tabindex={region.id === rovingId ? 0 : -1}
                    aria-pressed={focus === region.id}
                    onclick={() => selectCandidate(region.id)}
                  >
                    <span class="id">{region.id}</span>
                    <span class="px">{REGION_KIND_KEYS[region.kind] ? t(REGION_KIND_KEYS[region.kind]) : region.kind}</span>
                    <span class="tags">
                      {#if region.kind === 'bubble_context'}<span class="tag muted">{t('workflow.tag.contextOnly')}</span>{/if}
                      {#if region.detectorOnly}<span class="tag muted">{t('workflow.tag.noMask')}</span>{/if}
                    </span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>

        <div class="panel">
          {#if selectedRegion}
            <p class="panel-head"><strong>{selectedRegion.id}</strong></p>
            <p>{t('workflow.panel.region')}</p>
          {:else if selectedComponent}
            <p class="panel-head"><strong>{selectedComponent.id}</strong><span>{t('workflow.list.pixels', { count: selectedComponent.pixels })}</span></p>
            {#if selectedGroup}
              <p class="group-note">{t(selectedGroup.disposition === 'candidate' ? 'workflow.panel.heldGroup' : 'workflow.panel.group',
                { id: selectedGroup.id, count: selectedGroup.componentIds.length })}</p>
            {/if}
            {#each reasonsOf(selectedComponent) as reason (reason)}
              <p class="reason-note"><Icon name="info" size={12} /><span>{t(REASON_KEYS[reason].note)}</span></p>
            {/each}

            <div class="slot" role="status">
              {#if shownComponentOutcome}{@render outcomeView(shownComponentOutcome)}{/if}
            </div>

            {#if chapterId && isRemote && focus?.startsWith('sam-')}
              <div class="review-only">
                <p id="{uid}-remote-reason"><Icon name="lock" size={12} /><span>{t('cloud.analysis.reviewOnly')}</span></p>
                <div class="buttons">
                  <Button disabled aria-describedby="{uid}-remote-reason">{t('workflow.action.prepare')}</Button>
                  <Button variant="primary" disabled aria-describedby="{uid}-remote-reason">{t('workflow.action.apply')}</Button>
                </div>
              </div>
            {:else if chapterId && result.analysisId && focus?.startsWith('sam-')}
              <div class="correction-tools">
                <label class="field">
                  <span>{t('workflow.field.padding')}</span>
                  <input aria-label={t('workflow.aria.padding')} type="number" min="0" max="64" step="1" bind:value={paddingPx} onchange={changePadding} />
                </label>
                <label class="field">
                  <span>{t('workflow.field.brush')}</span>
                  <input aria-label={t('workflow.aria.brush')} type="number" min="1" max="24" step="1" bind:value={brushRadius} />
                </label>
                <label class="field wide">
                  <span>{t('workflow.field.correction')}</span>
                  <select aria-label={t('workflow.aria.correction')} bind:value={correctionMode}>
                    <option value="inspect">{t('workflow.correction.inspect')}</option>
                    <option value="add">{t('workflow.correction.add')}</option>
                    <option value="remove">{t('workflow.correction.remove')}</option>
                  </select>
                </label>
              </div>

              {#if pendingVisible}
                <p class="pending-note">{t('workflow.correction.pending', { added: addedCount, removed: removedCount })}</p>
              {/if}

              <div class="buttons">
                <Button disabled={busy || !canPrepare} onclick={prepare}>{t('workflow.action.prepare')}</Button>
                <Button disabled={busy || (!correctionRevision && !addedCount && !removedCount)} onclick={clearCorrections}>{t('workflow.action.clear')}</Button>
                {#if busy}<span class="busy">{t('workflow.status.working')}</span>{/if}
              </div>

              {#if prepared}
                <div class="prepared">
                  <p class="w-summary"><span class="swatch write"></span>{t('workflow.write.summary', { count: prepared.supportPixels, padding: prepared.paddingPx ?? paddingPx })}</p>
                  <details>
                    <summary>{t('workflow.write.identity')}</summary>
                    <dl>
                      <dt>{t('workflow.write.plan')}</dt><dd><code>{prepared.planIdentitySha256 ?? prepared.supportVersion}</code></dd>
                      <dt>{t('workflow.write.support')}</dt><dd><code>{prepared.supportSha256}</code></dd>
                      <dt>{t('workflow.write.source')}</dt><dd><code>{prepared.sourceSha256}</code></dd>
                      <dt>{t('workflow.write.underlay')}</dt><dd><code>{prepared.underlaySha256}</code></dd>
                      <dt>{t('workflow.write.renderer')}</dt><dd>{prepared.renderVersion}</dd>
                    </dl>
                  </details>
                  <label class="approval">
                    <input type="checkbox" bind:checked={approved} disabled={!componentCanWrite || busy} />
                    <span>{t('workflow.write.approve', { component: prepared.componentId, page: pageNumber })}</span>
                  </label>
                  <Button variant="primary" disabled={busy || !approved || !componentCanWrite} onclick={applyPrepared}>{t('workflow.action.apply')}</Button>
                </div>
              {/if}

              <ul class="rules">
                <li>{t('workflow.rule.writesW')}</li>
                <li>{t('workflow.rule.refine')}</li>
                <li>{t('workflow.rule.reviewOnly')}</li>
              </ul>
            {/if}
          {:else}
            <p class="panel-empty">{t('workflow.panel.empty')}</p>
          {/if}
        </div>
      </div>
    </div>

    <details class="provenance">
      <summary>{t('workflow.detail.provenance')}</summary>
      <dl>
        <dt>{t('workflow.detail.sourceSha')}</dt><dd><code>{result.sourceSha256}</code></dd>
        {#if result.maskSha256}<dt>{t('workflow.detail.maskSha')}</dt><dd><code>{result.maskSha256}</code></dd>{/if}
        <dt>{t('workflow.detail.models')}</dt>
        {#if isRemote}
          <dd>{t('cloud.analysis.provenance', {
            capabilityKey: capabilityKeyOf(cloudMeta?.capability ?? result.workflow),
            name: cloudMeta?.profileName ?? '',
            providerKey: providerKeyOf(cloudMeta?.provider ?? String(result.remoteSource ?? '').split(':')[1]),
          })}</dd>
        {:else}
          <dd>{t('workflow.detail.modelsValue', {
            rtProfile: result.rtProfile ?? t('workflow.detail.off'),
            rtBackend: result.rtBackend ? backendName(result.rtBackend) : t('workflow.detail.off'),
            samBackend: result.samBackend ? backendName(result.samBackend) : t('workflow.detail.off'),
          })}</dd>
        {/if}
        {#if !isRemote && result.timingsMs}
          <dt>{t('workflow.detail.timings')}</dt>
          <dd>{t('workflow.detail.timingsValue', {
            rtLoad: result.timingsMs.rtLoad ?? 0, samLoad: result.timingsMs.samLoad ?? 0,
            samPage: (result.timingsMs.samEncoder ?? 0) + (result.timingsMs.samHead ?? 0), rtPage: result.timingsMs.rtPage ?? 0,
          })}</dd>
        {/if}
        {#if result.samBackend === 'ort-webgpu'}
          <dt>{t('workflow.detail.nodes')}</dt>
          <dd>{t('workflow.detail.nodesValue', {
            encoder: result.samWebgpuNodes?.[0] ?? 0, head: result.samWebgpuNodes?.[1] ?? 0,
            cpuEncoder: result.samCpuFallbackNodes?.[0] ?? 0, cpuHead: result.samCpuFallbackNodes?.[1] ?? 0,
          })}</dd>
        {/if}
      </dl>
    </details>
  {/if}
</section>

<style>
  .workflow-analysis {
    /* Marks drawn over the artwork. Theme-independent like app.css's
       --page-mark: the page is paper in every theme. W is the one warm hue,
       so the pixels that will be written never read as evidence. */
    --wa-evidence: rgba(2, 132, 199, .26);
    --wa-write: rgba(255, 72, 26, .72);
    --wa-add: #15803d;
    --wa-remove: #a21caf;
    container-type: inline-size;
    display: grid;
    gap: var(--s-4);
    min-width: 0;
  }
  .workflow-analysis.embedded { border-top: 1px solid var(--line); margin-top: var(--s-7); padding-top: var(--s-6) }
  h3, h4, p { margin: 0 }
  h3 { font-size: 12.5px; font-weight: 600; color: var(--text) }
  p, li, span, dd { overflow-wrap: anywhere }
  .intro { font-size: 12px; color: var(--t2); max-width: 78ch }
  code { font-size: 11px; color: var(--t2) }

  /* ---- setup ---- */
  .setup { display: flex; align-items: end; flex-wrap: wrap; gap: var(--s-3) var(--s-4) }
  .field { display: grid; gap: 3px; min-width: 0 }
  .field > span { font-size: 11px; color: var(--t3) }
  .field.source { flex: 1 1 220px }
  select, input[type='text'], input[type='number'] {
    height: 26px;
    min-width: 0;
    max-width: 100%;
    padding: 0 8px;
    border: 1px solid var(--line2);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--text);
    font-size: 12px;
  }
  select:disabled, input:disabled { opacity: .5 }
  input[type='text'] { width: 100% }
  .run-actions { display: inline-flex; align-items: center; gap: var(--s-3); margin-left: auto }
  .run-status { font-size: 12px; color: var(--t2) }
  .run-status::after { content: ''; display: inline-block; width: 36px; height: 2px; margin-left: 10px; vertical-align: middle; border-radius: 1px; background: linear-gradient(90deg, transparent, var(--t3), transparent) 0 0 / 200% 100%; animation: wa-sweep 1.1s linear infinite }
  @keyframes wa-sweep { from { background-position: 100% 0 } to { background-position: -100% 0 } }
  .models { font-size: 12px }
  .models > summary { cursor: pointer; color: var(--t2); width: max-content }
  .models-body { display: grid; gap: var(--s-4); padding-top: var(--s-4) }

  .capabilities { display: grid; gap: 1px; background: var(--line); border: 1px solid var(--line); border-radius: var(--r-md); overflow: hidden }
  .capabilities > div { display: flex; justify-content: space-between; gap: var(--s-5); padding: 8px 10px; background: var(--surface) }
  .capabilities strong { white-space: nowrap; font-size: 12px; color: var(--text) }
  .capabilities span { font-size: 12px; color: var(--t2); text-align: right }
  .actions { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s-2) }
  .verified { display: inline-flex; align-items: center; gap: 4px; font-size: 11.5px; color: var(--t2) }
  details { font-size: 12px; color: var(--t2) }
  details > summary { cursor: pointer }
  details ul { margin: 6px 0 0; padding-left: 18px }

  /* ---- outcomes ---- */
  .slot { display: grid; gap: var(--s-2) }
  .slot:empty { display: none }
  .busy { font-size: 11.5px; color: var(--t3) }

  /* ---- results ---- */
  /* Separator dots sit in the column gap; one that starts a wrapped line falls
     outside the left edge and is clipped, so no line opens on a stray dot. */
  .summary { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s-2) var(--s-4); overflow: hidden; font-size: 12px; color: var(--text) }
  .summary > span { position: relative }
  .summary > span + span::before { content: ''; position: absolute; left: calc(var(--s-4) / -2 - 1.5px); top: 50%; width: 3px; height: 3px; margin-top: -1.5px; border-radius: 50%; background: var(--t3) }
  .can-write, .cloud-source { display: inline-flex; align-items: center; gap: 4px; color: var(--t2) }
  .note { display: flex; gap: 6px; align-items: baseline; font-size: 11.5px; color: var(--t3) }
  .note strong { color: var(--t2); font-weight: 600 }

  .review {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(260px, 316px);
    gap: var(--s-6);
    align-items: start;
  }
  .stage-col { display: grid; gap: var(--s-3); position: sticky; top: 0; min-width: 0 }
  .zoombar { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s-2) }
  .zoom-range { width: 132px }
  .zoom-value { min-width: 4ch; font-size: 11.5px; color: var(--t2); font-variant-numeric: tabular-nums }
  .zoombar :global(button[aria-pressed='true']) { color: var(--text); box-shadow: inset 0 0 0 1px var(--line2) }

  .viewport {
    height: min(62vh, 640px);
    overflow: auto;
    overscroll-behavior: contain;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel2);
  }
  .stage { display: flex; justify-content: center; width: max-content; min-width: 100%; padding: 12px }
  .page-preview { display: block }
  .page-art {
    position: relative;
    display: block;
    width: max-content;
    padding: 0;
    border: 0;
    border-radius: 0;
    background: var(--paper);
    box-shadow: var(--page-shadow);
    touch-action: none;
    cursor: default;
    text-align: left;
  }
  .page-art.painting { cursor: crosshair }
  .page-art:focus-visible { outline: 2px solid var(--page-mark); outline-offset: 3px }
  .page-art img { display: block; max-width: none; height: auto; user-select: none; -webkit-user-drag: none }
  .page-art img.crisp { image-rendering: pixelated }
  .page-art .blank { display: block; background: var(--paper) }
  .pending { position: absolute; image-rendering: pixelated; pointer-events: none; opacity: .82 }
  .pending.add { color: var(--wa-add) }
  .pending.remove { color: var(--wa-remove) }
  .locator {
    position: absolute;
    outline: 2px solid var(--page-mark);
    outline-offset: 1px;
    box-shadow: 0 0 0 4px var(--page-mark-halo);
    pointer-events: none;
  }
  .locator.detector { outline-style: dashed }
  .cursor {
    position: absolute;
    z-index: 3;
    width: var(--size, 8px);
    height: var(--size, 8px);
    border: 1px solid #fff;
    border-radius: 50%;
    outline: 1px solid var(--page-ink);
    transform: translate(-50%, -50%);
    pointer-events: none;
  }

  .legend { display: flex; flex-wrap: wrap; gap: 4px var(--s-4); margin: 0; padding: 0; list-style: none; font-size: 11px; color: var(--t2) }
  .legend li { display: inline-flex; align-items: center; gap: 5px }
  .swatch { display: inline-block; flex: none; width: 10px; height: 10px; border-radius: 2px; background: var(--paper); box-shadow: inset 0 0 0 1px var(--line2) }
  .swatch.evidence { background: linear-gradient(var(--wa-evidence), var(--wa-evidence)), var(--paper) }
  .swatch.write { background: var(--wa-write) }
  .swatch.add { background: var(--wa-add); opacity: .82 }
  .swatch.remove { background: repeating-conic-gradient(var(--wa-remove) 0 25%, var(--paper) 0 50%) 0 0 / 4px 4px; opacity: .82 }
  .swatch.box { background: transparent; box-shadow: inset 0 0 0 2px var(--page-mark) }

  /* ---- side column ---- */
  .side { display: grid; gap: var(--s-4); align-content: start; min-width: 0 }
  .permission { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; font-size: 12px }
  .permission input { margin: 2px 0 0; cursor: pointer }
  .permission > div { display: grid; gap: 2px }
  .permission-label { color: var(--text); cursor: pointer }
  .help { font-size: 11px; color: var(--t3); line-height: 1.4 }

  .candidate-list {
    max-height: 236px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel);
  }
  .list-head {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 6px 10px 4px;
    background: var(--panel);
    font-size: 11px;
    font-weight: 600;
    color: var(--t3);
  }
  .candidate-list ul { margin: 0; padding: 0 4px 4px; list-style: none }
  .candidate-list button {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    width: 100%;
    min-height: 28px;
    padding: 3px 6px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--t2);
    font-size: 11.5px;
    text-align: left;
    cursor: pointer;
  }
  .candidate-list button:hover { background: var(--accent-soft); color: var(--text) }
  .candidate-list button[aria-pressed='true'] { background: var(--accent-soft); color: var(--text); box-shadow: inset 0 0 0 1px var(--line2) }
  .candidate-list button[aria-pressed='true'] .id { font-weight: 600 }
  .id { font-variant-numeric: tabular-nums; color: inherit }
  .px { font-size: 11px; color: var(--t3) }
  .tags { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 3px }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 6px;
    border-radius: var(--r-pill);
    background: var(--accent-soft);
    color: var(--t2);
    font-size: 10.5px;
    line-height: 17px;
    white-space: nowrap;
  }
  .tag.muted { background: transparent; box-shadow: inset 0 0 0 1px var(--line2); color: var(--t3) }
  .tag.held { color: var(--warn); box-shadow: inset 0 0 0 1px currentColor; background: transparent }
  .tag.reason { background: transparent; box-shadow: inset 0 0 0 1px var(--line2); border-radius: var(--r-xs) }

  .panel { display: grid; gap: var(--s-3); padding-top: var(--s-4); border-top: 1px solid var(--line); font-size: 12px; color: var(--t2) }
  .panel-head { display: flex; align-items: baseline; gap: 8px; color: var(--text) }
  .panel-head span { font-size: 11px; color: var(--t3) }
  .panel-empty { color: var(--t3) }
  .reason-note { display: flex; gap: 6px; align-items: baseline; font-size: 11.5px; color: var(--t2) }
  .group-note { font-size: 11.5px; color: var(--t3); overflow-wrap: anywhere }
  .correction-tools { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--s-3) }
  .correction-tools .wide { grid-column: 1 / -1 }
  .pending-note { font-size: 11.5px; color: var(--t2) }
  .review-only { display: grid; gap: var(--s-3) }
  .review-only > p { display: flex; gap: 6px; align-items: baseline; color: var(--text); line-height: 1.45 }
  .review-only > p :global(svg) { flex: none; transform: translateY(2px); color: var(--t2) }
  .buttons { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s-2) }
  .prepared { display: grid; gap: var(--s-3); justify-items: start; padding: 10px; border-radius: var(--r-md); background: var(--panel2) }
  .w-summary { display: inline-flex; align-items: center; gap: 6px; color: var(--text) }
  .approval { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; color: var(--text); cursor: pointer }
  .approval input { margin: 2px 0 0 }
  dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 3px 10px; margin: 6px 0 0 }
  dt { color: var(--t3) }
  dd { margin: 0; min-width: 0 }
  .rules { display: grid; gap: 4px; margin: 0; padding: 8px 0 0; border-top: 1px solid var(--line); list-style: none; font-size: 11px; color: var(--t3); line-height: 1.45 }
  .provenance { font-size: 11.5px }

  @container (max-width: 720px) {
    .review { grid-template-columns: minmax(0, 1fr) }
    .stage-col { position: static }
    .viewport { height: min(52vh, 520px) }
    .run-actions { margin-left: 0 }
  }
</style>
