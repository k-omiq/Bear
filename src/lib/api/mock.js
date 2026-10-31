/**
 * The mock engine - the only implementation of the backend interface today.
 *
 * It owns the fixture data and simulates the pipeline with timers. Every
 * method is async and progress arrives only on the event channel, because
 * that is all the real backend will be able to offer: a Tauri `invoke` cannot
 * return a streaming result, and no component may come to depend on timing
 * the real backend cannot provide.
 *
 * All latency is injected. `timing` overrides the base milliseconds and
 * `speed` divides them, so tests drive this with fake timers and a demo can
 * slow it down without touching the code.
 *
 * Nothing in this file imports from `svelte` or touches the DOM.
 */

import { createJournal, pushEntry, redoEntry, undoEntry, viewOf } from '../model/journal.js'
import { displayBbox, editedLayer, isErasableLayer, isRegionCopy, layerCapabilities, layerOf, sanitizeLayer } from '../model/layers.js'
import { regionState, reviewReason } from '../model/review.js'
import { aboutInfo, buildChapter, buildFixtures, buildProject, defaultSettings } from './fixtures.js'
import { cloudAttemptId, sha256Hex } from './attempt.js'
import { buildReviewPage, supportOf } from './mockreview.js'
import { commitMask, ENGINE_INFO } from './provenance.js'
import { createRng } from './rng.js'
import { createRunnerPool } from './runner.js'
import { DENOISE_PRESETS, outputScalesFor, presetById, recipeAt } from '../model/denoise.js'
import { currentRung } from '../model/ladder.js'
import { MAX_MASK_PADDING } from '../model/masks.js'
import { FINISHINGS } from '../model/model-names.js'
import {
  applyToolToRegion,
  cleanRegionAnyway,
  cleanRegionAutomatically,
  isOutsideHeld,
  createHandRegion,
  deleteRegionMask,
  isQueueable,
  rerunNeedsCloud,
  rerunRegionMask,
} from './tools.js'

/** Base milliseconds, before the speed factor. */
const DEFAULT_TIMING = Object.freeze({
  method: 140,
  openChapter: 220,
  export: 700,
  cloud: 900,
  analysis: 1400,
  provision: 350,
  region: 70,
  pageTail: 150,
  noticeStagger: 260,
})

const DEFAULT_TIMERS = Object.freeze({
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (id) => globalThis.clearTimeout(id),
  now: () => Date.now(),
})

/**
 * The four formats `exportChapter` can produce, and the extension each one
 * puts on a single file. Anything not here is refused by name rather than
 * substituted - `src-tauri/src/exporting.rs` is the side that decides it and
 * this is the same list, kept short so the two cannot drift far.
 */
const EXPORT_FORMATS = Object.freeze({
  PNG: 'png',
  TIFF: 'tiff',
  TIF: 'tiff',
  PSD: 'psd',
  CBZ: 'cbz',
})

/**
 * The cloud recipe a gateway advertises on `/model-info` (IC-6), pinned to an
 * immutable model revision. The native side reads these from the gateway;
 * the mock answers them itself.
 */
export const PINNED_CLOUD_MODEL_ID = 'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic'
export const PINNED_CLOUD_MODEL_REVISION = '45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd'
export const PINNED_CLOUD_RECIPE_ID = 'mc-flux2-klein-inpaint-v2'
export const CLOUD_PREPROCESSING_VERSION = '2.0.0'

/**
 * Whether the cloud render service takes the crop a render of `region` on
 * `page` cuts, as `CloudRegion::sendable_crop` answers it: the box grown by
 * the 5 px isolation radius within the page, then padded with as much as
 * 128 px context per side and snapped up to the 16 px stride, within
 * 2048 px a side and 2048 x 2048 in all (`MAX_CROP_SIDE`, `MAX_CROP_PIXELS`).
 *
 * @param {{ bbox: { x: number, y: number, w: number, h: number } }} region - its box in page percent
 * @param {{ width: number, height: number }} page
 * @returns {number|null} crop pixels, or null when the service refuses it
 */
export function cloudCropPixels(region, page) {
  const span = (/** @type {number} */ at, /** @type {number} */ length, /** @type {number} */ extent) =>
    Math.min(extent, Math.round((at + length) / 100 * extent) + 5) - Math.max(0, Math.round(at / 100 * extent) - 5)
  const side = (/** @type {number} */ extent) => Math.ceil(extent / 16) * 16
  const width = span(region.bbox.x, region.bbox.w, page.width)
  const height = span(region.bbox.y, region.bbox.h, page.height)
  for (let context = 128; context >= 0; context -= 1) {
    const w = side(width + context * 2)
    const h = side(height + context * 2)
    if (w <= 2048 && h <= 2048 && w * h <= 2048 * 2048) return w * h
  }
  return null
}

export function cloudCropFits(region, page) {
  return cloudCropPixels(region, page) !== null
}

/** The cloud analysis protocol the native side speaks (`cloud_analysis_wire::VERSION`). */
export const ANALYSIS_PROTOCOL_VERSION = '1.1.0'

/**
 * How evidence analysed on that protocol's tile plan records its spatial input
 * (`text_groups::SpatialInput::OverlappingCloudTiles`, camelCase on the wire).
 */
export const CLOUD_ANALYSIS_SPATIAL = 'overlappingCloudTiles'

/**
 * The cloud analysis tile plan, as `cleaner_core::cloud_tiles::planned` makes
 * it: along an axis longer than 1024, tiles of exactly 1024 from 0 to the far
 * edge, evenly spaced so neighbours overlap by at least 256; each tile owns
 * the core between the midpoints of its overlaps.
 *
 * @param {number} width
 * @param {number} height
 * @returns {Array<{ tile: {x: number, y: number, width: number, height: number},
 *   core: {x: number, y: number, width: number, height: number} }>}
 */
export function cloudTilePlan(width, height) {
  const axis = (extent) => {
    if (extent <= 1024) return [{ start: 0, len: extent, coreStart: 0, coreLen: extent }]
    const span = extent - 1024
    const count = 1 + Math.ceil(span / (1024 - 2 * 128))
    const origin = (k) => Math.floor(k * span / (count - 1))
    const cuts = [0]
    for (let k = 1; k < count; k += 1) cuts.push(Math.floor((origin(k - 1) + 1024 + origin(k)) / 2))
    cuts.push(extent)
    return Array.from({ length: count }, (_, k) => ({ start: origin(k), len: 1024, coreStart: cuts[k], coreLen: cuts[k + 1] - cuts[k] }))
  }
  const columns = axis(width)
  return axis(height).flatMap((row) => columns.map((column) => ({
    tile: { x: column.start, y: row.start, width: column.len, height: row.len },
    core: { x: column.coreStart, y: row.coreStart, width: column.coreLen, height: row.coreLen },
  })))
}

/**
 * The GPUs a mock plan offers, per provider. A real plan says which ones the
 * helper accepts; these stand in for that list, and Beam has no serverless L4.
 */
export const MOCK_PROVISION_GPUS = Object.freeze({
  modal: Object.freeze({ options: Object.freeze(['L4', 'A10', 'L40S']), fallback: 'L4' }),
  beam: Object.freeze({ options: Object.freeze(['RTX4090', 'A10G', 'RTX5090']), fallback: 'RTX4090' }),
})

/** The steps an `apply` walks through, in the order the helper reports them (IC-2). */
export const PROVISION_APPLY_STEPS = Object.freeze([
  'validate',
  'volume',
  'state',
  'secret',
  'image',
  'deploy',
  'weights',
  'token',
  'endpoint',
  'health',
])

/** @param {string} value */
/**
 * The mock's own copy of `history::UNDO_LIMIT_*`. It is a backend, so it owns
 * these as the Rust side owns its own; the interface asks `undoLimit()`.
 */
const UNDO_LIMIT = Object.freeze({ min: 20, max: 2000, default: 500 })

function isAbsolutePath(value) {
  return /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(String(value ?? ''))
}

/**
 * The strip is as wide as its widest
 * page and the rest are centred, so a narrower page contributes the columns
 * beside it - pixels no source file has. `ExportDialog.svelte` computes the
 * same figure to state it before the run; this is what a backend reports
 * having written.
 *
 * @param {Array<{width?: number, height?: number}>} pages
 */
function gutterPixels(pages) {
  const width = pages.reduce((widest, page) => Math.max(widest, page.width ?? 0), 0)
  return pages.reduce((sum, page) => sum + (width - (page.width ?? 0)) * (page.height ?? 0), 0)
}

/**
 * @typedef {Object} MockOptions
 * @property {number} [speed] - divides every delay; 2 is twice as fast
 * @property {Partial<typeof DEFAULT_TIMING>} [timing] - base milliseconds, before `speed`
 * @property {{ setTimeout: Function, clearTimeout: Function, now: () => number }} [timers] - injectable clock
 * @property {string} [seed] - seeds the session generator; equal seeds give equal sessions
 */

/**
 * The capabilities a mock run sends to the cloud, as `cloudCapabilitiesFor`
 * in `model/pipelines.js` answers it for the interface.
 *
 * @param {unknown} detectorModels
 * @param {unknown} targets
 * @returns {string[]}
 */
function mockCloudCapabilities(detectorModels, targets) {
  const models = Array.isArray(detectorModels) ? detectorModels : []
  const routed = targets && typeof targets === 'object' ? /** @type {Record<string, unknown>} */ (targets) : {}
  const capabilities = []
  if (models.includes('samTs') && routed.samTs === 'cloud') capabilities.push('text_mask_sam_ts@1')
  if (models.includes('rtFull') && routed.rtFull === 'cloud') capabilities.push('text_regions_rt@1')
  return capabilities.sort()
}

/**
 * @param {MockOptions} [options]
 * @returns {import('./backend.js').Backend}
 */
export function createMockBackend(options = {}) {
  const speed = options.speed ?? 1
  const timers = options.timers ?? DEFAULT_TIMERS
  const base = { ...DEFAULT_TIMING, ...(options.timing ?? {}) }
  const timing = Object.fromEntries(
    Object.entries(base).map(([key, ms]) => [key, Math.max(0, Math.round(ms / speed))]),
  )

  const defaultInferenceConfig = () => ({
    schemaVersion: 1,
    selectedTarget: { type: 'local' },
    beamProfiles: {},
    modalProfiles: {},
  })

  /**
   * How the mock cloud GPU misbehaves, if at all: `stale`, `unknown` or
   * `missingCapability` (see `confirmRemoteAnalysis`), or `runFail`, which
   * fails an Auto clean's cloud detection on its first page (see `runClean`).
   * From the options, or a `?remoteAnalysis=` knob so the review and the run
   * can be seen failing in a browser.
   */
  const remoteScenario = () =>
    options.remoteAnalysisScenario ?? new URLSearchParams(globalThis.location?.search ?? '').get('remoteAnalysis')
  const pendingAnalyses = new Map()
  const usedAnalysisRequests = new Set()
  const remoteAnalyses = new Map()
  /** Auto clean's cloud consent: open proposals, and grants not yet spent. */
  const runProposals = new Map()
  const runGrants = new Map()
  /** The cloud clean's consent: open proposals, and grants not yet spent. */
  const cleanProposals = new Map()
  const cleanGrants = new Map()
  /** A stream's start-up and idle window, and one region, at an L4's list price. */
  const MOCK_STREAM_FIXED_USD = 0.15
  const MOCK_STREAM_REGION_USD = 0.002
  /** Page denoise's cloud consent: open proposals, and grants not yet spent. */
  const denoiseProposals = new Map()
  const denoiseGrants = new Map()
  /**
   * How the mock page denoise fails, if at all: `pageFail` fails the third
   * page of a run with an inference error, the rest are written. From the
   * options, or a `?denoise=` knob.
   */
  const denoiseScenario = () =>
    options.denoiseScenario ?? new URLSearchParams(globalThis.location?.search ?? '').get('denoise')
  /** As `history::valid_limit` and `history::limit_from`. */
  const validUndoLimit = (/** @type {unknown} */ value) =>
    Number.isInteger(value) && Number(value) >= UNDO_LIMIT.min && Number(value) <= UNDO_LIMIT.max
  const undoLimitValue = () =>
    validUndoLimit(state.settings.undoLimit) ? state.settings.undoLimit : UNDO_LIMIT.default
  /** `missing`: the chapter's scan folder is on a drive that is not connected. */
  const exportDrive =
    options.exportDrive ?? new URLSearchParams(globalThis.location?.search ?? '').get('exportDrive')
  /** Whether any local page denoise file is still to download. */
  const denoiseMissing = () => ['pageDenoiseModel', 'pageDenoiseSeams'].some((id) => state.missingModels.has(id))
  /** @param {unknown} outDir */
  const checkOutDir = (outDir) => {
    const path = typeof outDir === 'string' ? outDir.trim() : ''
    if (!path || !/^(\/|~|[A-Za-z]:[\\/])/.test(path)) throw new Error('denoise_out_dir_invalid')
  }
  /** @param {any} chapter @param {number[]|null} indices */
  const denoisePages = (chapter, indices) => {
    const wanted = Array.isArray(indices) ? new Set(indices) : null
    return chapter.pages.filter((/** @type {any} */ page) => !wanted || wanted.has(page.index))
  }
  /** @param {any[]} pages @param {string} outDir */
  const denoiseReport = (pages, outDir, prefix) => {
    const dir = outDir.trim().replace(/[\\/]+$/, '')
    const failing = denoiseScenario() === 'pageFail' ? pages[2]?.index : undefined
    return {
      written: pages.filter((page) => page.index !== failing).map((page) => ({
        pageIndex: page.index,
        path: `${dir}/${String(page.index + 1).padStart(3, '0')}.png`,
        gray: true,
      })),
      failed: failing === undefined ? [] : [{ pageIndex: failing, code: `${prefix}inference_failed` }],
    }
  }
  /**
   * Each chapter's denoise history, as the manifest's `denoised` rows keep
   * it (`Job::record_denoised`): one row per page per run, a run being the
   * rows sharing `created`. A rerun into the same folder replaces the rows
   * whose files it wrote over, a run in the same second as a held one moves
   * a second on, and only the newest `MAX_DENOISE_RUNS` runs are kept.
   * `scale` is the size the file was saved at, as a multiple of the page's.
   *
   * @typedef {{pageIndex: number, path: string, created: number, preset: string|null, scale: number,
   *   target: 'local'|'cloud', fromCleaned: boolean, taken: boolean}} MockDenoiseRow
   * @type {Map<string, MockDenoiseRow[]>}
   */
  const denoiseRows = new Map()
  const MAX_DENOISE_RUNS = 10
  /** @param {any} page */
  const pageCleaned = (page) => page.regions.some((/** @type {any} */ region) => regionState(region) === 'cleaned')
  /**
   * @param {any} chapter
   * @param {{written: Array<{pageIndex: number, path: string}>}} report
   * @param {{preset: string|null, target: 'local'|'cloud', scale?: number}} run
   */
  const recordDenoised = (chapter, report, { preset, target, scale = 1 }) => {
    if (!report.written.length) return
    let rows = (denoiseRows.get(chapter.id) ?? []).filter((row) => !report.written.some((output) => output.path === row.path))
    let created = Math.floor(Date.now() / 1000)
    if (rows.some((row) => row.created === created)) created = Math.max(...rows.map((row) => row.created)) + 1
    for (const output of report.written) {
      const page = chapter.pages.find((/** @type {any} */ entry) => entry.index === output.pageIndex)
      rows.push({ pageIndex: output.pageIndex, path: output.path, created, preset, target, scale,
        fromCleaned: page ? pageCleaned(page) : false, taken: false })
    }
    const runs = [...new Set(rows.map((row) => row.created))].sort((a, b) => b - a)
    if (runs.length > MAX_DENOISE_RUNS) rows = rows.filter((row) => row.created >= runs[MAX_DENOISE_RUNS - 1])
    denoiseRows.set(chapter.id, rows)
  }
  /**
   * `Project::current_denoised`: the page's newest row, unless it was taken.
   *
   * @param {string} chapterId @param {number} pageIndex
   * @returns {MockDenoiseRow|null}
   */
  const currentDenoised = (chapterId, pageIndex) => {
    /** @type {MockDenoiseRow|null} */
    let newest = null
    for (const row of denoiseRows.get(chapterId) ?? []) {
      if (row.pageIndex === pageIndex && (!newest || row.created >= newest.created)) newest = row
    }
    return newest && !newest.taken ? newest : null
  }
  /**
   * `page_denoise.rs#standings`, page by page: a page with no current
   * denoised file is missing and stays raw (a run that failed it, as
   * `?denoise=pageFail` does); a cleaned page, or one denoised from its
   * cleaning, is kept; the rest are ready, a file saved larger than the page
   * among them. The mock's files never go missing and its pages never change
   * under a run.
   *
   * @param {any} chapter
   * @returns {{ready: number[], kept: number[], missing: number[]}}
   */
  const replacementPlan = (chapter) => {
    /** @type {{ready: number[], kept: number[], missing: number[]}} */
    const plan = { ready: [], kept: [], missing: [] }
    for (const page of chapter.pages) {
      const current = currentDenoised(chapter.id, page.index)
      if (!current) plan.missing.push(page.index)
      else if (pageCleaned(page) || current.fromCleaned) plan.kept.push(page.index)
      else plan.ready.push(page.index)
    }
    return plan
  }
  /** @param {any} chapter */
  const denoiseReplacementOf = (chapter) => {
    const plan = replacementPlan(chapter)
    return plan.ready.length ? { pages: plan.ready.length, kept: plan.kept.length, missing: plan.missing.length } : null
  }
  /** `denoise_history::summary`. @param {any} chapter */
  const denoiseHistoryOf = (chapter) => {
    const rows = denoiseRows.get(chapter.id) ?? []
    if (!rows.length) return null
    const runs = new Set(rows.map((row) => row.created))
    return { runs: runs.size, latest: Math.max(...runs), taken: rows.some((row) => row.taken) }
  }
  /**
   * A stand-in page for the compare view: the page's panels on paper, and on
   * the raw side a grain over it, so the wipe has something to show. SVG, as
   * the mock has no encoder; the view sniffs the type from the bytes. A file
   * saved larger says so in its own size, as a real one does, so the view is
   * shown two sides of different sizes.
   *
   * @param {any} page @param {boolean} raw @param {number} [scale]
   */
  const compareArt = (page, raw, scale = 1) => {
    const height = Math.round(100 * (page.height ?? 1400) / (page.width ?? 1000))
    const panels = (page.panels ?? []).map((/** @type {any} */ panel) =>
      `<rect x="${panel.x}" y="${panel.y * height / 100}" width="${panel.w}" height="${panel.h * height / 100}" fill="none" stroke="#222" stroke-width="0.6"/>`).join('')
    const grain = raw
      ? '<filter id="g"><feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2"/><feColorMatrix values="0 0 0 0 0.4 0 0 0 0 0.4 0 0 0 0 0.4 0 0 0 0.55 0"/></filter>'
        + `<rect width="100" height="${height}" filter="url(#g)"/>`
      : ''
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 ${height}" width="${(page.width ?? 1000) * scale}" height="${(page.height ?? 1400) * scale}">`
      + `<rect width="100" height="${height}" fill="${raw ? '#e9e4d8' : '#fbfaf7'}"/>${panels}${grain}</svg>`
    return new TextEncoder().encode(svg)
  }
  /**
   * Each project's standing cloud consent, by project id (`cloud_consent` in
   * library.rs): the first consent given in a project covers the rest of it,
   * for the same endpoint. Proposals and single-use grants are unchanged.
   * Only a consent given with both statements ticked stands for them, so a
   * standing proposal is confirmed without them.
   *
   * @type {Map<string, {provider: string, profileId: string, originFingerprint: string, grantedAt: number, statementsAccepted: boolean}>}
   */
  const standingConsents = new Map()
  /** @param {string} chapterId @param {string} provider @param {string} profileId @param {string} fingerprint */
  const standingFor = (chapterId, provider, profileId, fingerprint) => {
    const held = standingConsents.get(findChapter(chapterId)?.project.id)
    return !!held && held.statementsAccepted && held.provider === provider && held.profileId === profileId
      && held.originFingerprint === fingerprint
  }
  /**
   * Record the project's standing consent, only for an answer that ticked
   * both statements (`record_cloud_consent`).
   *
   * @param {string} chapterId @param {string} provider @param {string} profileId @param {string} fingerprint
   * @param {boolean} statementsAccepted
   */
  const recordStanding = (chapterId, provider, profileId, fingerprint, statementsAccepted) => {
    const project = findChapter(chapterId)?.project
    if (statementsAccepted && project && !standingFor(chapterId, provider, profileId, fingerprint)) {
      standingConsents.set(project.id, { provider, profileId, originFingerprint: fingerprint, grantedAt: Date.now(),
        statementsAccepted })
    }
  }
  /**
   * The statements a confirm needs: none for a standing proposal, whose
   * project holds them, else both (`analysis::require_consent`).
   *
   * @param {boolean} standing @param {unknown} rightsAttested @param {unknown} retentionAcknowledged
   */
  const requireStatements = (standing, rightsAttested, retentionAcknowledged) => {
    if (standing) return
    if (rightsAttested !== true) throw new Error('rights_attestation_required')
    if (retentionAcknowledged !== true) throw new Error('retention_acknowledgement_required')
  }
  /**
   * How the mock cloud clean fails, if at all: `startFail` refuses to start
   * (`credential_missing`), `regionFail` fails the batch's first region, which
   * stays detected, and `stop` ends the batch at its first region as a
   * refused key does. From the options, or a `?cloudClean=` knob.
   */
  const cloudCleanScenario = () =>
    options.cloudCleanScenario ?? new URLSearchParams(globalThis.location?.search ?? '').get('cloudClean')
  /** Whether the endpoint a cloud clean was proposed for is still the selected one. */
  let nextAnalysisRequest = 0
  /**
   * A local analysis: `produce` runs after `timing.analysis` unless the
   * request is cancelled first, which rejects with the native cancellation.
   * Registered synchronously, so a cancel that follows the call finds it.
   */
  const pendingAnalysis = (requestId, produce = () => { throw new Error('Model review requires the desktop runtime') }) => {
    const id = requestId ?? `analysis-${Date.now()}-${++nextAnalysisRequest}`
    if (usedAnalysisRequests.has(id)) return Promise.reject(new Error('Analysis request id was already used'))
    usedAnalysisRequests.add(id)
    return new Promise((resolve, reject) => {
      const timer = timers.setTimeout(() => {
        pendingAnalyses.delete(id)
        try { resolve(produce()) } catch (error) { reject(error) }
      }, timing.analysis)
      pendingAnalyses.set(id, { reject, timer })
    })
  }
  const fixtures = buildFixtures()
  const state = {
    projects: fixtures.projects,
    settings: defaultSettings(),
    inferenceConfig: defaultInferenceConfig(),
    /**
     * Which cloud secrets this session holds, as `provider:profileId:role`.
     * Presence only: the value is never kept, because nothing here needs it.
     *
     * @type {Set<string>}
     */
    secrets: new Set(),
    /** Interactive cloud renders in flight, by attempt id. @type {Map<string, any>} */
    cloudJobs: new Map(),
    /** Whether a render has run this session: the first one pays the cold start. */
    cloudWarm: false,
    /** Installations the mock provisioner knows, by `provider:installationId`. @type {Map<string, any>} */
    installations: new Map(),
    /** Setups an Update has redeployed, by installation key (`checkCloudRelease`). */
    updatedReleases: new Set(),
    /** The provisioner run in flight, if any. @type {any} */
    provisionRun: null,
    /** Profile mutation epochs keyed by profileId. @type {Map<string, number>} */
    profileEpochs: new Map(),
    /** Cached consent proposals. @type {Map<string, any>} */
    cachedProposals: new Map(),
    /** Cached authorization grants. @type {Map<string, any>} */
    cachedGrants: new Map(),
    /** Attempt journal state machine. @type {Map<string, any>} */
    attemptJournal: new Map(),
    /**
     * Which catalogue rows this machine does *not* have. The redraw model
     * starts here on purpose - see `listModels` - so the engine gating is
     * visible in a browser rather than only inside a Tauri window with a
     * half-empty models directory. It used to be MI-GAN's row, which was the
     * cheapest one to be missing; that rung is gone and the inpainter is what
     * is left to withhold.
     *
     * @type {Set<string>}
     */
    // The redraw model, and all three parts of the rescue reader. See
    // `listModels` for why anything is missing at all; the reader is here
    // because *absent* is its real default - it is the one catalogue row
    // nothing requires, so a browser that showed it installed would never draw
    // an optional row with something to download.
    missingModels: new Set(['inpainter', 'ocrEncoder', 'ocrDecoder', 'ocrVocab', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer', 'pageDenoiseModel', 'pageDenoiseSeams']),
    /**
     * Free memory the native side would measure, in bytes. Under the 5 GB the
     * lettering mask wants, so Advanced runs show their low-memory warning.
     */
    memoryRoomBytes: 4 * 1024 ** 3,
    /** What `verifyModel` has digested this session. @type {Map<string, boolean>} */
    verifiedModels: new Map(),
    /** Downloads in flight, by id, holding the timer that advances each one. @type {Map<string, any>} */
    downloads: new Map(),
    /**
     * What a stopped download left behind, by id: the bytes a `.part` file
     * would be holding on a real machine.
     *
     * The native side keeps the prefix so the next press can resume from it
     * and reports it as `partialBytes` so the row can offer to
     * throw it away. Here it is a number, which is enough to drive both
     * halves of the interface - the line under the row and the Discard press -
     * in a browser.
     *
     * @type {Map<string, number>}
     */
    partials: new Map(),
  }
  const sequence = { value: fixtures.sequence }
  const rng = createRng(options.seed ?? 'session')

  /** @type {Set<(event: Object) => void>} */
  const handlers = new Set()
  /** `provision://progress` listeners (IC-2). @type {Set<(payload: Object) => void>} */
  const provisionListeners = new Set()
  /** `cloud://attempt` listeners (IC-3). @type {Set<(payload: Object) => void>} */
  const attemptListeners = new Set()
  const remoteAnalysisListeners = new Set()
  /** `denoise://progress` listeners. @type {Set<(payload: Object) => void>} */
  const denoiseProgressListeners = new Set()
  /**
   * Denoise runs in flight, local and cloud, by run id: whether each was told
   * to stop, and how far it is, for `listJobs`.
   *
   * @type {Map<string, {stopped: boolean, kind: 'denoise'|'cloudDenoise', chapterId: string, done: number, total: number}>}
   */
  const denoiseRuns = new Map()
  /** `app://quit-requested` listeners. @type {Set<(payload: Object) => void>} */
  const quitListeners = new Set()
  /** `sam-install://progress` listeners. @type {Set<(payload: Object) => void>} */
  const samInstallListeners = new Set()
  const emitTo = (listeners, payload) => {
    for (const listener of listeners) listener(structuredClone(payload))
  }
  /** Resolves to an unlisten function, as Tauri's `listen` does. */
  const listenOn = (listeners, handler) => {
    listeners.add(handler)
    return Promise.resolve(() => {
      listeners.delete(handler)
    })
  }
  let noticeCounter = 0
  let projectCounter = 0
  let handCounter = 0
  let mockEraseCount = 0
  /** Numbers each Detect run's undo group. */
  let detectRuns = 0

  const emit = (event) => {
    const frozen = structuredClone(event)
    for (const handler of handlers) handler(frozen)
  }

  /**
   * @param {string} key
   * @param {Object} [params]
   * @param {'info'|'warn'} [tone]
   */
  const notify = (key, params = {}, tone = 'info') => {
    noticeCounter += 1
    emit({ type: 'notice', id: `notice-${noticeCounter}`, key, params, tone })
  }

  const notifyAll = (notices) => {
    for (const notice of notices) notify(notice.key, notice.params, notice.tone)
  }

  const delay = (ms) => new Promise((resolve) => timers.setTimeout(resolve, ms))

  const toolContext = {
    rng,
    nextSequence: () => {
      sequence.value += 1
      return sequence.value
    },
    created: () => new Date(timers.now()).toISOString(),
    settings: state.settings,
  }

  /* ---------- lookups ---------- */

  const findProject = (projectId) => state.projects.find((p) => p.id === projectId) ?? null

  /** @returns {{ project: Object, chapter: Object }|null} */
  const findChapter = (chapterId) => {
    for (const project of state.projects) {
      const chapter = project.chapters.find((c) => c.id === chapterId)
      if (chapter) return { project, chapter }
    }
    return null
  }

  const findRegion = (regionId) => {
    for (const project of state.projects) {
      for (const chapter of project.chapters) {
        for (const page of chapter.pages) {
          const region = page.regions.find((r) => r.id === regionId)
          if (region) return { project, chapter, page, region }
        }
      }
    }
    return null
  }

  const findPage = (pageId) => {
    for (const project of state.projects) {
      for (const chapter of project.chapters) {
        const page = chapter.pages.find((p) => p.id === pageId)
        if (page) return page
      }
    }
    return null
  }

  const findMask = (maskId) => {
    const regionId = maskId.slice(0, maskId.lastIndexOf('-m'))
    const found = findRegion(regionId)
    return found && found.region.mask && found.region.mask.id === maskId ? found : null
  }

  /* ---------- the text-shaped review ---------- */

  /**
   * What the native `ReviewStore` holds: one analysis and one prepared write
   * at a time, plus the corrections each written component was last applied
   * with (the native side keeps those in the chapter's plan sidecars).
   *
   * The pages are drawn by `mockreview.js`, once per page geometry: the same
   * page analyzed twice is the same picture, so a saved correction still lines
   * up with it.
   */
  const review = {
    /** @type {any} */ analysis: null,
    /** @type {any} */ prepared: null,
    /** @type {Map<string, any>} */ saved: new Map(),
    /** @type {Map<string, import('./mockreview.js').ReviewPage>} */ pages: new Map(),
    /** Imported model graphs: both present, so the review can be exercised in a browser. */
    models: { fullRt: true, sam: true },
    count: 0,
  }

  const reviewPageFor = (width, height, panels) => {
    const key = `${width}x${height}:${JSON.stringify(panels ?? null)}`
    let page = review.pages.get(key)
    if (!page) {
      page = buildReviewPage({ width, height, panels })
      review.pages.set(key, page)
    }
    return page
  }

  /**
   * The evidence one workflow sees on a drawn page. Regions only has no mask,
   * so every box is detector-only; mask only has no boxes, so every component
   * is outside every bubble. Text groups are drawn for the fused workflow
   * only: the mock does not imitate native layout grouping, so a single-model
   * workflow reports no groups and no reasons.
   */
  const evidenceFor = (page, { rt, sam, cloud = false, small = false }) => {
    const fused = rt && sam
    const components = sam
      ? page.components.map((component) => ({ ...structuredClone(component),
        ...(rt ? {} : { rtTextIds: [], rtBubbleIds: [], reviewReasons: [], groupId: null }) }))
      : []
    const regions = rt
      ? page.regions.map((region) => ({ ...structuredClone(region),
        ...(sam ? {} : { componentIds: [], detectorOnly: true }) }))
      : []
    return {
      width: page.width,
      height: page.height,
      components,
      regions,
      links: fused ? structuredClone(page.links) : [],
      groupingSuggestions: [],
      groups: fused ? structuredClone(page.groups) : [],
      models: [
        ...(sam ? [{ model: 'samTsL', execution: cloud ? 'cloud' : 'local',
          spatial: cloud ? CLOUD_ANALYSIS_SPATIAL : 'whole' }] : []),
        ...(rt ? [{ model: small ? 'ogkaluSmall' : 'ogkaluFull', execution: cloud ? 'cloud' : 'local',
          spatial: cloud ? CLOUD_ANALYSIS_SPATIAL : small ? 'whole' : 'halves' }] : []),
      ],
      maskPixels: sam ? page.maskPixels : 0,
    }
  }

  const reviewRegionId = (pageId, componentId) => `${pageId}-hreview-${componentId}`

  /**
   * A remote analysis's journal record, snake_case as `journal.rs` writes it:
   * the tile index on the phases that have one, the failure code on `failed`,
   * and `cancel_requested` only once the user has asked.
   */
  const statusOf = (proposalId, record) => {
    const phase = { phase: record.phase }
    if (['submitted_tile', 'result_cached_tile', 'unknown_remote_state'].includes(record.phase)) phase.index = record.index ?? 0
    if (record.phase === 'failed') phase.code = record.code ?? 'analysis_failed'
    const { proposal } = record
    return { created_at_ms: proposal.issuedAtMs, schema_version: 2, proposal_id: proposalId, provider: proposal.provider,
      profile_id: proposal.profileId, capability: proposal.capability,
      source_sha256: proposal.sourcePageSha256, underlay_sha256: proposal.underlaySha256,
      total_tiles: proposal.tiles.length * (proposal.models?.length || 1), completed_tiles: record.completedTiles,
      reported_cost_usd: null, tile_submission_started: record.tileSubmissionStarted,
      ...(record.cancelled ? { cancel_requested: true } : {}), phase }
  }

  /**
   * One local analysis of a drawn page, checked the way
   * `model_workflows.rs#analyze_capabilities` checks its request.
   */
  const analyzePage = ({ page, target, sourceMime, workflow, rtProfile, rtBackend, samBackend }) => {
    const ctd = ['ctd', 'ctd_regions', 'ctd_mask', 'ctd_text_shape'].includes(workflow)
    const rt = ['regions', 'text_shape', 'ctd_regions', 'ctd_text_shape'].includes(workflow)
    const sam = ['mask', 'text_shape', 'ctd_mask', 'ctd_text_shape'].includes(workflow)
    if (!rt && !sam && !ctd) throw new Error('Choose a supported detection model combination')
    if (ctd && state.missingModels.has('textDetector')) throw new Error('The Comic Text Detector (CTD) graph is not installed')
    if (rt && rtBackend !== 'ort-cpu') throw new Error(`RT backend ${rtBackend} is not qualified for this analysis path`)
    if (sam && !['ort-cpu', 'ort-webgpu'].includes(samBackend)) {
      throw new Error(`SAM backend ${samBackend} is unavailable for this analysis path`)
    }
    if (state.missingModels.has(MOCK_RUNTIME_ID)) throw new Error('ONNX Runtime is not installed')
    if (rt && rtProfile === 'full-halves' && state.missingModels.has('fullRt')) throw new Error('The Ogkalu comic text & bubble detector (Full) graph is not installed or failed SHA-256')
    if (rt && rtProfile === 'small-whole' && state.missingModels.has('balloonDetector')) {
      throw new Error('The Ogkalu comic text & bubble detector (Small) graph is not installed or failed SHA-256')
    }
    if (rt && !['full-halves', 'small-whole'].includes(rtProfile)) throw new Error('Choose the Full (tiled) or installed Small Ogkalu detector profile')
    if (sam && !review.models.sam) throw new Error('The SAM-TS-L lettering mask graphs are not installed')
    const webgpu = sam && samBackend === 'ort-webgpu'
    const eligible = webgpu && sourceMime === 'image/png'
    const sourceSha256 = sha256Hex(`review-source:${target.key}`)
    let analysisId = null
    if (sam) {
      review.count += 1
      analysisId = sha256Hex(`analysis-v1:${samBackend}:${sourceSha256}:${review.count}`)
      review.analysis = { id: analysisId, remote: false, eligible, page, sourceSha256, ...target,
        bubbled: new Set(rt ? page.components.filter((c) => c.rtBubbleIds.length).map((c) => c.id) : []) }
      review.prepared = null
    }
    return {
      analysisId,
      sourceSha256,
      rtModelSha256: rt ? (rtProfile === 'full-halves' ? '065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195'
        : '5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79') : null,
      samEncoderSha256: sam ? 'c7b1cc85d2316886db1dfd22886c1988c71c317d4bcd869beca07e8b6c822141' : null,
      samHeadSha256: sam ? 'a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281' : null,
      maskSha256: sam ? sha256Hex(`review-mask:${target.key}`) : null,
      workflow,
      rtProfile: rt ? rtProfile : null,
      rtBackend: rt ? 'ort-cpu' : null,
      samBackend: sam ? samBackend : null,
      remoteSource: null,
      samWriteEligible: eligible,
      samWebgpuNodes: webgpu ? [412, 37] : null,
      samCpuFallbackNodes: webgpu ? [0, 0] : null,
      evidence: evidenceFor(page, { rt, sam, small: rtProfile !== 'full-halves' }),
      sourceDataUrl: page.sourceDataUrl.replace('data:image/png', `data:${sourceMime}`),
      maskDataUrl: sam ? page.maskDataUrl : null,
      timingsMs: { rtLoad: rt ? 820 : 0, rtPage: rt ? 1410 : 0, samLoad: sam ? 2310 : 0, samPrepare: sam ? 30 : 0,
        samEncoder: sam ? (webgpu ? 640 : 5120) : 0, samHead: sam ? 90 : 0, samRestore: sam ? 20 : 0 },
    }
  }

  /** The chapter page an analysis or a proposal names, refused as the native commands refuse it. */
  const reviewTarget = (chapterId, pageIndex, refusal) => {
    const found = findChapter(chapterId)
    if (!found) throw new Error(`no such chapter: ${chapterId}`)
    // A run's cloud detection takes a long strip page by page; the review does not.
    if (refusal && found.project.mode === 'longstrip') throw new Error(refusal)
    const page = found.chapter.pages.find((candidate) => candidate.index === pageIndex)
    if (!page) throw new Error('Page is no longer in chapter')
    const format = String(found.chapter.sourceFormat ?? 'PNG').toUpperCase()
    return {
      page,
      drawn: reviewPageFor(page.width, page.height, page.panels),
      sourceMime: format === 'JPG' || format === 'JPEG' ? 'image/jpeg' : 'image/png',
    }
  }

  /* ---------- the run scheduler ---------- */

  /* ---------- stored detections (docs/detect-clean.md) ---------- */

  const isDetection = (region) => region.outcome === 'detected'
  const hasDetections = (page) => page.regions.some(isDetection)
  const detectionsOf = (page) => page.regions.filter(isDetection)

  /** A Mask padding the native side takes: a whole number of pixels, 0 to 32. */
  const validPadding = (value) => Number.isInteger(value) && value >= 0 && value <= MAX_MASK_PADDING

  /** How many mask edits the mock has answered, so each one names a new digest. */
  let maskEdits = 0

  /**
   * The pages a mask edit anchored on `page` reaches, each with where it
   * sits in the anchor page's percent: `top` and `height` on the anchor's y
   * axis, `left` and `width` on its x axis, pages centred in the strip as the
   * native strip lays them. The anchor alone, at 0 to 100, on a chapter of
   * single pages; on a longstrip one page more each way, as
   * `region.rs#mask_edit_pages` reaches.
   */
  const maskEditFrame = (found, page) => {
    const anchor = { page, top: 0, height: 100, left: 0, width: 100 }
    if (found.project.mode !== 'longstrip') return [anchor]
    const width = (candidate) => candidate.width || 1600
    const height = (candidate) => candidate.height || 2400
    const near = [page.index - 1, page.index + 1]
      .map((index) => found.chapter.pages.find((candidate) => candidate.index === index))
      .filter(Boolean)
    const strip = Math.max(width(page), ...near.map(width))
    const centred = (candidate) => (strip - width(candidate)) / 2
    return [anchor, ...near.map((other) => ({
      page: other,
      top: (other.index < page.index ? -height(other) : height(page)) / height(page) * 100,
      height: height(other) / height(page) * 100,
      left: (centred(other) - centred(page)) / width(page) * 100,
      width: width(other) / width(page) * 100,
    }))]
  }

  /** A box in `entry`'s page percent, in the anchor page's. The anchor's own is left as it is. */
  const toAnchor = (entry, box) => entry.top === 0 && entry.left === 0 && entry.width === 100 && entry.height === 100
    ? box
    : { x: entry.left + box.x * entry.width / 100, y: entry.top + box.y * entry.height / 100,
        w: box.w * entry.width / 100, h: box.h * entry.height / 100 }

  /** A box in the anchor page's percent, in `entry`'s page percent: past 0 to 100 where it hangs. */
  const fromAnchor = (entry, box) => entry.top === 0 && entry.left === 0 && entry.width === 100 && entry.height === 100
    ? box
    : { x: (box.x - entry.left) * 100 / entry.width, y: (box.y - entry.top) * 100 / entry.height,
        w: box.w * 100 / entry.width, h: box.h * 100 / entry.height }

  /**
   * The box around a mask edit's gesture, in the anchor page's percent and
   * clipped to the pages `frame` names, or `null` when there is no gesture or
   * it covers none of them. A stroke's radius is in page pixels, so it is
   * taken to percent per axis.
   */
  const gestureBox = (page, stroke, painted, frame) => {
    const points = (stroke?.points ?? painted?.points ?? [])
      .filter((point) => Number.isFinite(point?.x) && Number.isFinite(point?.y))
    if (points.length === 0) return null
    const reach = stroke ? Math.max(0, Number(stroke.radius) || 0) : Math.max(0, Number(painted?.feather) || 0)
    const rx = (reach / (page.width || 1600)) * 100
    const ry = (reach / (page.height || 2400)) * 100
    const left = Math.max(Math.min(...frame.map((entry) => entry.left)), Math.min(...points.map((point) => point.x)) - rx)
    const top = Math.max(Math.min(...frame.map((entry) => entry.top)), Math.min(...points.map((point) => point.y)) - ry)
    const right = Math.min(Math.max(...frame.map((entry) => entry.left + entry.width)),
      Math.max(...points.map((point) => point.x)) + rx)
    const bottom = Math.min(Math.max(...frame.map((entry) => entry.top + entry.height)),
      Math.max(...points.map((point) => point.y)) + ry)
    if (!(right > left) || !(bottom > top)) return null
    return { x: left, y: top, w: right - left, h: bottom - top }
  }

  /** The area two boxes share, in square page percent. */
  const overlapArea = (a, b) =>
    Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x)) *
    Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y))

  /** The smallest box holding both. */
  const unionBox = (a, b) => {
    const x = Math.min(a.x, b.x)
    const y = Math.min(a.y, b.y)
    return { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y }
  }

  /**
   * Grow a detection to `padding` page pixels past its unpadded box, as
   * `region.rs#set_detection_padding` grows its masks: always from the box
   * Detect or the last mask edit left (`baseBbox`), never from a grown one,
   * so 0 gives the unpadded box back. The mask moves to a new digest, as a
   * native one would.
   *
   * @returns {boolean} whether anything changed
   */
  const padDetection = (region, page, padding) => {
    if ((region.paddingPx ?? 0) === padding) return false
    const base = region.baseBbox ?? region.bbox
    const px = (padding / (page.width || 1600)) * 100
    const py = (padding / (page.height || 2400)) * 100
    const x = Math.max(0, base.x - px)
    const y = Math.max(0, base.y - py)
    region.bbox = padding === 0 ? { ...base } : {
      x, y, w: Math.min(100, base.x + base.w + px) - x, h: Math.min(100, base.y + base.h + py) - y,
    }
    if (padding === 0) {
      delete region.baseBbox
      delete region.paddingPx
    } else {
      region.baseBbox = base
      region.paddingPx = padding
    }
    maskEdits += 1
    if (region.mask?.provenance) region.mask.provenance.mask_sha256 = sha256Hex(`mask-pad:${region.id}:${maskEdits}`)
    if (region.mask) region.mask.sequence = (Number.isInteger(region.mask.sequence) ? region.mask.sequence : 1) + 1
    return true
  }

  /**
   * What a page is once a step has been through it: `detected` while a
   * detection waits, `cleaned` when something on it is cleaned and nothing is
   * left pending, `unclean` otherwise. Detect never marks a page examined.
   */
  const settledStatus = (page) =>
    hasDetections(page)
      ? 'detected'
      : page.regions.some((region) => region.mask) && !page.regions.some((region) => region.outcome === 'pending')
        ? 'cleaned'
        : 'unclean'

  /**
   * Detect one region: the mask the automatic pass would clean with is fitted
   * and stored, and the rung it would start at is kept as `pick`. Nothing on
   * the page changes. The mock fits by committing and then taking the patch
   * back, which leaves exactly the stored mask and the rung.
   */
  const detectRegion = (region, page, ctx, detector, padding = 0) => {
    // Detect fits afresh from the text box, never from a padded one.
    if (region.baseBbox) region.bbox = region.baseBbox
    delete region.baseBbox
    delete region.paddingPx
    const mask = cleanRegionAutomatically(region, page, toolContext, ctx)
    region.outcome = 'detected'
    region.detected = true
    region.source = 'auto'
    region.pick = mask.provenance.engine
    region.detector = detector
    // The balloon answer a native detection carries (`DetectedRegion.inside`),
    // read off the stand-in's kind: a sound effect or a hand area is outside.
    region.insideBubble = region.kind === 'bubble'
    if (padding > 0) padDetection(region, page, padding)
    return true
  }

  /**
   * Clean one detection from its stored mask, starting at its `pick`, or at
   * the engine the caller named (`ctx.engine`, the region menu's Clean with).
   * No detection runs. A pick saved as the retired `denoise` rung starts on
   * Fill.
   */
  const cleanDetection = (region, page, ctx = {}) =>
    cleaningDetection(region, page, () => {
      const pick = currentRung(ctx.engine ?? region.pick ?? 'fill')
      cleanRegionAutomatically(region, page, toolContext, { ...ctx, bubbleEngine: pick, outsideEngine: pick })
      delete region.pick
      delete region.detector
      delete region.insideBubble
      return true
    })

  /* ---------- a clean kept, and taken back ---------- */

  /**
   * What a clean of a detection keeps, as the native manifest does. The
   * detection it replaced, by region id (`cleaned_detections`), so the clean
   * can be taken back exactly. And, while a clean is taken back, the result
   * that left the page (`withdrawn_patches`), so it can be put back with no
   * render. A fixture region that is cleaned from the start has neither, as
   * a chapter cleaned by an older build has not.
   */
  /** @type {Map<string, Object>} */
  const keptDetections = new Map()
  /** @type {Map<string, {region: Object, pageId: string}>} */
  const withdrawnResults = new Map()

  /** The Undo tooltip of a clean the backend recorded, and of a reject (`history.rs`). */
  const CLEAN_LABEL = 'masks.command.cleanRegion'
  const REJECT_LABEL = 'masks.command.rejectResult'
  /** The Undo tooltip of a cloud result finished again (`history.rs#FINISHING_LABEL`). */
  const FINISHING_LABEL = 'masks.finishing.command'

  const chapterOfPage = (page) =>
    state.projects.flatMap((project) => project.chapters).find((chapter) => chapter.pages.includes(page)) ?? null

  /** Forget what was kept for regions that are gone for good, by their pages. */
  const dropKeptFor = (pages) => {
    const stems = pages.map((page) => `${page.id}-`)
    for (const map of [keptDetections, withdrawnResults]) {
      for (const id of [...map.keys()]) {
        if (stems.some((stem) => id.startsWith(stem))) map.delete(id)
      }
    }
  }

  /**
   * Write one `clean-state` step into a chapter's journal and say the index
   * moved, as `history.rs#record_step` does. Answers the index after it.
   */
  const recordCleanStep = (chapterId, label, regionId, before, after) => {
    const journal = journalFor(chapterId)
    pushEntry(journal, { label, op: 'clean-state', regionId, before, after }, undoLimitValue())
    emit({ type: 'history-changed', chapterId })
    return snapshot(viewOf(journal, undoLimitValue()))
  }

  /**
   * Write one `region-state` step the backend made itself, as
   * `history.rs#record_region_step` does. Its replay is `restoreRegion`, like
   * any other. Answers the index after it.
   */
  const recordRegionStep = (chapterId, label, regionId, before, after) => {
    const journal = journalFor(chapterId)
    pushEntry(journal, { label, op: 'region-state', regionId, before, after }, undoLimitValue())
    emit({ type: 'history-changed', chapterId })
    return snapshot(viewOf(journal, undoLimitValue()))
  }

  /**
   * A stand-in preview of a result finished one way: the mock keeps no
   * answer, so each method is a tone and a dot screen of its own, enough to
   * tell the tiles apart.
   *
   * @param {number} index
   */
  const finishingPreviewUrl = (index) => {
    const shade = 150 + ((index * 37) % 90)
    const dot = 1 + (index % 4)
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="160" height="120" viewBox="0 0 160 120">`
      + `<defs><pattern id="d" width="8" height="8" patternUnits="userSpaceOnUse"><circle cx="4" cy="4" r="${dot}" fill="rgb(60,60,60)"/></pattern></defs>`
      + `<rect width="160" height="120" fill="rgb(${shade},${shade},${shade})"/>`
      + `<rect x="${20 + index * 3}" y="30" width="80" height="60" rx="10" fill="url(#d)" opacity="${0.3 + (index % 5) * 0.12}"/>`
      + `<ellipse cx="110" cy="62" rx="36" ry="28" fill="white" stroke="black" stroke-width="2"/></svg>`
    return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`
  }

  /**
   * Why a layer's finishing cannot be changed (`refinish.rs#Unavailable`),
   * or null when it can: a cloud result with its answer kept, or a LaMa or
   * FLUX result from this computer, whose model is asked again.
   *
   * @param {any} provenance
   * @returns {string|null}
   */
  const unfinishable = (provenance) => {
    const attemptId = provenance.cloud?.attempt_id
    if (attemptId) return attemptId.startsWith('att-gone') ? 'noResult' : null
    if (provenance.cloud || !['lama', 'flux'].includes(provenance.engine)) return 'notCloud'
    return null
  }

  /**
   * Run `clean` on a region and, when the region was a stored detection and
   * the clean left a result, keep the detection it replaced and record the
   * step, as `Job::complete_detection` and `history.rs` do for every cleaner
   * of a detection: a run, a region edit, a cloud render. A clean of anything
   * else keeps and records nothing.
   *
   * The page's status in the entry is what the page settles as, never the
   * `cleaning` a run shows while it is on the page.
   *
   * @template T
   * @param {any} region
   * @param {any} page
   * @param {() => T} clean
   * @returns {T}
   */
  const cleaningDetection = (region, page, clean) => {
    if (!isDetection(region)) return clean()
    const detected = snapshot(region)
    const before = { present: true, pageStatus: settledStatus(page), region: detected }
    // A detection's mask is the detector's, whoever asked for it cleaned
    // (`region.rs`), so the result is not a hand mask.
    region.source = 'auto'
    const answer = clean()
    if (isDetection(region) || !region.mask) {
      region.source = detected.source
      return answer
    }
    keptDetections.set(region.id, detected)
    // A result set aside for this region is replaced by the new one.
    withdrawnResults.delete(region.id)
    const chapter = chapterOfPage(page)
    if (chapter) {
      recordCleanStep(chapter.id, CLEAN_LABEL, region.id, before,
        { present: true, pageStatus: settledStatus(page), region: snapshot(region) })
    }
    return answer
  }

  /**
   * A region edit that is not Clean itself (a hand tool, a re-run, Clean
   * anyway) on a region that may be a stored detection. When it is one and
   * the edit leaves a result, the detection is kept and the step recorded as
   * for any clean of one, and the page settles: `region.rs` commits every
   * edit of a detection's id through the same replacement.
   *
   * @template T
   * @param {{region: any, page: any}} found
   * @param {() => T} edit
   * @returns {T}
   */
  const cleaningHere = (found, edit) => {
    if (!isDetection(found.region)) return edit()
    const answer = cleaningDetection(found.region, found.page, () => {
      const made = edit()
      if (!isDetection(found.region) && found.region.mask) {
        delete found.region.pick
        delete found.region.detector
        delete found.region.insideBubble
      }
      return made
    })
    if (!isDetection(found.region) && found.page.status === 'detected') found.page.status = settledStatus(found.page)
    return answer
  }

  /**
   * A detection made from a result's own mask, for a result whose clean kept
   * nothing, as `Job::detection_of_patch` makes one: only a cleaner's result
   * qualifies, never a paint or clone stroke or a region copy. A Fill result
   * starts on Fill inside a balloon, anything else on LaMa outside.
   *
   * @param {any} region
   * @returns {any|null}
   */
  const detectionOfResult = (region) => {
    const engine = region.mask?.provenance?.engine
    if (!engine || engine === 'paint' || engine === 'clone' || region.mask?.provenance?.params_snapshot?.mode === 'region-copy') return null
    const fill = currentRung(engine) === 'fill'
    const restored = snapshot(region)
    delete restored.paddingPx
    delete restored.baseBbox
    return { ...restored, outcome: 'detected', detected: true, pick: fill ? 'fill' : 'lama', detector: 'restored', insideBubble: fill }
  }

  /**
   * Take a clean back or put its result back, as `Job::restore_detection` and
   * `Job::restore_patch` do, with their refusals in their order. Answers the
   * region's place after it, or `null` when no chapter names the region.
   * Nothing renders: the result that returns is the one that left.
   *
   * @param {string} regionId
   * @param {boolean} cleaned
   * @returns {{region: any, page: any, chapter: any}|null}
   */
  const moveRegionCleaned = (regionId, cleaned) => {
    const found = findRegion(regionId)
    if (cleaned) {
      const saved = withdrawnResults.get(regionId)
      if (!saved) {
        if (!found) return null
        // On the page already. Redo may ask twice.
        if (found.region.mask && !isDetection(found.region)) return found
        throw new Error(`clean_no_result: no result is saved for region ${regionId} to put back`)
      }
      // A detection deleted in between is not needed: the result returns
      // to its page without it.
      const page = found?.page ?? findPage(saved.pageId)
      const chapter = page ? chapterOfPage(page) : null
      if (!page || !chapter) return null
      refuseBusyPage(chapter.id, page.index)
      // The detection as it stands now is the one kept, as a clean keeps it.
      if (found && isDetection(found.region)) keptDetections.set(regionId, snapshot(found.region))
      const region = snapshot(saved.region)
      const at = page.regions.findIndex((candidate) => candidate.id === regionId)
      if (at >= 0) page.regions[at] = region
      else page.regions.push(region)
      withdrawnResults.delete(regionId)
      page.status = settledStatus(page)
      return { region, page, chapter }
    }
    if (!found) return null
    // It is a detection already. Undo may ask twice.
    if (isDetection(found.region)) return found
    if (!found.region.mask) {
      throw new Error(`clean_no_patch: region ${regionId} has no cleaned result to take back`)
    }
    const kept = keptDetections.get(regionId) ?? detectionOfResult(found.region)
    if (!kept) {
      throw new Error(`clean_not_restorable: no detection was kept for region ${regionId}; its mask cannot be put back`)
    }
    refuseBusyPage(found.chapter.id, found.page.index)
    withdrawnResults.set(regionId, { region: snapshot(found.region), pageId: found.page.id })
    keptDetections.delete(regionId)
    const region = snapshot(kept)
    found.page.regions[found.page.regions.indexOf(found.region)] = region
    found.page.status = settledStatus(found.page)
    return { region, page: found.page, chapter: found.chapter }
  }

  // Several runs at once, one per chapter and three in all, as `run.rs`
  // keeps them. Each run's step rides with it, for the notice it finishes with.
  /** A Solid bubbles only run's count of regions it left detected, by run id. */
  const solidKept = new Map()
  /**
   * The chapters whose cancelled run was a Solid bubbles only one, so a resume
   * cleans under the same rule (`run.rs` keeps it in the run's snapshot).
   *
   * @type {Set<string>}
   */
  const solidInterrupted = new Set()

  /**
   * Refuse an edit of the page a run is on right now, as the native side does
   * (`page_busy`). Every other page of the chapter takes edits while the run
   * goes on.
   *
   * @param {string} chapterId
   * @param {number} pageIndex
   */
  const refuseBusyPage = (chapterId, pageIndex) => {
    if (runner.onPage(chapterId, pageIndex)) throw new Error('page_busy')
  }

  const runner = createRunnerPool({
    emit,
    timers,
    timing: { region: timing.region, pageTail: timing.pageTail },
    cleanRegion: (region, page, ctx) => cleanRegionAutomatically(region, page, toolContext, ctx),
    onFinished: (summary) => {
      const found = findChapter(summary.chapterId)
      const kept = solidKept.get(summary.runId)
      solidKept.delete(summary.runId)
      if (summary.reason === 'cancelled') {
        if (found) {
          found.project.interruptedJob = {
            chapterId: summary.chapterId,
            pageIndex: summary.nextPageIndex,
          }
          if (kept) solidInterrupted.add(summary.chapterId)
          else solidInterrupted.delete(summary.chapterId)
        }
        notify('notice.run.cancelled', {}, 'warn')
        return
      }
      if (found) found.project.interruptedJob = null
      solidInterrupted.delete(summary.chapterId)
      if (kept) {
        // Solid bubbles only says both counts, as `run.rs` does: a run that
        // cleaned nothing still found the regions it left.
        notify('notice.run.solidOnly', { cleaned: summary.regionsCleaned, kept: kept() })
      } else if (summary.mode === 'detect' && summary.regionsCleaned > 0) {
        // Detect's own closing word, as `run.rs` sends it: `finished` would
        // say regions were cleaned.
        notify('notice.run.detected', { regions: summary.regionsCleaned, pages: summary.pagesCleaned })
      } else if (summary.regionsCleaned === 0) {
        notify('notice.chapter.emptyResult', { regions: 0, pages: summary.pagesCleaned }, 'warn')
      } else {
        notify('notice.run.finished', {
          pages: summary.pagesCleaned,
          regions: summary.regionsCleaned,
        })
      }
    },
  })

  // A page is runnable when something on it is queueable - and with the
  // outside-bubble opt-in on, a region held back as "text outside a speech
  // bubble" is queueable too (`isOutsideHeld`).
  const runnable = (page, outsideBubbles) =>
    page.status !== 'skipped' &&
    (page.status === 'unclean' ||
      page.regions.some(
        (region) => isQueueable(region) || (outsideBubbles === 'clean' && isOutsideHeld(region)),
      ))

  // Clean takes only the pages with detections; Detect and clean takes those
  // too, so a stored detection is cleaned, never lost. Detect leaves a page it
  // already detected unless the page is asked for by name (`runnable` in
  // `run.rs`), so a stopped Detect resumes where it stopped.
  const inStep = (page, outsideBubbles, mode, byName) =>
    mode === 'clean' ? hasDetections(page)
      : mode === 'detect' && !byName && hasDetections(page) ? false
        : hasDetections(page) || runnable(page, outsideBubbles)

  const queueFor = ({ scope, chapterId, pageIndex, outsideBubbles, mode = 'auto' }) => {
    const found = findChapter(chapterId)
    if (!found) return []
    const chapters =
      scope === 'project' ? found.project.chapters : [found.chapter]
    const entries = []
    for (const chapter of chapters) {
      const pages =
        scope === 'page'
          ? chapter.pages.filter((page) => page.index === pageIndex)
          : chapter.pages
      for (const page of pages) {
        if (inStep(page, outsideBubbles, mode, scope === 'page')) {
          entries.push({ projectId: found.project.id, chapterId: chapter.id, page })
        }
      }
    }
    return entries
  }

  /**
   * What a real run holds in memory, as the loaded-models tab would see it.
   *
   * The sizes are this machine's actual weights and the measured
   * 510 MB for rung 2, so the tab's layout is exercised against the numbers it
   * will really be given rather than against round ones. The mock has no
   * sessions, so what stands in for "loaded" is "a run is in progress" - which
   * is when the real backend has these open too.
   */
  const MOCK_MODELS = Object.freeze([
    { id: 1, kindKey: 'models.kind.textDetector', bytes: 94_669_756, basis: 'weights', deviceKey: 'accel.coreml', gpu: true },
    { id: 2, kindKey: 'models.kind.balloonDetector', bytes: 11_120_765, basis: 'weights', deviceKey: 'accel.cpu', gpu: false },
    { id: 3, kindKey: 'models.kind.scriptGate', bytes: 3_722_314, basis: 'weights', deviceKey: 'accel.cpu', gpu: false },
    { id: 4, kindKey: 'models.kind.inpainter', bytes: 534_773_760, basis: 'measured', deviceKey: 'accel.webgpu', gpu: true },
  ])

  /** Ids the caller has asked back. Reset when a run starts, because a run reloads what it needs. */
  const unloadedModels = new Set()

  const startRun = ({
    scope,
    chapterId,
    pageIndex,
    engineCeiling,
    bubbleEngine,
    outsideEngine,
    outsideBubbles,
    bubbleColor,
    maskPaddingPx = 0,
    detection,
    detectorModels,
    geometryPolicy,
    textPolicy,
    ocrRescue,
    mode = 'auto',
    detector = 'local',
    solidBubblesOnly = false,
    area = null,
    cloudClean = null,
  }) => {
    // One run per chapter, and a cap on all of them, as `run.rs` answers: the
    // chapter's own run back, or no run at all when the cap is reached.
    const own = runner.runFor(chapterId, { allowAreas: !!area })
    if (own) return { runId: own, pages: [], alreadyRunning: true }
    if (runner.atCapacity({ cloudArea: !!area && detector === 'cloud' })) return { runId: null, pages: [], alreadyRunning: true, atCapacity: true }
    unloadedModels.clear()
    // Solid bubbles only is the clean's choice, and it stores text outside a
    // bubble as a detection where the run would have held it (`run.rs`).
    const solidOnly = solidBubblesOnly === true && mode !== 'detect'
    if (solidOnly) outsideBubbles = 'clean'
    const queue = queueFor({ scope, chapterId, pageIndex, outsideBubbles, mode })
    if (queue.length === 0) {
      notify(mode === 'clean' ? 'notice.run.nothingDetected' : 'notice.run.nothingInScope', {}, 'warn')
      return { runId: null, pages: [] }
    }
    // As `run.rs#reader_gap`: a run that asked for the text reader and cannot
    // have it says so before it starts, rather than detecting without it
    // quietly. A clean reads nothing, so it says nothing.
    const readerFiles = ['hayaiVision', 'hayaiDecoder', 'hayaiTokenizer']
    if (mode !== 'clean' && ocrRescue === true && readerFiles.some((id) => state.missingModels.has(id))) {
      const missing = MOCK_CATALOGUE.filter((model) => readerFiles.includes(model.id) && state.missingModels.has(model.id))
        .map((model) => model.fileName)
      notify('notice.run.ocrRescueUnavailable', { reason: `Hayai OCR is not installed: missing ${missing.join(', ')}` }, 'warn')
    }
    const optedIn = (region) => outsideBubbles === 'clean' && isOutsideHeld(region)
    const fresh = (page) => page.regions.filter((region) => isQueueable(region) || optedIn(region))
    // Each step names its regions and what it does to them (runner.js). Detect
    // redoes a page's detections with its new ones; Detect and clean cleans a
    // page's stored detections rather than detecting it again.
    // A Detect run's pages are one undo step, each page taken back to what
    // it held before the run reached it (`run.rs`, `history::record_pages`).
    const detectGroup = `mock-detect-${++detectRuns}`
    const beforeDetect = new Map()
    const steps = {
      detect: {
        pick: (page) => {
          beforeDetect.set(page.id, capturePages([page]))
          return [...detectionsOf(page), ...fresh(page)]
        },
        apply: (region, page, ctx) => detectRegion(region, page, ctx, detector, maskPaddingPx),
        pageStatus: (page) => {
          const status = settledStatus(page)
          const captured = beforeDetect.get(page.id)
          beforeDetect.delete(page.id)
          const chapter = chapterOfPage(page)
          if (captured && chapter) recordPageStep(chapter.id, DETECTION_LABELS.detect, captured, detectGroup)
          return status
        },
        // As `run.rs`: a detection arrives with its page, never as a region done.
        reportRegions: false,
      },
      clean: { pick: detectionsOf, apply: cleanDetection, pageStatus: settledStatus },
      auto: {
        pick: (page) => (hasDetections(page) ? detectionsOf(page) : fresh(page)),
        apply: (region, page, ctx) =>
          isDetection(region) ? cleanDetection(region, page, ctx) : (cleanRegionAutomatically(region, page, toolContext, ctx), true),
      },
    }
    // Solid bubbles only, as `run.rs` runs it: a speech bubble is painted flat
    // and every other region is left, or stored, as a detection for a later
    // clean. The mock has no paper to measure, so a bubble is the stand-in's
    // kind, as `detectRegion` reads it.
    let kept = 0
    if (solidOnly) {
      const solid = {
        apply: (region, page, ctx) => {
          if (!isDetection(region)) detectRegion(region, page, ctx, detector, maskPaddingPx)
          if (!(region.insideBubble ?? region.kind === 'bubble')) {
            kept += 1
            return false
          }
          return cleanDetection(region, page, { ...ctx, engine: ctx.bubbleEngine === 'solid' ? 'solid' : 'fill' })
        },
        pageStatus: settledStatus,
      }
      steps.clean = { ...steps.clean, ...solid }
      steps.auto = { ...steps.auto, ...solid }
    }
    // A streaming cloud clean (`cloud_clean.rs#run_stream`): each region is
    // detected, then rendered on the cloud GPU while the estimate of what was
    // sent stays within the cap. The region that would pass it, and every one
    // after, stays detected, said once.
    if (cloudClean) {
      let spent = 0
      let capped = false
      steps.auto = {
        ...steps.auto,
        apply: (region, page, ctx) => {
          if (!isDetection(region)) detectRegion(region, page, ctx, detector, maskPaddingPx)
          const next = spent + (spent === 0 ? MOCK_STREAM_FIXED_USD : 0) + MOCK_STREAM_REGION_USD
          if (!capped && next > cloudClean.capUsd) {
            capped = true
            notify('notice.cloudClean.capReached', { page: page.index + 1, cap: cloudClean.capUsd }, 'warn')
          }
          if (capped) return false
          spent = next
          commitCloudResult({ region, page }, cloudRecordFor(cloudAttemptId(`${cloudClean.grantId}:${region.id}`), cloudClean.target))
          delete region.pick
          delete region.detector
          delete region.insideBubble
          return true
        },
        pageStatus: settledStatus,
      }
    }
    const handle = runner.start(queue, {
      ...(steps[mode] ?? steps.auto),
      chapterId,
      mode,
      kind: mode === 'detect' ? 'detect' : 'clean',
      area,
      detector,
      engineCeiling: engineCeiling ?? state.settings.engineCeiling,
      bubbleEngine,
      outsideEngine,
      outsideBubbles,
      bubbleColor,
      detection,
      detectorModels,
      geometryPolicy,
      textPolicy,
      ocrRescue,
    })
    if (solidOnly && handle.runId) solidKept.set(handle.runId, () => kept)
    return handle
  }

  const snapshot = (value) => structuredClone(value)

  /**
   * Project a public inference configuration DTO, ensuring no unknown fields
   * (especially secrets, tokens, or credential keys) are retained in mock memory.
   *
   * @param {Object} raw
   * @returns {import('./backend.js').InferenceConfig}
   */
  const projectPublicInferenceConfig = (raw) => {
    if (!raw || typeof raw !== 'object') {
      throw new Error('invalid inference configuration: expected an object')
    }

    const allowedTopKeys = new Set([
      'schemaVersion',
      'selectedTarget',
      'beamProfiles',
      'modalProfiles',
    ])
    for (const key of Object.keys(raw)) {
      if (!allowedTopKeys.has(key)) {
        throw new Error(`unrecognized inference config field '${key}'`)
      }
    }

    const schemaVersion = raw.schemaVersion ?? 1
    if (typeof schemaVersion !== 'number') {
      throw new Error('invalid schemaVersion: expected number')
    }

    const rawTarget = raw.selectedTarget ?? { type: 'local' }
    if (!rawTarget || typeof rawTarget !== 'object' || typeof rawTarget.type !== 'string') {
      throw new Error('invalid selectedTarget: expected object with type')
    }

    let selectedTarget
    if (rawTarget.type === 'local') {
      for (const key of Object.keys(rawTarget)) {
        if (key !== 'type') {
          throw new Error(`unrecognized field '${key}' in local execution target`)
        }
      }
      selectedTarget = { type: 'local' }
    } else if (rawTarget.type === 'beam' || rawTarget.type === 'modal') {
      for (const key of Object.keys(rawTarget)) {
        if (key !== 'type' && key !== 'profile_id') {
          throw new Error(`unrecognized field '${key}' in ${rawTarget.type} execution target`)
        }
      }
      if (typeof rawTarget.profile_id !== 'string') {
        throw new Error(`missing or invalid profile_id in ${rawTarget.type} execution target`)
      }
      selectedTarget = { type: rawTarget.type, profile_id: rawTarget.profile_id }
    } else {
      throw new Error(`unknown execution target type '${rawTarget.type}'`)
    }

    const projectProfileMap = (rawMap, provider) => {
      const projected = Object.create(null)
      if (!rawMap || typeof rawMap !== 'object') return projected
      const allowedProfileKeys = new Set([
        'id',
        'name',
        'endpointUrl',
        'canonicalOrigin',
        'canonicalOriginFingerprint',
        'createdAtMs',
        'updatedAtMs',
      ])
      for (const [id, profile] of Object.entries(rawMap)) {
        if (!profile || typeof profile !== 'object') {
          throw new Error(`invalid ${provider} profile '${id}'`)
        }
        for (const key of Object.keys(profile)) {
          if (!allowedProfileKeys.has(key)) {
            throw new Error(`unrecognized field '${key}' in ${provider} profile '${id}'`)
          }
        }
        projected[id] = {
          id: String(profile.id ?? id),
          name: String(profile.name ?? id),
          endpointUrl: String(profile.endpointUrl ?? ''),
          canonicalOrigin: String(profile.canonicalOrigin ?? ''),
          canonicalOriginFingerprint: String(profile.canonicalOriginFingerprint ?? ''),
          createdAtMs: Number(profile.createdAtMs ?? 0),
          updatedAtMs: Number(profile.updatedAtMs ?? 0),
        }
      }
      return projected
    }

    return {
      schemaVersion,
      selectedTarget,
      beamProfiles: projectProfileMap(raw.beamProfiles, 'beam'),
      modalProfiles: projectProfileMap(raw.modalProfiles, 'modal'),
    }
  }

  /**
   * A settings snapshot with the Hugging Face token taken out.
   *
   * The seam says the token never travels in this direction, and the mock
   * keeps it in the same object every other preference lives in - so the rule is enforced here
   * rather than left to fall out of where the value happens to be stored. The
   * native adapter strips it at the same boundary and for the same reason: on
   * a machine with no credential store, `settings.json` really is holding one.
   *
   * @param {Object} settings
   */
  const withoutToken = (settings) => {
    const { hfToken: _hfToken, ...rest } = settings
    return rest
  }

  /* ---------- residency ---------- */

  /*
   * The mock owns a fixture library and holds all of it, which a real backend
   * never does. What it must still do is *answer the way the real one answers*,
   * or the interface's paging is exercised by nothing: a chapter crosses the
   * seam as page **headers** plus a chapter-wide review index, and the regions
   * of a page arrive only when `loadPages` is asked for them.
   *
   * Without this the window in `src/lib/state/pagewindow.svelte.js` would be
   * dead code under test - every page would already be resident - and the first
   * time anyone ran the built application against the Tauri adapter, the Layers
   * panel would be empty for every page.
   */

  /**
   * The three numbers a header answers with: how many regions the page has,
   * how many are finished, how many need review. A real backend reads them out
   * of the manifest; the Pages list draws all three for every page of the
   * chapter, so a header without them is a row that says `0 / 0`.
   */
  const countsOf = (regions) => ({
    regionCount: regions.length,
    // `library.rs#count_page`: finished is a cleaned layer with nothing
    // flagged, which a detection waiting on its mask is not.
    doneCount: regions.filter((region) => regionState(region) === 'cleaned').length,
    reviewCount: regions.filter((region) => reviewReason(region) !== null).length,
    candidateCount: regions.filter((region) => regionState(region) === 'candidate').length,
  })

  /** One page, with its regions withheld. */
  const pageHeader = (page) => {
    const { regions, ...header } = page
    return { ...header, ...countsOf(regions), regions: [], resident: false }
  }

  /** One page, paged in. */
  const residentPage = (page) => ({
    ...page,
    ...countsOf(page.regions),
    resident: true,
  })

  /** The chapter's review set, in page order - `ReviewRef` on the seam. */
  const reviewIndexOf = (chapter) => {
    const refs = []
    for (const page of chapter.pages) {
      for (const region of page.regions) {
        const reasonKey = reviewReason(region)
        if (reasonKey) {
          refs.push({ id: region.id, pageId: page.id, pageIndex: page.index, reasonKey })
        }
      }
    }
    return refs
  }

  /** A chapter as it crosses the seam: headers, a review index, no regions. */
  const chapterHeaders = (chapter) => ({
    ...chapter,
    pages: chapter.pages.map(pageHeader),
    review: reviewIndexOf(chapter),
    denoiseReplacement: denoiseReplacementOf(chapter),
    denoiseHistory: denoiseHistoryOf(chapter),
  })

  /** A project, with every chapter reduced the same way. */
  const projectHeaders = (project) => ({
    ...project,
    chapters: project.chapters.map(chapterHeaders),
  })

  /* ---------- the undo journal ---------- */

  /**
   * One journal per chapter. In a real backend this is
   * `<job>.mtclean.d/history.json`; here it is a Map, and the semantics are the
   * same three rules `src/lib/model/journal.js` states, because both sides call
   * the same functions.
   */
  /** @type {Map<string, import('../model/journal.js').Journal>} */
  const journals = new Map()
  const journalFor = (chapterId) => {
    let journal = journals.get(chapterId)
    if (!journal) {
      journal = createJournal()
      journals.set(chapterId, journal)
    }
    return journal
  }

  /*
   * `detection-state` steps, as `history.rs#record_pages` writes them: an edit
   * of a page's detections and held rows, kept as the page's whole state
   * before and after. The native side keeps each state once, by digest, in
   * the job's sidecar; the mock keeps the copies in the entry, which is its
   * memory either way.
   */
  const DETECTION_LABELS = {
    delete: 'masks.command.deleteDetection',
    type: 'masks.command.detectionType',
    edit: 'masks.command.editDetection',
    padding: 'masks.command.detectionPadding',
    approve: 'masks.command.approveMask',
    dismiss: 'masks.command.deleteRegion',
    detect: 'masks.command.detectPages',
  }
  /** The rows a page state holds: every detection, and every row with no mask. */
  const inPageState = (region) => isDetection(region) || !region.mask
  const pageStateOf = (page) => snapshot(page.regions.filter(inPageState))
  const samePageState = (a, b) => JSON.stringify(a) === JSON.stringify(b)
  const putPageState = (page, regions) => {
    page.regions = [...page.regions.filter((region) => !inPageState(region)), ...structuredClone(regions)]
    page.status = settledStatus(page)
  }
  /** @type {(pages: any[]) => Array<{page: any, before: any}>} */
  const capturePages = (pages) => pages.map((page) => ({ page, before: pageStateOf(page) }))
  /**
   * Record the pages `capturePages` took before an edit as one step; a page
   * the edit left as it was is not part of it, and an edit that changed none
   * records nothing. With a `group`, a run's later pages join its entry.
   */
  const recordPageStep = (chapterId, label, captured, group = null) => {
    const pages = captured
      .map(({ page, before }) => ({ pageIndex: page.index, pageId: page.id, before, after: pageStateOf(page) }))
      .filter((step) => !samePageState(step.before, step.after))
    if (pages.length === 0) return
    const journal = journalFor(chapterId)
    const last = journal.entries.at(-1)
    if (group && last?.group === group && journal.cursor === journal.entries.length) {
      for (const step of pages) {
        const held = last.pages.find((page) => page.pageId === step.pageId)
        if (held) held.after = step.after
        else last.pages.push(step)
      }
    } else {
      pushEntry(journal, { label, op: 'detection-state', regionId: '', before: { present: false }, after: { present: false }, pages, group }, undoLimitValue())
    }
    emit({ type: 'history-changed', chapterId })
  }
  /**
   * Replay a `detection-state` entry, as `history.rs#replay_pages` does:
   * every page is checked first, and a page not in the state the entry left
   * it in writes nothing at all (`history_stale`).
   */
  const replayPageStep = (entry, undo) => {
    const targets = []
    for (const step of entry.pages) {
      const page = findPage(step.pageId)
      if (!page) return { pages: [], refused: 'history_stale' }
      const [from, to] = undo ? [step.after, step.before] : [step.before, step.after]
      const now = pageStateOf(page)
      if (samePageState(now, to)) continue
      if (!samePageState(now, from)) return { pages: [], refused: 'history_stale' }
      targets.push([page, to])
    }
    for (const [page, to] of targets) putPageState(page, to)
    return { pages: entry.pages.map((step) => step.pageIndex) }
  }

  /* ---------- the model catalogue ---------- */

  /**
   * The weights `src-tauri/src/weights.rs` pins, including each exact digest,
   * with the real sizes and kind keys. This lets the UI show the same artifact
   * identity as native without making up model revisions for mutable URLs.
   */
  const MOCK_CATALOGUE = Object.freeze([
    { id: 'textDetector', fileName: 'comictextdetector.onnx', bytes: 94_669_756, sha256: '1a86ace74961413cbd650002e7bb4dcec4980ffa21b2f19b86933372071d718f', kindKey: 'models.kind.textDetector', requiredBy: ['autoClean'] },
    { id: 'inpainter', fileName: 'lama-manga.onnx', bytes: 207_482_644, sha256: '4512adab295ee5a5e02ccd1bdf8d45dccbac88309d9cff1532ffd5de876f02a4', kindKey: 'models.kind.inpainter', requiredBy: ['lama'] },
    { id: 'scriptGate', fileName: 'image-script-identification-osd_lstm.onnx', bytes: 3_722_314, sha256: 'b18e0c1479d9eb67394993098f7e1079c9a93ef6f7b0416ee333fccb865c6e72', kindKey: 'models.kind.scriptGate', requiredBy: ['autoClean'] },
    { id: 'scriptGateLabels', fileName: 'image-script-identification-osd_labels.json', bytes: 1_163, sha256: 'a1888156b005065039c356e13a7bbef1ec454b45bf6aaf18c11f4a59b1ee35c5', kindKey: 'models.kind.scriptGateLabels', requiredBy: ['autoClean'] },
    { id: 'balloonDetector', fileName: 'comic-text-and-bubble-detector-detector-v4-s_int8.onnx', bytes: 11_120_765, sha256: '5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79', kindKey: 'models.kind.balloonDetector', requiredBy: ['autoClean'] },
    { id: 'fullRt', fileName: 'detector.onnx', bytes: 168_481_531, sha256: '065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195', kindKey: 'models.kind.fullRt', requiredBy: ['review'] },
    // The gate's rescue reader. `requiredBy` is empty in the real table too,
    // and that is the row's whole character: it is downloadable and nothing
    // needs it, so it belongs in Settings and not in the first-launch offer.
    // Mirrored here so the Models section is exercised against a catalogue
    // that has such a row in it rather than against one where every row is a
    // precondition of something.
    { id: 'ocrEncoder', fileName: 'manga-ocr-encoder_model.onnx', bytes: 343_454_249, sha256: '15fa8155fe9bc1a7d25d9bb353debaa4def033d0174e907dbd2dd6d995def85f', kindKey: 'models.kind.ocr', requiredBy: [] },
    { id: 'ocrDecoder', fileName: 'manga-ocr-decoder_model.onnx', bytes: 117_480_262, sha256: 'ef7765261e9d1cdc34d89356986c2bbc2a082897f753a89605ae80fdfa61f5e8', kindKey: 'models.kind.ocrDecoder', requiredBy: [] },
    { id: 'ocrVocab', fileName: 'manga-ocr-vocab.txt', bytes: 30_216, sha256: '5cb5c5586d98a2f331d9f8828e4586479b0611bfba5d8c3b6dadffc84d6a36a3', kindKey: 'models.kind.ocrVocab', requiredBy: [] },
    // The text reader (Hayai OCR), optional like the rescue above.
    { id: 'hayaiVision', fileName: 'hayai-ocr-vision.onnx', bytes: 343_538_300, sha256: '379ec20e7d5b134e6bd0e7c5cf0a4e705318129bfb710e25e69c6ca027b84021', kindKey: 'models.kind.hayaiVision', requiredBy: [] },
    { id: 'hayaiDecoder', fileName: 'hayai-ocr-decoder.onnx', bytes: 255_717_567, sha256: '23342ad16efee65486347b7ac15c98d5f78412eb4c4bee3236d03d8478cb0e20', kindKey: 'models.kind.hayaiDecoder', requiredBy: [] },
    { id: 'hayaiTokenizer', fileName: 'hayai-ocr-tokenizer.json', bytes: 1_247_253, sha256: 'f8a0a909c628a684fe463094614e236a8b1d3609e7770f77e7beafaf1056bf13', kindKey: 'models.kind.hayaiTokenizer', requiredBy: [] },
    // The local page denoise package (`waifu2x-scan-4x-n2`): the model and
    // its seam blending graph, the two rows `weights.rs` pins, one group
    // (`pageDenoise`). Optional, so not installed on the fixture machine.
    { id: 'pageDenoiseModel', fileName: 'waifu2x--swin_unet--art_scan--noise2_scale4x.onnx', bytes: 18_905_524, sha256: '532424408a1fd293c6fbfd54b44cd9077b7a4da8452218fd4d8040c7c9c78747', kindKey: 'models.kind.pageDenoise', requiredBy: ['pageDenoise'] },
    { id: 'pageDenoiseSeams', fileName: 'waifu2x--utils--create_seam_blending_filter.onnx', bytes: 26_530, sha256: '7d825bc0bbba65493cd3e1809a1cd6db4999243d516154b7974522dce6826ad5', kindKey: 'models.kind.pageDenoiseSeams', requiredBy: ['pageDenoise'] },
  ])
  const MOCK_MODEL_GROUPS = Object.freeze({
    scriptGate: ['scriptGate', 'scriptGateLabels'],
    mangaOcr: ['ocrEncoder', 'ocrDecoder', 'ocrVocab'],
    hayaiOcr: ['hayaiVision', 'hayaiDecoder', 'hayaiTokenizer'],
    pageDenoise: ['pageDenoiseModel', 'pageDenoiseSeams'],
  })
  const activeModelGroups = new Set()

  /**
   * The published size of the macOS arm64 archive, from
   * `crates/cleaner-core/src/runtime/package.rs`. The real number for the same
   * reason the catalogue's sizes are real: a second table that invented
   * plausible ones would be a table to disagree with the first.
   */
  const MOCK_RUNTIME_BYTES = 32_396_562

  /**
   * The chosen build, as the runtime row reports it. Whatever `runtimeFlavour`
   * says, the answer here is the one flavour this platform publishes - which is
   * exactly what `package::for_host_flavour` does with an id it does not
   * recognise.
   */
  function runtimeFlavour() {
    if (MOCK_MACHINE === 'windows-nvidia') {
      const chosen = MOCK_WINDOWS_FLAVOURS.find((row) => row.id === state.settings.runtimeFlavour) ?? MOCK_WINDOWS_FLAVOURS[0]
      return { version: chosen.ortVersion, flavour: chosen.id, bytes: chosen.bytes }
    }
    return { version: '1.28.0', flavour: 'stock', bytes: MOCK_RUNTIME_BYTES }
  }

  const MOCK_MODELS_DIR = '/Users/you/Library/Application Support/manga-cleaner/models'
  const MOCK_RUNTIME_DIR = '/Users/you/Library/Application Support/manga-cleaner/runtimes'
  const MOCK_RUNTIME_ID = 'runtime'

  // FIXTURE KNOB: `?machine=windows-nvidia` is a Windows PC with an NVIDIA
  // card and two CUDA toolkits, 12.6 and 13.0, but cuDNN for 13.0 only - the
  // side-by-side case the Runtime screen exists to explain. The CUDA 12 build
  // then reads "no cuDNN for CUDA 12" and the CUDA 13 build is recommended.
  // Without it the machine is the Apple one `listModels` reports, which has no
  // CUDA at all. Inert inside a Tauri window, which asks the machine.
  const MOCK_MACHINE = new URLSearchParams(globalThis.location?.search ?? '').get('machine') === 'windows-nvidia'
    ? 'windows-nvidia'
    : 'mac'

  /** The Windows builds, with the real archive sizes from `runtime/package.rs`. */
  const MOCK_WINDOWS_FLAVOURS = [
    { id: 'directml', ortVersion: '1.28.0', bytes: 215_000_000, isDefault: true, userInstalled: [] },
    { id: 'cuda12', ortVersion: '1.28.0', bytes: 455_000_000, isDefault: false, userInstalled: ['CUDA 12.8+', 'cuDNN 9.x'] },
    { id: 'cuda13', ortVersion: '1.28.0', bytes: 366_000_000, isDefault: false, userInstalled: ['CUDA 13.x', 'cuDNN 9.x'] },
  ]

  /**
   * `runtimeReport`'s answer for a wanted CUDA major, worked out the way
   * `cuda::Report::verdict` does: an adapter, a driver new enough, a complete
   * toolkit of that major, a cuDNN that names it or names none.
   *
   * @param {number|null} wanted
   */
  function mockCudaReport(wanted) {
    const nvidia = MOCK_MACHINE === 'windows-nvidia'
    const gpus = nvidia ? [{ name: 'NVIDIA GeForce RTX 4070', driver: '581.42' }] : []
    const toolkits = nvidia
      ? [
          { major: 13, minor: 0, dir: 'C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v13.0\\bin\\x64', missing: [] },
          // Older than the CUDA 12 build's floor (12.8), so that build reads as
          // too old here.
          { major: 12, minor: 6, dir: 'C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA\\v12.6\\bin', missing: [] },
        ]
      : []
    const cudnn = nvidia ? [{ cudaMajor: 13, dir: 'C:\\Program Files\\NVIDIA\\CUDNN\\v9.14\\bin\\13.0\\x64', missing: [] }] : []
    // `cuda::minor_floor`.
    const MINOR_FLOOR = { 12: 8 }
    const newEnough = (toolkit) => MINOR_FLOOR[toolkit.major] === undefined || toolkit.minor === null || toolkit.minor >= MINOR_FLOOR[toolkit.major]
    const verdict = (major) => {
      if (!gpus.length) return 'accel.cudaCheck.noGpu'
      const complete = toolkits.filter((toolkit) => toolkit.major === major && !toolkit.missing.length)
      if (!complete.length) return 'accel.cudaCheck.noToolkit'
      if (!complete.some(newEnough)) return 'accel.cudaCheck.toolkitTooOld'
      if (!cudnn.some((install) => !install.missing.length && (install.cudaMajor === null || install.cudaMajor === major))) return 'accel.cudaCheck.noCudnn'
      return 'accel.cudaCheck.ready'
    }
    const verdictKey = wanted === null ? null : verdict(wanted)
    const tooOld = verdictKey === 'accel.cudaCheck.toolkitTooOld'
    const newest = toolkits.find((toolkit) => toolkit.major === wanted)
    return {
      wanted,
      toolkits,
      cudnn,
      gpus,
      verdictKey,
      verdictHave: tooOld ? `${newest.major}.${newest.minor}` : null,
      verdictNeed: tooOld ? `${wanted}.${MINOR_FLOOR[wanted]}` : null,
      recommended: [['cuda13', 13], ['cuda12', 12]].find(([, major]) => verdict(major) === 'accel.cudaCheck.ready')?.[0] ?? null,
    }
  }

  /**
   * The six rows `models.rs` places, with the backends each can take on the
   * Windows machine `listAccelerators` reports. `table` is where Automatic
   * lands without a measurement, and `timings` is what a measurement here
   * finds: `[backend, run ms, load ms]`, or `[backend, null, null, reason]`
   * for one that fails.
   */
  const MOCK_PLACED_MODELS = [
    { id: 'ctd', modelName: 'Comic Text Detector (CTD)', modelKey: 'models.kind.textDetector', supported: ['cpu', 'directml', 'cuda', 'webgpu'], table: 'directml',
      timings: [['cpu', 412, 90], ['directml', 118, 1450], ['webgpu', 171, 820]] },
    { id: 'rtSmall', modelName: 'Ogkalu comic text & bubble detector (Small)', modelKey: 'models.kind.balloonDetector', supported: ['cpu', 'directml', 'cuda'], table: 'cpu',
      timings: [['cpu', 81, 85], ['directml', 64, 1210]] },
    { id: 'rtFull', modelName: 'Ogkalu comic text & bubble detector (Full)', modelKey: 'models.kind.fullRt', supported: ['cpu', 'directml', 'cuda'], table: 'cpu',
      timings: [['cpu', 236, 240], ['directml', 74, 1630]] },
    { id: 'samTs', modelName: 'SAM-TS-L lettering mask', modelKey: 'models.kind.samTs', supported: ['cpu'], table: 'cpu',
      timings: [['cpu', 5120, 3900]] },
    { id: 'inpainter', modelName: 'LaMa Manga', modelKey: 'models.kind.inpainter', supported: ['cpu', 'directml', 'cuda', 'webgpu'], table: 'directml',
      timings: [['cpu', 2310, 1700], ['directml', 402, 2900], ['webgpu', null, null, 'accel.declined.failed']] },
    { id: 'hayai', modelName: 'Hayai OCR v2.5 Nova', modelKey: 'models.kind.hayai', supported: ['cpu', 'cuda'], table: 'cpu',
      timings: [['cpu', 188, 350]] },
  ]

  /** The saved measurement, as `measurements.json` would hold it. */
  let mockMeasurement = null
  let mockMeasuring = null
  let devLog = [
    '2026-10-31T09:12:03Z Manga Cleaner 1.0.0-beta.12 on windows x86_64',
    '2026-10-31T09:12:41Z manga-cleaner: detection model: comic-text-and-bubble-detector-v2-int8',
    '2026-10-31T09:13:02Z manga-cleaner: SAM-TS-L did not open on DirectML, using the CPU: 887A0005',
    '2026-10-31T09:13:40Z manga-cleaner: ~/AppData/Roaming/com.mangacleaner.app/projects/a-very-long-project-name/chapter-009/job/page-0012.png changed under the run; reading it again',
    '2026-10-31T09:14:18Z manga-cleaner: cloud clean could not read page 3: error sending request for url (https://[hidden].modal.run/[hidden])',
  ].join('\n') + '\n'

  /**
   * One model's row, as `accelerators_configured` builds it.
   *
   * @param {typeof MOCK_PLACED_MODELS[number]} model
   * @param {string} preference - the whole-app setting
   * @param {Array<[string, boolean, boolean, boolean, string|null]>} providers
   */
  function placeMockModel(model, preference, providers) {
    const requested = state.settings.modelAccelerators?.[model.id] ?? preference
    const available = (id) => providers.some(([provider, ok]) => provider === id && ok)
    const backendStatus = providers.map(([id, ok, , , reasonKey]) => {
      const supported = model.supported.includes(id)
      return {
        id,
        supported,
        installed: id === 'cpu' || ok || reasonKey === 'accel.declined.missingRuntime',
        available: supported && (id === 'cpu' || ok),
        verified: false,
        reasonKey: supported && !(id === 'cpu' || ok) ? reasonKey : null,
      }
    })
    const winner = mockMeasurement?.models.find((row) => row.id === model.id)?.winner ?? null
    let acceleratorId = model.table
    let noteKey = model.table === 'cpu' ? null : 'accel.chosen.unmeasured'
    let declinedKey = null
    let declinedId = null
    let neededBytes = null
    let roomBytes = null
    if (requested === 'auto' && winner) {
      acceleratorId = winner
      noteKey = 'accel.chosen.measuredHere'
    } else if (requested === 'cpu') {
      acceleratorId = 'cpu'
      noteKey = null
    } else if (requested !== 'auto') {
      noteKey = null
      if (!model.supported.includes(requested) || !available(requested)) {
        acceleratorId = 'cpu'
        declinedKey = 'accel.declined.unavailable'
        declinedId = requested
      } else if (model.id === 'inpainter' && requested === 'directml') {
        // The memory decline, with the two figures the sentence cannot carry.
        acceleratorId = 'cpu'
        declinedKey = 'accel.declined.memory'
        declinedId = 'directml'
        neededBytes = 6_034_997_248
        roomBytes = 3_221_225_472
      } else {
        acceleratorId = requested
      }
    }
    return {
      id: model.id,
      modelName: model.modelName,
      modelKey: model.modelKey,
      preference: requested,
      supportedIds: model.supported,
      backendStatus,
      acceleratorId,
      labelKey: `accel.${acceleratorId}`,
      noteKey,
      declinedKey,
      declinedId,
      neededBytes,
      roomBytes,
    }
  }

  // FIXTURE KNOB: `?firstLaunch=1` empties this machine, which is the only way
  // to see the first-launch offer in a browser - the fixture above is a
  // machine missing the redraw model alone. Applied here rather than
  // where `missingModels` is built so it can name the catalogue's own ids
  // instead of a second copy of them, and inert inside a Tauri window, which
  // answers from disk.
  if (new URLSearchParams(globalThis.location?.search ?? '').has('firstLaunch')) {
    state.missingModels = new Set([...MOCK_CATALOGUE.map((model) => model.id), MOCK_RUNTIME_ID])
  }

  /**
   * A download as a chain of timers, and the one place three of the seam's
   * seven model methods meet.
   *
   * Eight ticks whatever the size, because what is being imitated is the
   * *shape* of the report - a first event at zero, a run of partial ones, and
   * exactly one carrying `done` - rather than a transfer rate the mock has no
   * way to have. Cancelling ends it through the same `done` event a failure
   * would, so the interface has one path back to "not installed".
   *
   * The answer says **why** it did not start one:
   * `alreadyRunning` for an id in flight, `alreadyInstalled` for
   * a weight already on disk. An unknown id is neither and is still a rejection
   * - that is a caller's bug rather than a race between two windows.
   *
   * @param {string} id
   * @param {boolean} [installedIsFine] the runtime, which is re-downloaded to switch build
   * @returns {import('./backend.js').DownloadStart}
   */
  function startDownload(id, installedIsFine = false) {
    if (state.downloads.has(id)) return 'alreadyRunning'
    const known = id === MOCK_RUNTIME_ID || MOCK_CATALOGUE.some((model) => model.id === id)
    if (!known) throw new Error(`no such model: ${id}`)
    if (!installedIsFine && !state.missingModels.has(id)) return 'alreadyInstalled'
    const total = MOCK_CATALOGUE.find((model) => model.id === id)?.bytes ?? MOCK_RUNTIME_BYTES
    // A press resumes from whatever the last one left, which is the whole of
    // the `.part` contract: the first event carries the prefix
    // rather than zero, so a bar picking a download back up starts where it
    // stopped.
    let sent = state.partials.get(id) ?? 0
    const step = Math.max(1, Math.ceil(total / 8))
    const tick = () => {
      if (!state.downloads.has(id)) return
      sent = Math.min(total, sent + step)
      if (sent >= total) {
        state.downloads.delete(id)
        state.partials.delete(id)
        state.missingModels.delete(id)
        state.verifiedModels.set(id, true)
        emit({ type: 'model-progress', id, downloaded: total, total, done: true, error: null })
        for (const groupId of activeModelGroups) {
          const members = MOCK_MODEL_GROUPS[groupId]
          if (!members.every((member) => !state.missingModels.has(member) && !state.downloads.has(member))) continue
          activeModelGroups.delete(groupId)
          emit({ type: 'model-progress', id: groupId, downloaded: 0, total: null, done: true, error: null })
        }
        return
      }
      // Written down on every tick rather than on the way out, because a
      // cancellation is a timer that simply stops: what survives it is what the
      // last tick had, exactly as a `.part` is however many bytes reached the
      // disk before the thread noticed the flag.
      state.partials.set(id, sent)
      emit({ type: 'model-progress', id, downloaded: sent, total, done: false, error: null })
      state.downloads.set(id, timers.setTimeout(tick, timing.method))
    }
    state.downloads.set(id, timers.setTimeout(tick, timing.method))
    emit({ type: 'model-progress', id, downloaded: sent, total, done: false, error: null })
    return 'started'
  }

  async function startModelGroup(groupId) {
    const members = MOCK_MODEL_GROUPS[groupId]
    if (!members) throw new Error(`no such model group: ${groupId}`)
    if (members.some((member) => state.downloads.has(member))) return 'alreadyRunning'
    const missing = members.filter((member) => state.missingModels.has(member))
    if (!missing.length) return 'alreadyInstalled'
    activeModelGroups.add(groupId)
    for (const member of missing) startDownload(member)
    return 'started'
  }

  async function deleteModelGroupFiles(groupId) {
    const members = MOCK_MODEL_GROUPS[groupId]
    if (!members) throw new Error(`no such model group: ${groupId}`)
    const present = members.filter((member) => !state.missingModels.has(member))
    if (!present.length) return 'notFound'
    if (present.some((member) => state.downloads.has(member))) return 'busy'
    await delay(timing.method)
    for (const member of present) {
      state.missingModels.add(member)
      state.verifiedModels.delete(member)
    }
    return 'deleted'
  }

  function cancelDownloadById(id) {
    const activeGroup = [...activeModelGroups].find((groupId) => MOCK_MODEL_GROUPS[groupId].includes(id))
    if (activeGroup) {
      const members = MOCK_MODEL_GROUPS[activeGroup]
      activeModelGroups.delete(activeGroup)
      for (const member of members) {
        const handle = state.downloads.get(member)
        if (handle === undefined) continue
        timers.clearTimeout(handle)
        state.downloads.delete(member)
        emit({ type: 'model-progress', id: member, downloaded: state.partials.get(member) ?? 0, total: null, done: true, error: 'cancelled' })
      }
      emit({ type: 'model-progress', id: activeGroup, downloaded: 0, total: null, done: true, error: 'cancelled' })
      return true
    }
    const handle = state.downloads.get(id)
    if (handle === undefined) return false
    timers.clearTimeout(handle)
    state.downloads.delete(id)
    emit({ type: 'model-progress', id, downloaded: 0, total: null, done: true, error: 'cancelled' })
    return true
  }

  /* ---------- cloud targets and secrets ---------- */

  const secretKey = (provider, profileId, role) => `${provider}:${profileId}:${role}`

  const requireSecretSpec = ({ provider, profileId, role } = {}) => {
    if (provider !== 'modal' && provider !== 'beam') throw new Error(`unknown cloud provider '${provider}'`)
    if (typeof profileId !== 'string' || profileId.trim() === '') throw new Error('profileId is required')
    if (!['setup', 'runtime', 'model_download'].includes(role)) throw new Error(`unknown secret role '${role}'`)
    return { provider, profileId, role }
  }

  const secretSummary = ({ provider, profileId, role }) => ({
    provider,
    profileId,
    role,
    present: state.secrets.has(secretKey(provider, profileId, role)),
    backend: 'session',
  })

  const profileOf = (provider, profileId) =>
    (provider === 'beam'
      ? state.inferenceConfig.beamProfiles?.[profileId]
      : state.inferenceConfig.modalProfiles?.[profileId]) ?? null

  /**
   * Replace the inference configuration, advancing every profile's epoch and
   * dropping cached proposals and grants, so consent given against the old
   * configuration cannot be spent against the new one.
   */
  const replaceInferenceConfig = (projected) => {
    const ids = new Set([
      ...Object.keys(state.inferenceConfig.beamProfiles || {}),
      ...Object.keys(state.inferenceConfig.modalProfiles || {}),
      ...Object.keys(projected.beamProfiles || {}),
      ...Object.keys(projected.modalProfiles || {}),
    ])
    for (const id of ids) {
      state.profileEpochs.set(id, (state.profileEpochs.get(id) ?? 1) + 1)
    }
    state.cachedProposals.clear()
    state.cachedGrants.clear()
    state.inferenceConfig = projected
  }

  /* ---------- interactive cloud renders ---------- */

  /**
   * The phases a render reports (IC-3) and how long the mock stays in each, as
   * a share of `timing.cloud`. The first render of a session also waits in
   * `queued` for a cold start, the way a provider brings a GPU up from zero.
   */
  const CLOUD_PHASES = Object.freeze([
    ['preparing', 0.2],
    ['submitting', 0.3],
    ['queued', 0.6],
    ['running', 1.5],
    ['downloading', 0.3],
    ['compositing', 0.2],
  ])
  const COLD_START_SHARE = 2.5

  /**
   * The permission check every cloud-capable command makes first, as
   * `region.rs` makes it: refused with `notice.cloud.blocked`, before any
   * event, when the switch is off.
   */
  const cloudBlocked = () => {
    if (state.settings.cloudEngines === 'allowed') return false
    notify('notice.cloud.blocked', {}, 'warn')
    return true
  }

  /**
   * The checks the native render makes before it spends anything: a live and
   * unspent grant, the target it was granted for still selected, a recipe and
   * an intent, and a runtime secret for that target. Rejections are the native
   * side's stable codes, thrown for `renderWithGrant` to report.
   */
  const claimCloudGrant = (params) => {
    const nonce = params?.grantNonce
    const grant = typeof nonce === 'string' ? state.cachedGrants.get(nonce) : null
    if (!grant || timers.now() > grant.expiresAtMs) throw new Error('consent_invalid')
    if ((grant.usedAttempts ?? 0) >= (grant.allowedAttempts ?? 1)) throw new Error('consent_invalid')
    if (!params.recipe || !params.intent) throw new Error('invalid_request')
    // As `render_cloud` in `region.rs`: the render goes to the target its
    // grant was minted for, selected now or not, while that profile exists.
    const target = params.executionTarget
    const profile = target ? profileOf(target.type, target.profile_id) : null
    if (!profile) throw new Error('target_changed')
    if (!state.secrets.has(secretKey(target.type, target.profile_id, 'runtime'))) {
      throw new Error('credential_missing')
    }
    grant.usedAttempts = (grant.usedAttempts ?? 0) + 1
    return { profile }
  }

  /**
   * One cloud render with the grant `params` carries, answered the way
   * `region.rs#render_in_cloud` answers: `{mask}` when it committed, or
   * `{phase, code}` when it stopped - `failed`, `cancelled` or `unknown`.
   * Every ending is also a `cloud://attempt` event, the grant's own refusals
   * included, so a render the interface is waiting on never goes quiet. An
   * endpoint whose address says `offline` or `unreachable` fails at submit,
   * the same knob `checkCloudConnection` reads.
   */
  const renderWithGrant = (params, found, commit) => {
    const nonce = String(params.grantNonce)
    const attemptId = cloudAttemptId(nonce)
    const job = {
      attemptId,
      regionId: found.region.id,
      chapterId: found.chapter.id,
      pageIndex: found.page.index,
      startedAt: timers.now(),
      cancelled: false,
      timer: null,
      step: null,
      // The phase last reported, which is what a cancel is answered by.
      phase: 'preparing',
    }
    const report = (phase, errorCode = null) => {
      job.phase = phase
      emitTo(attemptListeners, {
        attemptId,
        regionId: job.regionId,
        chapterId: job.chapterId,
        pageIndex: job.pageIndex,
        phase,
        elapsedMs: Math.max(0, timers.now() - job.startedAt),
        errorCode,
      })
    }
    // A render already running under this id is the one being watched: the
    // native side refuses a second one before a single event goes out.
    if (state.cloudJobs.has(attemptId)) return Promise.resolve({ phase: 'failed', code: 'attempt_busy' })
    report('preparing')
    let profile
    try {
      ;({ profile } = claimCloudGrant(params))
    } catch (error) {
      const code = error instanceof Error ? error.message : 'consent_invalid'
      report('failed', code)
      return Promise.resolve({ phase: 'failed', code })
    }
    state.cloudJobs.set(attemptId, job)
    const unreachable = /offline|unreachable/.test(profile.endpointUrl)
    const phases = CLOUD_PHASES.filter(([phase]) => phase !== 'preparing').map(([phase, share]) => [
      phase,
      Math.round(timing.cloud * (phase === 'queued' && !state.cloudWarm ? share + COLD_START_SHARE : share)),
    ])
    const preparing = Math.round(timing.cloud * CLOUD_PHASES[0][1])
    return new Promise((resolve) => {
      let index = 0
      const end = (phase, code) => {
        state.cloudJobs.delete(attemptId)
        report(phase, code)
        resolve({ phase, code })
      }
      job.step = () => {
        job.timer = null
        if (job.cancelled) return end('cancelled', 'cancelled')
        if (index < phases.length) {
          const [phase, ms] = phases[index]
          index += 1
          if (phase === 'queued' && unreachable) return end('failed', 'gateway_unreachable')
          report(phase)
          job.timer = timers.setTimeout(job.step, ms)
          return
        }
        state.cloudJobs.delete(attemptId)
        state.cloudWarm = true
        const mask = commit(cloudRecordFor(attemptId, params.executionTarget, params.recipe))
        report('committed')
        resolve({ mask })
      }
      job.timer = timers.setTimeout(job.step, preparing)
    })
  }

  /** Does this request name a cloud engine or target, as `region.rs` reads one? */
  const wantsCloud = (params = {}) =>
    params?.engine === 'cloud' ||
    ['executionTarget', 'target'].some(
      (key) => params?.[key] !== undefined && params[key]?.type !== 'local',
    )

  /**
   * The record `inference/service.rs` writes beside a cloud render's patch,
   * which is what marks a mask as the cloud's. No cost: the endpoint does not
   * report one.
   *
   * @param {string} attemptId
   * @param {{type: string, profile_id: string}|null|undefined} target
   * @param {any} [recipe]
   */
  const cloudRecordFor = (attemptId, target, recipe) => ({
    provider: target?.type === 'beam' ? 'beam' : 'modal',
    profile_id: target?.profile_id ?? null,
    job_id: `job-${attemptId.slice(4, 20)}`,
    request_id: `req-${attemptId.slice(4, 20)}`,
    attempt_id: attemptId,
    recipe_id: recipe?.recipe_id ?? PINNED_CLOUD_RECIPE_ID,
    model: recipe?.model_id ?? PINNED_CLOUD_MODEL_ID,
    model_revision: recipe?.model_revision ?? PINNED_CLOUD_MODEL_REVISION,
    tier: null,
    cost: null,
    duration_ms: null,
  })

  /**
   * A render's result on its region: FLUX, reconstructed, cleaned, with the
   * cloud record beside it, as the native side commits one.
   */
  const commitCloudResult = (found, cloud, tool) => {
    // A detection the cloud cleaned keeps what it replaced and is one undo
    // step, as one cleaned here is (`cleaningDetection`).
    const wasDetection = isDetection(found.region)
    const mask = cleaningDetection(found.region, found.page, () => {
      const committed = commitMask(found.region, toolContext, {
        engine: 'flux',
        fillMode: 'reconstruct',
        ...(tool ? { tool } : {}),
      })
      committed.provenance.cloud = cloud
      // A detection the cloud cleaned is no longer one, as `cleanDetection`
      // leaves a detection cleaned here.
      delete found.region.pick
      delete found.region.detector
      delete found.region.insideBubble
      return committed
    })
    if (found.page.status === 'unclean') found.page.status = 'cleaned'
    // A page whose last detection was just cleaned is no longer waiting. A
    // run sets its own page's status when it leaves the page.
    if (wasDetection && found.page.status === 'detected') found.page.status = settledStatus(found.page)
    return mask
  }

  /* ---------- the cloud provisioner ---------- */

  const PROVISION_PROTOCOL = '1.0.0'
  const MODEL_WEIGHTS_BYTES = 5_475_930_180
  /** The six page denoise preset models (`deploy/cloud/common/denoise.py`). */
  const DENOISE_MODELS_BYTES = 506_170_387

  const helperSuccess = (op, requestId, data) => ({
    protocol_version: PROVISION_PROTOCOL,
    request_id: requestId || `req-mock-${op}`,
    success: true,
    data,
    error: null,
  })

  const helperFailure = (op, requestId, code, message) => ({
    protocol_version: PROVISION_PROTOCOL,
    request_id: requestId || `req-mock-${op}`,
    success: false,
    data: null,
    error: { code, message, actionable_guidance: null, remedy_steps: [] },
  })

  const credentialsPresent = (provider, credentials = {}) =>
    provider === 'modal'
      ? Boolean(credentials.token_id?.trim?.() && credentials.token_secret?.trim?.())
      : Boolean((credentials.token ?? credentials.beam_token)?.trim?.())

  const credentialsDenied = (credentials = {}) =>
    Object.values(credentials).some((value) => typeof value === 'string' && value.includes('denied'))

  const installationKey = (provider, installationId) => `${provider}:${installationId}`

  const isInstallationId = (value) => typeof value === 'string' && /^[a-z0-9][a-z0-9-]{2,40}$/.test(value)

  /** Where a Modal gateway's requests may enter (`deploy/cloud/common/deployment.py`). */
  const MOCK_ROUTING_REGIONS = ['us-east', 'us-west', 'eu-west', 'ap-south']

  /** What a plan for these choices creates, in the shape the helper answers. */
  const planFor = (provider, installationId, options = {}) => {
    const gpus = MOCK_PROVISION_GPUS[provider]
    const gpu = options.gpu ?? gpus.fallback
    const idleSeconds = options.idle_seconds ?? 120
    const analysisModels = options.analysis_models ?? []
    if (!Array.isArray(analysisModels) || analysisModels.length > 2 ||
        analysisModels.some((item) => item !== 'text_mask_sam_ts@1' && item !== 'text_regions_rt@1') ||
        new Set(analysisModels).size !== analysisModels.length) {
      return { error: ['ERR_VALIDATION_ERROR', 'Choose SAM-TS-L lettering mask, Ogkalu comic text & bubble detector (Full), or both'] }
    }
    const denoise = options.denoise ?? false
    if (typeof denoise !== 'boolean') return { error: ['ERR_VALIDATION_ERROR', 'denoise must be true or false'] }
    if (denoise && provider !== 'modal') {
      return { error: ['ERR_VALIDATION_ERROR', 'Page denoise is only available on Modal installations'] }
    }
    const routingRegion = options.routing_region ?? 'us-east'
    if (!MOCK_ROUTING_REGIONS.includes(routingRegion) || (routingRegion !== 'us-east' && provider !== 'modal')) {
      return { error: ['ERR_VALIDATION_ERROR', 'routing_region must be one of us-east, us-west, eu-west, ap-south, on Modal'] }
    }
    if (!gpus.options.includes(gpu)) {
      return { error: ['ERR_VALIDATION_ERROR', `GPU '${gpu}' is not offered for ${provider}`] }
    }
    if (!Number.isInteger(idleSeconds) || idleSeconds < 60 || idleSeconds > 600) {
      return { error: ['ERR_VALIDATION_ERROR', 'idle_seconds must be an integer from 60 to 600'] }
    }
    // `none`: a Modal setup with no cleaning model, as the helper plans it.
    const modelId = options.model_id ?? PINNED_CLOUD_MODEL_ID
    const noModel = modelId === 'none'
    if (noModel ? provider !== 'modal' : modelId !== PINNED_CLOUD_MODEL_ID) {
      return { error: ['ERR_VALIDATION_ERROR', 'model_id is not a supported cloud render model'] }
    }
    if (noModel && analysisModels.length === 0 && !denoise) {
      return { error: ['ERR_VALIDATION_ERROR', 'choose a cleaning model, a detection model or page denoise'] }
    }
    const resources =
      provider === 'modal'
        ? [
            { type: 'volume', name: `mc-weights-${installationId}`, purpose: 'Model weights' },
            { type: 'dict', name: `mc-jobs-${installationId}`, purpose: 'Job state' },
            { type: 'app', name: `mc-${installationId}`, purpose: 'Gateway and GPU worker' },
            { type: 'proxy_token', name: `mc-token-${installationId}`, purpose: 'Runtime access token' },
          ]
        : [
            { type: 'volume', name: `mc-weights-${installationId}`, purpose: 'Model weights' },
            { type: 'gateway', name: `mc-gateway-${installationId}`, purpose: 'Gateway' },
            { type: 'worker', name: `mc-worker-${installationId}`, purpose: 'GPU worker' },
            ...(analysisModels.length ? [{ type: 'worker', name: `mc-analysis-${installationId}`, purpose: 'GPU analysis worker' }] : []),
          ]
    const plan = {
      plan_id: `plan-${provider}-${installationId}`,
      installation_id: installationId,
      provider,
      app_name: `mc-${installationId}`,
      target_environment: provider === 'modal' ? 'main' : 'default',
      resource_allocation: {
        gpu,
        gpu_options: [...gpus.options],
        idle_seconds: idleSeconds,
        max_containers: 1,
        model_weights_bytes: noModel ? 0 : MODEL_WEIGHTS_BYTES,
        model_id: modelId,
        model_options: [
          { model_id: PINNED_CLOUD_MODEL_ID, label: 'FLUX.2 Klein 4B (4-bit)', weights_bytes: MODEL_WEIGHTS_BYTES, license: 'Apache-2.0', required_gpu: null },
          ...(provider === 'modal' ? [{ model_id: 'none', label: 'No cleaning model', weights_bytes: 0, license: '', required_gpu: null }] : []),
        ],
        analysis_models: [...analysisModels],
        analysis_graph_bytes: analysisModels.reduce((total, item) => total +
          (item === 'text_mask_sam_ts@1' ? 1_358_010_626 : 168_481_531), 0),
        analysis_options: [
          { capability: 'text_regions_rt@1', label: 'Ogkalu comic text & bubble detector (Full)' },
          { capability: 'text_mask_sam_ts@1', label: 'SAM-TS-L lettering mask' },
        ],
        denoise,
        denoise_supported: provider === 'modal',
        denoise_models_bytes: DENOISE_MODELS_BYTES,
        ...(provider === 'modal' ? { routing_region: routingRegion, routing_region_options: [...MOCK_ROUTING_REGIONS] } : {}),
      },
      resources_to_create: resources,
      runtime_credential_kind: provider === 'modal' ? 'modal_proxy' : 'beam_bearer',
      required_permissions: [],
      estimated_monthly_cost: 'Billed by your provider. Nothing runs while idle.',
      cost_notes: [
        'GPU time is billed by the second while a job runs, plus the idle window before it scales down.',
        'The gateway and the stored weights cost little or nothing while idle.',
      ],
      cleanup_plan_summary: resources.map((resource) => `Delete ${resource.type} '${resource.name}'`),
      created_at_utc: new Date(timers.now()).toISOString(),
    }
    const { created_at_utc: _created, ...hashed } = plan
    return { plan: { ...plan, plan_hash: sha256Hex(JSON.stringify(hashed)) } }
  }

  /**
   * Installations another computer set up in the mock Modal account, which
   * `inspect` lists (`existing_installations`) so reusing one can be seen in a
   * browser: `?cloudExisting=1` is one with its models downloaded and its
   * choices recorded; `?cloudExisting=2` adds an older one whose download did
   * not finish and that recorded no choices. Reusing one runs the pipeline
   * with the weights step finishing at once, as the helper's does.
   */
  const cloudExistingKnob = String(
    options.cloudExisting ?? new URLSearchParams(globalThis.location?.search ?? '').get('cloudExisting') ?? '',
  )
  const existingInstallations = () => {
    const seeded = cloudExistingKnob === '1' || cloudExistingKnob === '2'
    const list = !seeded ? [] : [
      {
        installation_id: 'mc-k3v9qa',
        app_name: 'mc-k3v9qa',
        created_at: 1_757_900_000,
        deployed_at: 1_758_300_000,
        weights_checked: true,
        models_ready: [PINNED_CLOUD_MODEL_ID],
        analysis_ready: [],
        options: { gpu: 'L4', idle_seconds: 120, model_id: PINNED_CLOUD_MODEL_ID, analysis_models: [], denoise: false },
      },
    ]
    if (cloudExistingKnob === '2') {
      list.push({
        installation_id: 'mc-p0old7',
        app_name: 'mc-p0old7',
        created_at: 1_750_100_000,
        deployed_at: null,
        weights_checked: true,
        models_ready: [],
        analysis_ready: [],
        options: null,
      })
    }
    // Setups finished in this session are in the account too, with the choices they recorded.
    for (const [key, installation] of state.installations) {
      if (installation.provider !== 'modal' || installation.stage !== 'completed') continue
      if (list.some((entry) => installationKey('modal', entry.installation_id) === key)) continue
      const chosen = installation.plan.resource_allocation
      list.push({
        installation_id: installation.installationId,
        app_name: installation.installationId,
        created_at: null,
        deployed_at: null,
        weights_checked: true,
        models_ready: chosen.model_id === 'none' ? [] : [PINNED_CLOUD_MODEL_ID],
        analysis_ready: [...chosen.analysis_models],
        options: {
          gpu: chosen.gpu, idle_seconds: chosen.idle_seconds, model_id: chosen.model_id,
          analysis_models: [...chosen.analysis_models], denoise: chosen.denoise === true,
        },
      })
    }
    return list.map((entry) => ({
      ...entry,
      on_this_computer: state.installations.get(installationKey('modal', entry.installation_id))?.stage === 'completed',
    }))
  }
  /** Ids whose models are already in their storage: reusing one downloads nothing. */
  const alreadySeeded = (provider, installationId) =>
    provider === 'modal' &&
    existingInstallations().some((entry) => entry.installation_id === installationId && entry.models_ready.length > 0)

  /** `?cloudSetupFail=deploy` fails the first apply at that step, so Resume can be tried in a browser. */
  const setupFailKnob = new URLSearchParams(globalThis.location?.search ?? '').get('cloudSetupFail')

  const emitProgress = (run, step, stepState, pct = null) =>
    emitTo(provisionListeners, { op: run.op, provider: run.provider, step, state: stepState, pct })

  /**
   * Walk `steps` on timers, emitting IC-2 progress. Resolves to the step that
   * failed, `'cancelled'`, or null when every step finished.
   */
  const walkSteps = (run, steps, { skip = new Set(), failAt = null, instant = new Set() } = {}) =>
    new Promise((resolve) => {
      let index = 0
      const next = () => {
        run.timer = null
        if (run.cancelled) return resolve('cancelled')
        if (index >= steps.length) return resolve(null)
        const step = steps[index]
        index += 1
        if (skip.has(step)) {
          emitProgress(run, step, 'skip')
          return next()
        }
        run.current = step
        emitProgress(run, step, 'start', step === 'weights' ? 0 : null)
        let pct = 0
        const tick = () => {
          run.timer = null
          if (run.cancelled) {
            emitProgress(run, step, 'fail')
            return resolve('cancelled')
          }
          if (step === failAt) {
            emitProgress(run, step, 'fail')
            return resolve(step)
          }
          if (step === 'weights' && pct < 100 && !instant.has(step)) {
            pct += 25
            emitProgress(run, step, 'start', pct)
            run.timer = timers.setTimeout(tick, timing.provision)
            return
          }
          emitProgress(run, step, 'done', step === 'weights' ? 100 : null)
          run.done.add(step)
          next()
        }
        run.timer = timers.setTimeout(tick, timing.provision)
      }
      next()
    })

  /**
   * What `provision.rs` does with a helper's success (IC-1): keep the runtime
   * credential out of the answer, hold it as the profile's runtime secret,
   * keep a Modal setup token for billing, save and select the profile, check
   * its health.
   */
  const finishInstallation = (installation, keepSelected = false) => {
    const { provider, installationId } = installation
    // A resume answers with the address the setup already has.
    const endpointUrl = installation.endpointUrl ?? (
      provider === 'modal'
        ? `https://mock-workspace--mc-${installationId}-gateway.modal.run/mc/v1`
        : `https://mc-${installationId}-gateway.app.beam.cloud/mc/v1`)
    const origin = new URL(endpointUrl).origin
    const now = timers.now()
    const mapKey = provider === 'modal' ? 'modalProfiles' : 'beamProfiles'
    const previous = state.inferenceConfig[mapKey]?.[installationId]
    // A resume keeps the name the person gave it, as `save_installation` does.
    const name = previous?.name ?? `${provider === 'modal' ? 'Modal' : 'Beam'} (${installationId})`
    const selectedTarget = keepSelected ? state.inferenceConfig.selectedTarget : { type: provider, profile_id: installationId }
    replaceInferenceConfig({
      ...state.inferenceConfig,
      selectedTarget,
      [mapKey]: {
        ...(state.inferenceConfig[mapKey] ?? {}),
        [installationId]: {
          id: installationId,
          name,
          endpointUrl,
          canonicalOrigin: origin,
          canonicalOriginFingerprint: sha256Hex(origin),
          createdAtMs: previous?.createdAtMs ?? now,
          updatedAtMs: now,
        },
      },
    })
    state.secrets.add(secretKey(provider, installationId, 'runtime'))
    installation.stage = 'completed'
    installation.endpointUrl = endpointUrl
    return {
      installation_id: installationId,
      provider,
      stage: 'completed',
      endpoint_url: endpointUrl,
      gpu: installation.plan.resource_allocation.gpu,
      idle_seconds: installation.plan.resource_allocation.idle_seconds,
      analysis_models: installation.plan.resource_allocation.analysis_models,
      denoise: installation.plan.resource_allocation.denoise,
      model: installation.plan.resource_allocation.model_id === 'none' ? null : {
        model_id: PINNED_CLOUD_MODEL_ID,
        model_revision: PINNED_CLOUD_MODEL_REVISION,
        recipe_id: PINNED_CLOUD_RECIPE_ID,
        preprocessing_version: CLOUD_PREPROCESSING_VERSION,
      },
      compatibility_status: 'compatible',
      resources_created: installation.plan.resources_to_create.map(({ type, name: resourceName }) => ({
        type,
        name: resourceName,
      })),
      setup_credential_forgotten: true,
      profile: { provider, profile_id: installationId, name, endpoint_url: endpointUrl },
      health: { ok: true, status: 'reachable', latency_ms: 42 },
      selected: selectedTarget?.type === provider && selectedTarget.profile_id === installationId,
    }
  }

  /** `apply` and `resume`: the staged pipeline, with progress, a stop and a resume point. */
  const runInstallation = async (op, provider, params, installation) => {
    const run = {
      op,
      provider,
      cancelled: false,
      timer: null,
      current: null,
      done: installation.done,
    }
    state.provisionRun = run
    try {
      const failAt =
        params.simulate_fail_step ??
        (setupFailKnob && !installation.failedOnce ? setupFailKnob : null) ??
        (installation.failOnceAt && !installation.failedOnce ? installation.failOnceAt : null)
      const reissue = installation.stage === 'completed'
      const skip = reissue
        ? new Set(PROVISION_APPLY_STEPS.filter((step) => step !== 'token' && step !== 'health'))
        : new Set(installation.done)
      const instant = alreadySeeded(provider, installation.installationId) ? new Set(['weights']) : new Set()
      const outcome = await walkSteps(run, PROVISION_APPLY_STEPS, { skip, failAt, instant })
      if (outcome === 'cancelled') {
        installation.stage = 'stopped'
        return helperFailure(op, params.request_id, 'ERR_CANCELLED', 'Setup was stopped.')
      }
      if (outcome) {
        installation.failedOnce = true
        installation.stage = 'failed'
        // `&cloudSetupCode=ERR_ORPHANED_TOKEN` beside the knob picks the code
        // the failure answers with, so a code's own failed view can be seen.
        const knobCode = new URLSearchParams(globalThis.location?.search ?? '').get('cloudSetupCode')
        const code = knobCode && /^ERR_[A-Z_]{1,48}$/.test(knobCode) ? knobCode : 'ERR_EXECUTION_FAILED'
        return helperFailure(op, params.request_id, code, `The ${outcome} step failed.`)
      }
      return helperSuccess(op, params.request_id, finishInstallation(installation, params.keep_selected_target === true))
    } finally {
      if (state.provisionRun === run) state.provisionRun = null
    }
  }

  /** The cleanup plan for an installation: whatever it created, and nothing else. */
  const cleanupPlanFor = (provider, installationId) => {
    const installation = state.installations.get(installationKey(provider, installationId))
    const resources = installation ? installation.plan.resources_to_create : []
    const plan = {
      plan_id: `cleanup-${provider}-${installationId}`,
      installation_id: installationId,
      provider,
      resources_to_delete: resources.map((resource) => ({
        resource_type: resource.type,
        name: resource.name,
      })),
      foreign_resources_ignored: [],
      persistent_storage_requires_explicit_confirmation: true,
      created_at_utc: new Date(timers.now()).toISOString(),
    }
    const { created_at_utc: _created, ...hashed } = plan
    return { ...plan, plan_hash: sha256Hex(JSON.stringify(hashed)) }
  }

  /* ---------- cloud GPU ---------- */

  // FIXTURE KNOB: `?cloudGpu=idle|busy|starting|unsupported|outdated` allows
  // cloud, selects a ready Modal profile and reports its render GPU in that
  // state, so the resource panel's cloud row can be seen and stopped in a
  // browser. `unsupported` and `outdated` answer the way a Beam deployment and
  // one set up before the GPU routes do. `waiting` has no GPU up and one
  // analysis out that never ends until the analysis GPU is stopped, as when
  // the provider has no GPU free: the panel's "Waiting for a GPU" row.
  const cloudGpuKnob =
    options.cloudGpuScenario ?? new URLSearchParams(globalThis.location?.search ?? '').get('cloudGpu')
  const cloudGpu = {
    scenario: cloudGpuKnob,
    /** @type {Map<string, {state: string, since: number, lastActive: number}>} */
    roles: new Map(),
  }
  /** The `waiting` scenario's analysis, as `cloud://analysis` tells one. */
  const waitingAnalysis = (phase) => ({
    proposal_id: 'mock-waiting-analysis', provider: 'modal', profile_id: 'mc-mock-gpu', phase,
  })
  if (cloudGpuKnob) {
    const profileId = 'mc-mock-gpu'
    const endpointUrl = `https://mock-workspace--mc-${profileId}-gateway.modal.run/mc/v1`
    const origin = new URL(endpointUrl).origin
    state.settings.cloudEngines = 'allowed'
    replaceInferenceConfig({
      ...state.inferenceConfig,
      selectedTarget: { type: 'modal', profile_id: profileId },
      modalProfiles: {
        ...(state.inferenceConfig.modalProfiles ?? {}),
        [profileId]: {
          id: profileId, name: `Modal (${profileId})`, endpointUrl, canonicalOrigin: origin,
          canonicalOriginFingerprint: sha256Hex(origin), createdAtMs: timers.now(), updatedAtMs: timers.now(),
        },
      },
    })
    state.secrets.add(secretKey('modal', profileId, 'runtime'))
    if (['idle', 'busy', 'starting'].includes(cloudGpuKnob)) {
      cloudGpu.roles.set('render', { state: cloudGpuKnob, since: timers.now() - 95_000, lastActive: timers.now() - 20_000 })
    }
  }
  // FIXTURE KNOB: `?cloudSetups=demo` allows cloud and saves four Modal setups
  // made on this computer, in three accounts, so Update all can be seen in a
  // browser. Studio A and B have a saved Modal key; B fails its first update
  // at the token step. Home has no saved key, so Update all skips it. Old
  // laptop has no access token, so its cloud code cannot be checked. Home is
  // the default. Add `release=old` for every checked setup to run older code.
  const cloudSetupsKnob =
    options.cloudSetupsScenario ?? new URLSearchParams(globalThis.location?.search ?? '').get('cloudSetups')
  if (cloudSetupsKnob === 'demo') {
    const seeds = [
      { id: 'mc-team01', name: 'Studio A', workspace: 'studio-a', token: true, setupKey: true },
      { id: 'mc-team02', name: 'Studio B', workspace: 'studio-b', token: true, setupKey: true, failOnceAt: 'token' },
      { id: 'mc-team03', name: 'Home', workspace: 'home', token: true, setupKey: false },
      { id: 'mc-team04', name: 'Old laptop', workspace: 'home', token: false, setupKey: false },
    ]
    const modalProfiles = { ...(state.inferenceConfig.modalProfiles ?? {}) }
    for (const [index, seed] of seeds.entries()) {
      const endpointUrl = `https://${seed.workspace}--mc-${seed.id}-gateway.modal.run/mc/v1`
      const origin = new URL(endpointUrl).origin
      modalProfiles[seed.id] = {
        id: seed.id, name: seed.name, endpointUrl, canonicalOrigin: origin,
        canonicalOriginFingerprint: sha256Hex(origin), createdAtMs: index + 1, updatedAtMs: index + 1,
      }
      state.installations.set(installationKey('modal', seed.id), {
        provider: 'modal', installationId: seed.id, plan: planFor('modal', seed.id, {}).plan, stage: 'completed',
        done: new Set(), failedOnce: false, failOnceAt: seed.failOnceAt ?? null, endpointUrl,
      })
      if (seed.token) state.secrets.add(secretKey('modal', seed.id, 'runtime'))
      if (seed.setupKey) state.secrets.add(secretKey('modal', seed.id, 'setup'))
    }
    state.settings.cloudEngines = 'allowed'
    replaceInferenceConfig({ ...state.inferenceConfig, selectedTarget: { type: 'modal', profile_id: 'mc-team03' }, modalProfiles })
  }
  const MOCK_GPU_IDLE_MS = 120_000

  /** GPU commands can inspect a configured profile independently of the default. */
  const requireConfiguredCloud = (provider, profileId) => {
    if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
    const profiles = provider === 'modal' ? state.inferenceConfig.modalProfiles : state.inferenceConfig.beamProfiles
    if (!profiles?.[profileId]) throw new Error('profile_missing')
    if (!state.secrets.has(secretKey(provider, profileId, 'runtime'))) throw new Error('credential_missing')
  }

  /* ---------- cloud recovery ---------- */

  // FIXTURE KNOB: `?cloudRecovery=1` leaves two renders behind "from the last
  // session", one finished and one whose submission nobody can vouch for, so
  // the start-up recovery notice can be seen in a browser.
  if (new URLSearchParams(globalThis.location?.search ?? '').has('cloudRecovery')) {
    const chapter = state.projects[0]?.chapters[0]
    const page = chapter?.pages.find((candidate) => candidate.regions.length > 1)
    if (chapter && page) {
      const [first, second] = page.regions
      const leftOver = (region, phase) => ({
        attemptId: cloudAttemptId(`grant-left-over-${region.id}`),
        grantNonce: null,
        phase,
        status: phase === 'result_cached' ? 'completed' : 'unknown',
        handle: phase === 'unknown' ? null : `handle-left-over-${region.id}`,
        autoRetryable: false,
        chapterId: chapter.id,
        pageIndex: page.index,
        regionId: region.id,
        snapshot: { regionRevision: 1, sourceImageHash: 'src-hash' },
        createdAtMs: timers.now(),
      })
      for (const record of [leftOver(first, 'result_cached'), leftOver(second, 'unknown')]) {
        state.attemptJournal.set(record.attemptId, record)
      }
    }
  }

  /**
   * `reconcile_cloud_recovery` with `apply` (IC-4): attach what finished,
   * leave what is still running, and name what needs a person. Unknown
   * submissions are never resubmitted.
   */
  const recoverCloudAttempts = () => {
    const report = { attached: [], stillRunning: [], needsAttention: [] }
    for (const record of state.attemptJournal.values()) {
      if (record.phase === 'committed' || record.phase === 'abandoned' || record.repairAcknowledged) continue
      const ref = {
        attemptId: record.attemptId,
        chapterId: record.chapterId ?? null,
        pageIndex: record.pageIndex ?? null,
        regionId: record.regionId ?? null,
      }
      if (record.phase === 'unknown') {
        report.needsAttention.push({ ...ref, reason: 'ambiguous' })
      } else if (record.phase === 'accepted' || record.phase === 'cancel_requested') {
        report.stillRunning.push(ref)
      } else if (record.phase === 'result_cached' || record.status === 'completed') {
        const found = record.regionId ? findRegion(record.regionId) : null
        if (!found) {
          record.phase = 'reported'
          report.needsAttention.push({ ...ref, reason: 'stale' })
          continue
        }
        commitCloudResult(
          found,
          cloudRecordFor(record.attemptId, state.inferenceConfig.selectedTarget, null),
        )
        record.phase = 'committed'
        report.attached.push(ref)
      } else if (record.phase === 'failed') {
        record.phase = 'reported'
        report.needsAttention.push({ ...ref, reason: 'failed' })
      }
    }
    return report
  }

  /* ---------- the adapter ---------- */

  return {
    subscribe(handler) {
      handlers.add(handler)
      return () => handlers.delete(handler)
    },

    onProvisionProgress(handler) {
      return listenOn(provisionListeners, handler)
    },

    onCloudAttempt(handler) {
      return listenOn(attemptListeners, handler)
    },

    onRemoteAnalysis(handler) {
      const off = listenOn(remoteAnalysisListeners, handler)
      if (cloudGpu.scenario === 'waiting') handler(waitingAnalysis({ phase: 'submitted_tile', index: 0 }))
      return off
    },

    onDenoiseProgress(handler) {
      return listenOn(denoiseProgressListeners, handler)
    },

    // The mock records a denoise as it answers, so it never puts one off.
    onDenoiseRecorded(handler) {
      return listenOn(new Set(), handler)
    },

    onQuitRequested(handler) {
      return listenOn(quitListeners, handler)
    },

    onSamInstallProgress(handler) {
      return listenOn(samInstallListeners, handler)
    },

    /**
     * What `list_jobs` answers: the runs and the denoises going now. The
     * mock's runs are its own pool (`runner.js#createRunnerPool`).
     */
    async listJobs() {
      await delay(timing.method)
      return [
        ...runner.list(),
        ...[...denoiseRuns].map(([runId, run]) => ({ runId, kind: run.kind, chapterId: run.chapterId, done: run.done, total: run.total })),
      ]
    },

    /**
     * Stop every job, as `confirm_quit` does before it exits. A browser tab
     * has no process to end, so this is where the mock stops.
     */
    async confirmQuit() {
      for (const { runId } of runner.list()) runner.cancel(runId)
      for (const run of denoiseRuns.values()) run.stopped = true
    },

    /** No tray outside the desktop window: nothing to hide into. */
    async hideToTray() {
      return false
    },

    /**
     * Not a seam method: raises `app://quit-requested` as the quit guard
     * would, so the question can be seen in a browser, where closing the tab
     * is never held. `getBackend()` puts it on the window in the mock.
     *
     * @param {{canHide?: boolean}} [options]
     */
    simulateQuitRequest({ canHide = false } = {}) {
      const jobs = runner.list().length + denoiseRuns.size
      emitTo(quitListeners, { jobs, canHide })
      return jobs
    },

    async listProjects() {
      await delay(timing.method)
      // Headers only: Home draws page counts, status marks and a review total,
      // and every one of those is answered without a single region (§1b).
      return snapshot(state.projects.map(projectHeaders))
    },

    async createProject({ name, mode, sourcePath, readingDirection }) {
      await delay(timing.method)
      projectCounter += 1
      const project = buildProject(
        {
          id: `project-${projectCounter}`,
          name,
          mode,
          sourcePath: sourcePath ?? '~/scans/untitled',
          readingDirection,
        },
        sequence,
      )
      state.projects.unshift(project)
      notify('notice.project.created', { modeKey: `project.mode.${mode}` })
      return snapshot(project)
    },

    /**
     * The mock has no filesystem, so it cannot run the real backend's
     * inference - it cannot know whether a subfolder named after the chapter
     * exists. What it can mirror is the rule that inference now obeys:
     * **no chapter is ever given a folder
     * another chapter of the same project already reads.** Its default is the
     * subfolder the name implies, and a collision is refused with the same
     * notice the Tauri backend sends rather than quietly producing two chapters
     * over one folder.
     */
    async createChapter({ projectId, name, number: given, sourcePath }) {
      await delay(timing.method)
      const project = findProject(projectId)
      if (!project) return null
      const resolved = sourcePath || `${project.sourcePath}/${name}`
      const taken = project.chapters.find((c) => c.sourcePath === resolved)
      if (taken) {
        notify('notice.chapter.sourceTaken', { chapter: taken.name }, 'warn')
        return null
      }
      // The user's number when the dialog sent one; `max + 1` is only the
      // fallback for a caller that did not name one.
      const number =
        Number.isFinite(given) && Number(given) >= 1
          ? Math.trunc(Number(given))
          : Math.max(...project.chapters.map((c) => c.number), 0) + 1
      // The mock builds a chapter's id out of its number (`pagebuilder.js`
      // `makeChapter`), so a repeat here is not an ambiguous label - it is two
      // chapters, their pages and their regions sharing one set of ids. The
      // dialog already refuses a taken number; this is the seam saying so
      // rather than trusting it.
      if (project.chapters.some((c) => c.number === number)) {
        notify('notice.chapter.numberTaken', { chapter: number, project: project.name }, 'warn')
        return null
      }
      const chapter = buildChapter(project, { number, name, sourcePath: resolved }, sequence)
      project.chapters.unshift(chapter)
      project.chapters.forEach((c, order) => {
        c.order = order
      })
      notify('notice.chapter.added', { chapter: number, project: project.name })
      return snapshot(chapterHeaders(chapter))
    },

    async openChapter({ projectId, chapterId, convert }) {
      await delay(timing.openChapter)
      const found = findChapter(chapterId) ?? { project: findProject(projectId), chapter: null }
      if (!found.project || !found.chapter) return null
      const { project, chapter } = found
      const needsConversion = !!project.conversion && chapter.sourceFormat !== project.conversion.to

      if (needsConversion && convert) {
        for (const page of chapter.pages) {
          page.file = page.file.replace(/\.[^.]+$/, `.${project.conversion.to.toLowerCase()}`)
        }
        chapter.sourceFormat = project.conversion.to
        notify('notice.convert.finished', {
          count: chapter.pages.length,
          format: project.conversion.to,
        })
      }

      const reports = [...chapter.inputReports]
      const skipped = chapter.pages.filter((page) => page.status === 'skipped')
      if (skipped.length > 0) {
        reports.push({
          key: 'notice.input.fileSkipped',
          params: {
            count: skipped.length,
            file: skipped[0].file,
            reasonKey: skipped[0].skipReason,
          },
          tone: 'warn',
        })
      }
      if (chapter.noTextDetected) {
        reports.push({
          key: 'notice.chapter.emptyResult',
          params: { regions: 0, pages: chapter.pages.length },
          tone: 'warn',
        })
      }
      reports.forEach((report, i) => {
        timers.setTimeout(
          () => notify(report.key, report.params, report.tone),
          timing.noticeStagger * (i + 1),
        )
      })

      return {
        project: snapshot(projectHeaders(project)),
        chapter: snapshot(chapterHeaders(chapter)),
        pendingConversion:
          needsConversion && !convert
            ? { ...project.conversion, fileCount: chapter.pages.length }
            : null,
      }
    },

    /**
     * Bring a window of pages into residency.
     *
     * Whole pages rather than bare region lists, so the status a page's regions
     * imply travels with them: a page whose last mask was deleted while it sat
     * outside the window comes back with the status it now has, not the one its
     * header carried when it left.
     *
     * An index that is not there is skipped rather than refused - the window
     * slides past both ends of a chapter, and clamping it in two places is how
     * the two come to disagree.
     */
    async loadPages({ chapterId, indices }) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      if (!found) return []
      const wanted = Array.isArray(indices) ? indices : []
      return snapshot(
        wanted
          .map((index) => found.chapter.pages.find((page) => page.index === index))
          .filter(Boolean)
          .map(residentPage),
      )
    },

    /* ---------- the undo journal ---------- */

    async historyLoad({ chapterId }) {
      await delay(timing.method)
      return snapshot(viewOf(journalFor(chapterId), undoLimitValue()))
    },

    async historyPush({ chapterId, entry }) {
      await delay(timing.method)
      const journal = journalFor(chapterId)
      pushEntry(journal, entry, undoLimitValue())
      return snapshot(viewOf(journal, undoLimitValue()))
    },

    /** As `history::undo_limit`: the value in force, and its bounds. */
    async undoLimit() {
      await delay(timing.method)
      return { value: undoLimitValue(), ...UNDO_LIMIT }
    },

    /**
     * One step. The cursor moves before anything is replayed, which is what
     * makes an interrupted undo resolve the same way twice: re-applying a delta
     * to a state already in it is a no-op by construction.
     */
    async historyMove({ chapterId, direction }) {
      await delay(timing.method)
      const journal = journalFor(chapterId)
      // A step onto the page a run is on is refused before the cursor moves,
      // as `history.rs#history_move` refuses it: the caller replays nothing
      // and puts its own cursor back.
      const next = journal.entries[direction === 'redo' ? journal.cursor : journal.cursor - 1]
      if (next?.op === 'region-state' || next?.op === 'clean-state') {
        const page =
          findRegion(next.regionId)?.page ?? findPage(next.before?.pageId ?? next.after?.pageId)
        if (page) refuseBusyPage(chapterId, page.index)
      }
      // A page edit is put back here, before the answer, as the native side
      // does it under the lock that moves the cursor.
      let replayed = null
      if (next?.op === 'detection-state') {
        for (const step of next.pages) refuseBusyPage(chapterId, step.pageIndex)
        replayed = replayPageStep(next, direction === 'undo')
      }
      const entry = direction === 'redo' ? redoEntry(journal) : undoEntry(journal)
      return snapshot({ cursor: journal.cursor, entry: entry ?? null, ...(replayed ?? {}) })
    },

    async renameProject({ projectId, name }) {
      await delay(timing.method)
      const project = findProject(projectId)
      if (!project) return null
      project.name = name
      notify('notice.project.renamed', { name })
      return snapshot(project)
    },

    async deleteProject({ projectId }) {
      await delay(timing.method)
      const index = state.projects.findIndex((p) => p.id === projectId)
      if (index === -1) return false
      const [removed] = state.projects.splice(index, 1)
      for (const chapter of removed.chapters) {
        dropKeptFor(chapter.pages)
        journals.delete(chapter.id)
      }
      notify('notice.project.deleted', { name: removed.name }, 'warn')
      return true
    },

    /**
     * Delete a chapter. The mock holds no files, so `sourceFiles` cannot be
     * acted on - but the one refusal the seam promises is a rule about paths,
     * not about files, and the mock can and does keep it: a chapter reading the
     * project's own folder keeps that folder, and the notice says so.
     */
    async deleteChapter({ projectId, chapterId, sourceFiles }) {
      await delay(timing.method)
      const project = findProject(projectId)
      if (!project) return false
      const at = project.chapters.findIndex((c) => c.id === chapterId)
      if (at === -1) return false
      const [removed] = project.chapters.splice(at, 1)
      dropKeptFor(removed.pages)
      journals.delete(chapterId)
      project.chapters.forEach((c, order) => {
        c.order = order
      })
      if (project.interruptedJob?.chapterId === chapterId) project.interruptedJob = null
      const key = !sourceFiles
        ? 'notice.chapter.deleted'
        : !removed.sourcePath
          ? 'notice.chapter.deletedSourceUnknown'
          : removed.sourcePath === project.sourcePath
            ? 'notice.chapter.deletedProjectFolderKept'
            : 'notice.chapter.deletedWithScans'
      notify(key, { chapter: removed.number }, 'warn')
      return true
    },

    async resumeJob({ projectId, chapterId }) {
      await delay(timing.method)
      const project = findProject(projectId)
      const job = project?.interruptedJob
      if (!job) return null
      const targetChapter = chapterId ?? job.chapterId
      const found = findChapter(targetChapter)
      if (!found) return null
      const run = startRun({
        scope: 'chapter',
        chapterId: targetChapter,
        solidBubblesOnly: solidInterrupted.has(job.chapterId),
      })
      notify('notice.job.resumed', { page: job.pageIndex + 1 })
      return {
        project: snapshot(projectHeaders(project)),
        chapter: snapshot(chapterHeaders(found.chapter)),
        resumedFrom: job.pageIndex,
        runId: run.runId,
        pages: run.pages,
        mode: 'auto',
      }
    },

    async runClean({
      scope,
      chapterId,
      pageIndex,
      engineCeiling,
      bubbleEngine,
      outsideEngine,
      outsideBubbles,
      bubbleColor,
      maskPaddingPx = 0,
      detection,
      detectorModels,
      geometryPolicy,
      textPolicy,
      ocrRescue,
      analysisTargets,
      cloudGrant,
      cloudCleanGrant,
      mode = 'auto',
      area = null,
      solidBubblesOnly = false,
    }) {
      await delay(timing.method)
      if (!['auto', 'detect', 'clean'].includes(mode)) throw new Error('invalid_request: mode')
      if (cloudCleanGrant && (mode !== 'auto' || area || !['page', 'chapter'].includes(scope))) {
        throw new Error('cloud_clean_stream_unsupported')
      }
      // The native side holds the Detect to the area and adds what it finds.
      // The mock has no detector to point anywhere, so it checks the request
      // as the native side does and detects the page.
      if (area) {
        const sides = [area.x, area.y, area.w, area.h]
        if (!sides.every(Number.isFinite) || area.x < 0 || area.y < 0 || area.w <= 0 || area.h <= 0
          || area.x + area.w > 100 + 1e-6 || area.y + area.h > 100 + 1e-6) throw new Error('detect_area_invalid')
        if (mode !== 'detect' || scope !== 'page') throw new Error('detect_area_scope_unsupported')
      }
      if (!validPadding(maskPaddingPx)) throw new Error('mask_padding_invalid')
      // The native side's rule, refused the same way: a stage routed to the
      // cloud runs only under a grant for exactly this chapter and these pages.
      // Clean loads no detector, so nothing of it is routed anywhere.
      const targets = analysisTargets ?? state.settings.analysisTargets ?? null
      const capabilities = mode === 'clean' ? [] : mockCloudCapabilities(detectorModels, targets)
      // A cloud run uses CTD and the text reader on this computer beside its
      // cloud stages, and never the Small profile: it has no cloud twin and
      // Full replaces it (`run.rs#cloud_detect_refusal`).
      if (capabilities.length && Array.isArray(detectorModels) && detectorModels.includes('rtSmall')) {
        throw new Error('cloud_detect_small_unsupported')
      }
      if (!capabilities.length && cloudGrant) throw new Error('cloud_run_grant_mismatch: capabilities')
      if (capabilities.length) {
        if (scope === 'project') throw new Error('cloud_run_scope_unsupported')
        if (!cloudGrant) throw new Error('cloud_run_grant_required')
        const grant = runGrants.get(cloudGrant)
        runGrants.delete(cloudGrant)
        if (!grant) throw new Error('cloud_run_grant_missing')
        if (Date.now() >= grant.expiresAtMs) throw new Error('cloud_run_grant_expired')
        if (grant.chapterId !== chapterId) throw new Error('cloud_run_grant_mismatch: chapter')
        const pages = queueFor({ scope, chapterId, pageIndex, outsideBubbles, mode }).map((entry) => entry.page.index)
        if (!pages.length || pages.some((index) => !grant.pageIndices.includes(index))) throw new Error('cloud_run_grant_mismatch: pages')
        if (grant.capabilities.join() !== capabilities.join()) throw new Error('cloud_run_grant_mismatch: capabilities')
        if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
        if (remoteScenario() === 'runFail') {
          // The native run reports a failed page as a notice and stops; this
          // one has no queue to stop, so it refuses after the same notice.
          notify('cloud.analysis.run.pageFailed', { page: pages[0] + 1, code: 'gateway_unreachable' }, 'warn')
          throw new Error('cloud_analysis_failed: gateway_unreachable')
        }
      }
      // As `open_stream` in `cloud_clean.rs`: spent before the run starts,
      // and only for the pages its consent named.
      /** @type {any} */
      let cloudClean = null
      if (cloudCleanGrant) {
        const grant = cleanGrants.get(cloudCleanGrant)
        if (!grant) throw new Error('cloud_clean_grant_missing')
        if (grant.chapterId !== chapterId) throw new Error('cloud_clean_grant_mismatch: chapter')
        cleanGrants.delete(cloudCleanGrant)
        if (Date.now() >= grant.expiresAtMs) throw new Error('cloud_clean_grant_expired')
        if (!grant.stream) throw new Error('cloud_clean_grant_mismatch: stream')
        if (!profileOf(grant.target.type, grant.target.profile_id)) throw new Error('cloud_clean_profile_not_active')
        if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
        const pages = queueFor({ scope, chapterId, pageIndex, outsideBubbles, mode }).map((entry) => entry.page.index)
        if (pages.some((index) => !grant.pageIndices.includes(index))) throw new Error('cloud_clean_grant_mismatch: pages')
        cloudClean = { grantId: cloudCleanGrant, capUsd: grant.capUsd, target: grant.target }
        solidBubblesOnly = false
      }
      // As the native run start does: a local lettering mask with less free
      // memory than it wants warns and runs anyway.
      // The notice names its run, so its Stop button stops that one.
      const localSam = Array.isArray(detectorModels) && detectorModels.includes('samTs') && !capabilities.length
      const lowMemory = localSam && mode !== 'clean' && state.memoryRoomBytes < 5_000_000_000
      const handle = await startRun({
        scope,
        chapterId,
        pageIndex,
        engineCeiling,
        bubbleEngine,
        outsideEngine,
        outsideBubbles,
        bubbleColor,
        maskPaddingPx,
        detection,
        detectorModels,
        geometryPolicy,
        textPolicy,
        ocrRescue,
        mode,
        detector: capabilities.length ? 'cloud' : 'local',
        area,
        solidBubblesOnly,
        cloudClean,
      })
      if (lowMemory && handle?.runId && !handle.alreadyRunning) {
        notify('notice.run.lowMemory', { free: state.memoryRoomBytes, runId: handle.runId }, 'warn')
      }
      return handle
    },

    async cancelRun({ runId } = {}) {
      await delay(timing.method)
      return runner.cancel(runId)
    },

    async applyTool({ tool, params = {}, chapterId, pageIndex, regionId }) {
      if (tool === 'autoClean') {
        await delay(timing.method)
        // A detected region is cleaned on its own, from its stored mask, here -
        // unless the cloud was chosen for it, which the grant path below renders.
        const detection = regionId ? findRegion(regionId) : null
        if (detection && isDetection(detection.region) && typeof params.grantNonce !== 'string' && !wantsCloud(params)) {
          refuseBusyPage(detection.chapter.id, detection.page.index)
          cleanDetection(detection.region, detection.page, params)
          detection.page.status = settledStatus(detection.page)
          return {
            status: 'applied',
            region: snapshot(detection.region),
            mask: snapshot(detection.region.mask),
            pageStatus: detection.page.status,
          }
        }
        // Text cleanup page runs use startRun. applyTool edits a stored region.
        if (!regionId) {
          return { status: 'not-found' }
        }
      }
      const grant = typeof params.grantNonce === 'string'
      if ((grant || wantsCloud(params)) && cloudBlocked()) {
        await delay(timing.method)
        return { status: 'blocked', errorCode: 'cloud_disabled' }
      }
      const found = findRegion(regionId)
      if (!found) return { status: 'not-found' }

      if (grant) {
        await delay(timing.method)
        const ended = await renderWithGrant(params, found, (cloud) => commitCloudResult(found, cloud, tool))
        if (!ended.mask) return { status: ended.phase, errorCode: ended.code }
        return {
          status: 'applied',
          region: snapshot(found.region),
          mask: snapshot(ended.mask),
          pageStatus: found.page.status,
        }
      }
      // A cloud engine with no grant: the interface's consent step comes first.
      if (wantsCloud(params)) {
        await delay(timing.method)
        return { status: 'needs-confirmation' }
      }

      // A hand tool over a stored detection cleans it too, so it keeps the
      // detection and is the backend's undo step, as `region.rs` commits it.
      const applied = cleaningHere(found, () => applyToolToRegion(found.region, found.page, toolContext, { tool, params }))
      await delay(timing.method)
      notifyAll(applied.notices)
      return {
        status: 'applied',
        region: snapshot(found.region),
        mask: snapshot(applied.mask),
        pageStatus: found.page.status,
      }
    },

    /**
     * A region drawn by hand, where the detector found nothing.
     *
     * The additive half of the tool seam. `applyTool` needs a region id, and
     * Brush, Shapes and the AI mask brush exist precisely to make masks where
     * there is no region yet. The geometry arrives in the
     * region's own normalised coordinate space, so the canvas, the Layers panel
     * and the export all describe the same rectangle.
     *
     * Returns the page's status with the region for the same reason every other
     * region-level edit does: a hand mask on an unclean page makes it cleaned.
     */
    async createRegion({ chapterId, pageIndex, sourceIndex, sourceSha, bbox, tool, params = {} }) {
      await delay(timing.method)
      refuseBusyPage(chapterId, pageIndex)
      // Always local, as the native command is: a cloud engine is refused while
      // cloud is off, and otherwise dropped for the fill mode's local default.
      // The interface asks for consent and applies the cloud to the new region.
      if (wantsCloud(params)) {
        if (cloudBlocked()) return null
        const { engine: _cloud, executionTarget: _target, target: _named, ...local } = params
        params = local
      }
      const found = findChapter(chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === pageIndex)
      if (!page || !bbox) return null
      if (sourceIndex !== undefined && page.sourceIndex !== sourceIndex) return null
      if (sourceSha !== undefined && page.sourceSha !== sourceSha) return null
      handCounter += 1
      const created = createHandRegion(page, toolContext, {
        id: `${page.id}-h${handCounter}`,
        bbox,
        tool,
        params,
      })
      notifyAll(created.notices)
      return { region: snapshot(created.region), pageStatus: page.status }
    },

    /**
     * Grow or erase the page's detections by hand, before anything is
     * cleaned.
     *
     * **An approximation on boxes.** The native command moves mask pixels;
     * the mock has no pixels, so it works on each detection's `bbox` and on
     * the box around the gesture. `add` grows the detection the gesture
     * overlaps most to take that box in, or makes a new detection shaped like
     * the ones a Detect run leaves where it overlaps none. `remove` deletes
     * every detection whose box centre the gesture's box covers, and changes
     * no other. Cleaned layers are never touched, as natively.
     *
     * On a longstrip chapter the box reaches one page past the anchor each
     * way (`maskEditFrame`) and is compared with those pages' detections in
     * the anchor's percent. A grown detection stays on its page and may hang
     * past 100 into the next; a new one goes to the page holding the box's
     * top row, as natively. Every page written is answered in `pages`.
     */
    /**
     * An eraser stroke over the layers under it, as `region.rs`
     * `erase_paint` takes it: every layer `isErasableLayer` names, and in a
     * longstrip a neighbour's hanging across the join (`maskEditFrame`). The
     * mock keeps no pixels, so it works on boxes: a layer the stroke's swept
     * box covers whole goes off the page, and one it only crosses is answered
     * as changed, with a new revision for an undo to name. Each is answered
     * with the page that owns it.
     */
    async erasePaint({ chapterId, pageIndex, points, size } = {}) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === pageIndex)
      if (!page) return null
      refuseBusyPage(chapterId, page.index)
      const path = (points ?? []).filter((point) => Number.isFinite(point?.x) && Number.isFinite(point?.y))
      if (path.length === 0) return { erased: [], pageStatus: page.status }
      const rx = (Math.max(1, Number(size) || 1) / 2 / (Number(page.width) || 1600)) * 100
      const ry = (Math.max(1, Number(size) || 1) / 2 / (Number(page.height) || 2400)) * 100
      const swept = {
        x0: Math.min(...path.map((point) => point.x)) - rx,
        y0: Math.min(...path.map((point) => point.y)) - ry,
        x1: Math.max(...path.map((point) => point.x)) + rx,
        y1: Math.max(...path.map((point) => point.y)) + ry,
      }
      const erased = []
      for (const entry of maskEditFrame(found, page)) {
        const owner = entry.page
        const changed = []
        for (const region of [...owner.regions]) {
          if (!isErasableLayer(region)) continue
          const box = toAnchor(entry, region.bbox)
          const crosses = box.x < swept.x1 && box.x + box.w > swept.x0 && box.y < swept.y1 && box.y + box.h > swept.y0
          if (!crosses) continue
          const before = snapshot(region)
          const covered = box.x >= swept.x0 && box.x + box.w <= swept.x1 && box.y >= swept.y0 && box.y + box.h <= swept.y1
          if (covered) {
            owner.regions.splice(owner.regions.indexOf(region), 1)
            changed.push({ regionId: region.id, pageIndex: owner.index, before, after: null })
            continue
          }
          region.mask.legacyPatchRevision = `${region.id}-erase-${(mockEraseCount += 1)}`
          changed.push({ regionId: region.id, pageIndex: owner.index, before, after: snapshot(region) })
        }
        if (changed.length === 0) continue
        if (owner.status === 'cleaned' && owner.regions.every((candidate) => !candidate.mask)) owner.status = 'unclean'
        erased.push(...changed.map((layer) => ({ ...layer, pageStatus: owner.status })))
      }
      return { erased, pageStatus: page.status }
    },

    /**
     * The Patch tool's proposed source, as `retouch.rs#propose_at` answers it.
     * The mock has no pixels to match, so its stand-in is the nearest place
     * the selection fits beside itself: one selection-width to the right,
     * else to the left, else below, else above. `null` where none fits, as
     * natively for a selection with no room.
     */
    async proposePatchSource({ chapterId, pageIndex, sourceIndex, sourceSha, painted, stroke } = {}) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === pageIndex)
      if (!page) return null
      if (sourceIndex !== undefined && page.sourceIndex !== sourceIndex) return null
      if (sourceSha !== undefined && page.sourceSha !== sourceSha) return null
      const box = gestureBox(page, stroke, painted, [{ page, top: 0, height: 100, left: 0, width: 100 }])
      if (!box) return null
      const fits = (dx, dy) => box.x + dx >= 0 && box.y + dy >= 0 && box.x + box.w + dx <= 100 && box.y + box.h + dy <= 100
      const offset = [[box.w, 0], [-box.w, 0], [0, box.h], [0, -box.h]].find(([dx, dy]) => fits(dx, dy))
      return offset ? { offset: { x: offset[0], y: offset[1] } } : null
    },

    /**
     * The Gradient tool's Match surroundings, as `retouch.rs#fit_at` answers
     * it. The stand-in fit is a linear dark-to-light gradient straight down
     * the selection, which is the shape of video 2 in the plan.
     */
    async fitGradient({ chapterId, pageIndex, sourceIndex, sourceSha, painted, stroke } = {}) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === pageIndex)
      if (!page) return null
      if (sourceIndex !== undefined && page.sourceIndex !== sourceIndex) return null
      if (sourceSha !== undefined && page.sourceSha !== sourceSha) return null
      const box = gestureBox(page, stroke, painted, [{ page, top: 0, height: 100, left: 0, width: 100 }])
      if (!box) return null
      const x = box.x + box.w / 2
      return {
        kind: 'linear',
        start: [x, box.y],
        end: [x, box.y + box.h],
        colors: [{ at: 0, color: [40, 30, 30] }, { at: 1, color: [225, 225, 225] }],
        opacities: [{ at: 0, opacity: 1 }, { at: 1, opacity: 1 }],
        reverse: false,
        dither: true,
      }
    },

    async editDetectionMask({ chapterId, pageIndex, sourceIndex, sourceSha, mode, stroke, painted } = {}) {
      await delay(timing.method)
      if (mode !== 'add' && mode !== 'remove') throw new Error('mask_edit_mode_invalid')
      const found = findChapter(chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === pageIndex)
      if (!page) return null
      if (sourceIndex !== undefined && page.sourceIndex !== sourceIndex) return null
      if (sourceSha !== undefined && page.sourceSha !== sourceSha) return null
      refuseBusyPage(chapterId, pageIndex)
      const frame = maskEditFrame(found, page)
      const box = gestureBox(page, stroke, painted, frame)
      if (!box) return null
      // Every page the gesture can write, as the native edit records them.
      const captured = capturePages(frame.map((entry) => entry.page))

      /** @type {{pageStatus: string, changed: string[], created: string[], removed: string[], pages: Array<{pageIndex: number, pageStatus: string}>}} */
      const answer = { pageStatus: page.status, changed: [], created: [], removed: [], pages: [] }
      /** @type {Set<any>} the pages written, in frame order */
      const touched = new Set()
      if (mode === 'remove') {
        const covered = (bbox) => {
          const cx = bbox.x + bbox.w / 2
          const cy = bbox.y + bbox.h / 2
          return cx >= box.x && cx <= box.x + box.w && cy >= box.y && cy <= box.y + box.h
        }
        for (const entry of frame) {
          for (const region of detectionsOf(entry.page).filter((candidate) => covered(toAnchor(entry, candidate.bbox)))) {
            entry.page.regions.splice(entry.page.regions.indexOf(region), 1)
            answer.removed.push(region.id)
            touched.add(entry)
          }
        }
      } else {
        let best = null
        let bestEntry = null
        let bestArea = 0
        for (const entry of frame) {
          for (const region of detectionsOf(entry.page)) {
            const area = overlapArea(toAnchor(entry, region.bbox), box)
            if (area > bestArea) {
              best = region
              bestEntry = entry
              bestArea = area
            }
          }
        }
        if (best && bestEntry) {
          const grown = fromAnchor(bestEntry, box)
          best.bbox = unionBox(best.bbox, grown)
          // A padded detection's unpadded box takes the add too, so it
          // outlives a later padding.
          if (best.baseBbox) best.baseBbox = unionBox(best.baseBbox, grown)
          // A new digest and one more edit, as the native mask's would be:
          // the mask changed.
          maskEdits += 1
          if (best.mask?.provenance) best.mask.provenance.mask_sha256 = sha256Hex(`mask-edit:${best.id}:${maskEdits}`)
          if (best.mask) best.mask.sequence = (Number.isInteger(best.mask.sequence) ? best.mask.sequence : 1) + 1
          answer.changed.push(best.id)
          touched.add(bestEntry)
        } else {
          // The page holding the box's top row owns it.
          const owner = frame.find((entry) => box.y >= entry.top && box.y < entry.top + entry.height) ?? frame[0]
          handCounter += 1
          /** @type {any} */
          const region = {
            id: `${owner.page.id}-h${handCounter}`,
            pageId: owner.page.id,
            sourceSha: owner.page.sourceSha,
            bbox: fromAnchor(owner, box),
            kind: 'outside',
            text: '',
            detected: false,
            source: 'hand',
            outcome: 'pending',
            gateSkipCause: null,
            declineReason: null,
            unusuallyLarge: false,
            mask: null,
          }
          owner.page.regions.push(region)
          detectRegion(region, owner.page, {}, 'local')
          // Drawn by a person, not found: the one field that says so.
          region.source = 'hand'
          answer.created.push(region.id)
          touched.add(owner)
        }
      }
      for (const entry of frame.filter((candidate) => touched.has(candidate))) {
        entry.page.status = settledStatus(entry.page)
        answer.pages.push({ pageIndex: entry.page.index, pageStatus: entry.page.status })
      }
      answer.pageStatus = page.status
      recordPageStep(chapterId, DETECTION_LABELS.edit, captured)
      return answer
    },

    /**
     * Grow every detection on a page, or on every page of the chapter when
     * `pageIndex` is absent, to `paddingPx` page pixels past its unpadded
     * mask, as `region.rs#set_detection_padding` does. The mock pads boxes
     * (`padDetection`).
     */
    async setDetectionPadding({ chapterId, pageIndex, regionId, paddingPx } = {}) {
      await delay(timing.method)
      if (!validPadding(paddingPx)) throw new Error('mask_padding_invalid')
      const found = findChapter(chapterId)
      if (!found) return null
      // One page waits for the run to leave it. The whole chapter waits for
      // the run to leave the chapter.
      if (pageIndex == null) {
        if (runner.runFor(chapterId)) throw new Error('run_in_progress')
      } else {
        refuseBusyPage(chapterId, pageIndex)
      }
      const pages = found.chapter.pages.filter((page) => pageIndex == null || page.index === pageIndex)
      if (regionId != null && !pages.some((page) => detectionsOf(page).some((region) => region.id === regionId))) {
        throw new Error('mask_padding_target_invalid')
      }
      // The mock pads boxes and never merges two, so `removed` stays empty.
      /** @type {{changed: string[], removed: string[], pages: number[]}} */
      const answer = { changed: [], removed: [], pages: [] }
      const captured = capturePages(pages)
      for (const page of pages) {
        const changed = detectionsOf(page).filter((region) => (regionId == null || region.id === regionId) && padDetection(region, page, paddingPx))
        if (changed.length === 0) continue
        answer.changed.push(...changed.map((region) => region.id))
        answer.pages.push(page.index)
      }
      recordPageStep(chapterId, DETECTION_LABELS.padding, captured)
      return answer
    },

    /**
     * Deleting a mask deletes the row: the original text comes back and the
     * region goes off the page. A region with no mask is one the Layers panel
     * lists as *unexamined* and the canvas draws a box for, so leaving one
     * behind answered the delete with an empty placeholder in the same place.
     * The native backend keeps the record on disk, invisible, so undo can put
     * it back - the mock's undo is the snapshot the caller kept, which is the
     * same round trip through `restoreRegion`.
     *
     * `region: null` is therefore the answer, and the page's status is the half
     * that carries information: a region-level edit can move the page it is on
     * - cleaning a gate-skipped region makes an unclean page a cleaned one, and
     * deleting the last mask on a page takes that back - and a caller that
     * replaced only the region would show a full track under a "not cleaned"
     * mark, or the reverse.
     */
    async duplicateRegion({ regionId }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found || !isRegionCopy(found.region)) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      const copied = snapshot(found.region)
      handCounter += 1
      copied.id = `${found.page.id}-h${handCounter}`
      copied.mask.id = `${copied.id}-m1`
      copied.mask.regionId = copied.id
      copied.mask.order = Math.max(0, ...found.page.regions.map((region) => region.mask?.order ?? 0)) + 1
      const layer = layerOf(copied)
      const centerX = ((copied.bbox.x + copied.bbox.w / 2) / 100) * found.page.width
      const centerY = ((copied.bbox.y + copied.bbox.h / 2) / 100) * found.page.height
      const step = (center, extent) => Math.round(Math.max(0, Math.min(extent, center + (center + 16 > extent ? -16 : 16))) - center)
      copied.mask.layer = sanitizeLayer({ ...layer,
        offsetX: layer.offsetX + step(centerX, found.page.width),
        offsetY: layer.offsetY + step(centerY, found.page.height),
      })
      copied.bbox = displayBbox(copied.mask.sourceBbox ?? found.region.bbox, copied.mask.layer, found.page)
      found.page.regions.push(copied)
      return { region: snapshot(copied), pageStatus: found.page.status }
    },

    async deleteMask({ maskId }) {
      await delay(timing.method)
      const found = findMask(maskId)
      if (!found) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      // A detection has no pixels to put back: it and its mask simply go, and
      // its page as it was is the undo step.
      const detection = isDetection(found.region)
      const captured = detection ? capturePages([found.page]) : []
      const { notices } = deleteRegionMask(found.region)
      if (!detection) notifyAll(notices)
      const index = found.page.regions.findIndex((r) => r.id === found.region.id)
      if (index >= 0) found.page.regions.splice(index, 1)
      if (found.page.status === 'detected') found.page.status = settledStatus(found.page)
      // Nothing on this page is cleaned any more, so it is not a cleaned page.
      if (found.page.status === 'cleaned' && found.page.regions.every((r) => !r.mask)) {
        found.page.status = 'unclean'
      }
      if (detection) recordPageStep(found.chapter.id, DETECTION_LABELS.delete, captured)
      return { region: null, pageStatus: found.page.status }
    },

    /**
     * Remove a row with no mask, as `library.rs#dismiss_region` does, and
     * record it as one step with its page as it was. `null` for a region with
     * a mask, which `deleteMask` takes.
     */
    async dismissRegion({ regionId }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found || (found.region.mask && !isDetection(found.region))) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      const captured = capturePages([found.page])
      found.page.regions.splice(found.page.regions.indexOf(found.region), 1)
      found.page.status = settledStatus(found.page)
      recordPageStep(found.chapter.id, DETECTION_LABELS.dismiss, captured)
      return { region: null, pageStatus: found.page.status }
    },

    /**
     * Puts a region back exactly as the caller last saw it. Undo needs a route
     * back through the seam, not only in the interface's own copy: after a
     * deleted mask is restored, `rerunMask` must be able to find it again, and
     * a re-run undone must leave the *previous* mask addressable. One method
     * covers delete, re-run and clean-anyway because all three are edits to one
     * region and all three are reversed by restoring it.
     *
     * *Exactly* as the caller last saw it: the stored region is replaced
     * wholesale rather than merged over. A merge restores only the keys the
     * snapshot happens to carry, so any key a forward edit added would survive
     * its own undo.
     *
     * `pageStatus` is the other half of the snapshot - see `deleteMask`. It is
     * optional, so a caller reversing an edit that cannot move the page need
     * not carry it.
     *
     * **A region that was not there is a state a snapshot can hold.** Undoing a
     * hand-drawn mask means removing the region the gesture created, and
     * redoing it means putting that exact region back - so `region: null`
     * removes, and a snapshot whose region is gone is re-inserted on the page
     * its `pageId` names. That keeps a creation's undo pair the same shape as
     * every other edit's: two snapshots, one call in each direction, no way for
     * redo to drift from undo. The method's signature is unchanged; only the
     * domain of `region` widens to include "nothing".
     *
     * Silent by design: the undo control is the feedback, and a notice per
     * undo would bury the ones that carry information.
     */
    async restoreRegion({ regionId, region, pageStatus }) {
      await delay(timing.method)
      const found = findRegion(regionId)

      if (!region) {
        if (!found) return null
        refuseBusyPage(found.chapter.id, found.page.index)
        const index = found.page.regions.findIndex((r) => r.id === regionId)
        found.page.regions.splice(index, 1)
        if (pageStatus) found.page.status = pageStatus
        return null
      }

      const page = found?.page ?? findPage(region.pageId)
      if (!page) return null
      const chapter =
        found?.chapter ??
        state.projects.flatMap((project) => project.chapters).find((c) => c.pages.includes(page))
      if (chapter) refuseBusyPage(chapter.id, page.index)
      const index = page.regions.findIndex((r) => r.id === regionId)
      if (index >= 0) page.regions[index] = structuredClone(region)
      else page.regions.push(structuredClone(region))
      if (pageStatus) page.status = pageStatus
      return snapshot(page.regions.find((r) => r.id === regionId))
    },

    /**
     * The replay of a `clean-state` undo entry, as `library.rs`
     * `set_region_cleaned` answers it: the detected region with its mask
     * (`cleaned: false`) or the saved result with no render (`cleaned: true`).
     * Writes no undo step. Asked for the state the region is in already, it
     * answers the region and changes nothing. `null` when no chapter names the
     * region; refused with `clean_not_restorable`, `clean_no_result` or
     * `clean_no_patch`, or while a run is on the page. The mock keeps no mask
     * files, so nothing here answers `clean_mask_unreadable`.
     */
    async setRegionCleaned({ regionId, cleaned }) {
      await delay(timing.method)
      const moved = moveRegionCleaned(regionId, cleaned === true)
      return moved ? { region: snapshot(moved.region), pageStatus: moved.page.status } : null
    },

    /**
     * Reject a generated result, as `library.rs` `reject_result` does: the
     * region is detected again with its mask, and the reject is one undo step
     * this side records itself. A region that is not a cleaned result on its
     * page is refused, and nothing is recorded.
     */
    async rejectResult({ regionId }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found) return null
      if (!found.region.mask || isDetection(found.region)) {
        throw new Error(`clean_no_patch: region ${regionId} has no cleaned result to take back`)
      }
      const before = { present: true, pageStatus: found.page.status, region: snapshot(found.region) }
      const moved = moveRegionCleaned(regionId, false)
      if (!moved) return null
      const after = { present: true, pageStatus: moved.page.status, region: snapshot(moved.region) }
      const history = recordCleanStep(moved.chapter.id, REJECT_LABEL, regionId, before, after)
      return { region: snapshot(moved.region), pageStatus: moved.page.status, history }
    },

    /**
     * A model result's finishing previews, as `refinish.rs#previews_at` makes
     * them: one per method, from the answer a cloud attempt kept or a LaMa or
     * FLUX result's model asked again. Any other layer answers `notCloud` and
     * no previews; an attempt id that starts `att-gone` stands in for one
     * whose answer was dropped.
     */
    async regionFinishingPreviews({ regionId }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found?.region?.mask || isDetection(found.region)) {
        throw new Error(`refinish_no_patch: region ${regionId} has no cleaned result`)
      }
      const provenance = found.region.mask.provenance ?? {}
      const none = (unavailable) => ({ regionId, unavailable, current: null, grayscale: false, previews: [] })
      const unavailable = unfinishable(provenance)
      if (unavailable) return none(unavailable)
      const postprocess = provenance.params_snapshot?.postprocess
      const current = typeof postprocess?.finishing === 'string'
        ? { finishing: postprocess.finishing, grayscale: postprocess.grayscale === true }
        : null
      return {
        regionId,
        unavailable: null,
        current,
        grayscale: current?.grayscale ?? false,
        previews: FINISHINGS.map((finishing, index) => ({ finishing, url: finishingPreviewUrl(index) })),
      }
    },

    /**
     * Finish a model result again, as `refinish.rs#apply_at` does: its
     * post-processing record changes, everything else of it is kept, and the
     * change is one `region-state` undo step this side records itself. The
     * finishing it has already writes nothing.
     */
    async setRegionFinishing({ regionId, finishing, grayscale }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found?.region?.mask || isDetection(found.region)) {
        throw new Error(`refinish_no_patch: region ${regionId} has no cleaned result`)
      }
      refuseBusyPage(found.chapter.id, found.page.index)
      if (!FINISHINGS.includes(finishing)) throw new Error(`unknown finishing ${finishing}`)
      const provenance = found.region.mask.provenance
      const unavailable = unfinishable(provenance ?? {})
      if (unavailable === 'notCloud') throw new Error('refinish_not_cloud: the region is not a model\'s result')
      if (unavailable === 'noResult') throw new Error('refinish_no_result: the saved cloud answer is gone')
      const was = provenance.params_snapshot?.postprocess
      if (was?.finishing === finishing && (was?.grayscale === true) === (grayscale === true)) {
        return { region: snapshot(found.region), pageStatus: found.page.status, history: null }
      }
      const before = { present: true, pageStatus: found.page.status, region: snapshot(found.region) }
      provenance.params_snapshot = {
        ...(provenance.params_snapshot ?? {}),
        postprocess: { ...(was ?? {}), finishing, grayscale: grayscale === true },
      }
      const after = { present: true, pageStatus: found.page.status, region: snapshot(found.region) }
      const history = recordRegionStep(found.chapter.id, FINISHING_LABEL, regionId, before, after)
      return { region: snapshot(found.region), pageStatus: found.page.status, history }
    },

    /**
     * One layer's opacity, placement or lock, judged by the capabilities the
     * native command enforces (`model/layers.js` is this side's copy of them):
     * a fixed redraw moved, a locked layer moved or a detection styled is
     * rejected with the refusal's catalogue key, and nothing changes. A moved
     * layer's `bbox` becomes the box it is drawn in, and `mask.sourceBbox`
     * keeps the one it was made in, as the native answer does.
     */
    async setLayerStyle({ regionId, layer }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      const { region, page } = found
      if (!region.mask) throw new Error('masks.refused.noOutput')
      const next = editedLayer(layerOf(region), layer ?? {}, layerCapabilities(region))
      const source = region.mask.sourceBbox ?? { ...region.bbox }
      region.mask.sourceBbox = source
      region.mask.layer = next
      region.bbox = displayBbox(source, next, page)
      return snapshot(region)
    },

    async keepDependencyResult({ regionId }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found?.region?.mask?.dependencyReview) return null
      found.region.mask.dependencyReview = null
      return snapshot(found.region)
    },

    /**
     * A detection's text type, as `library.rs#retype_detection` sets it: the
     * balloon answer flips, and a Fill or Solid pick moved outside a balloon
     * becomes LaMa, with the rung the row's mask shows following it. The mock
     * stores no balloon colour, so there is none to clear. Refused, as
     * natively, for an id that is not a stored detection, and while a run is
     * on its page, as a mask edit is.
     */
    async setDetectionType({ regionId, inside }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found || !isDetection(found.region)) throw new Error(`not_a_detection: ${regionId} is not a stored detection`)
      refuseBusyPage(found.chapter.id, found.page.index)
      const { region } = found
      const captured = capturePages([found.page])
      if (region.insideBubble !== inside) {
        region.insideBubble = inside
        if (!inside && (region.pick === 'fill' || region.pick === 'solid')) {
          region.pick = 'lama'
          if (region.mask) {
            region.mask.fillMode = ENGINE_INFO.lama.fillMode
            if (region.mask.provenance) region.mask.provenance.engine = 'lama'
          }
        }
      }
      recordPageStep(found.chapter.id, DETECTION_LABELS.type, captured)
      return snapshot(region)
    },

    async rerunMask({ maskId, kind, engine, params }) {
      await delay(timing.method)
      const found = findMask(maskId)
      if (!found) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      // With a grant the region renders in the cloud; one that does not commit
      // answers null, its code on the `cloud://attempt` event.
      if (typeof params?.grantNonce === 'string') {
        if (cloudBlocked()) return null
        const ended = await renderWithGrant(params, found, (cloud) => commitCloudResult(found, cloud))
        if (!ended.mask) return null
        return {
          region: snapshot(found.region),
          mask: snapshot(ended.mask),
          reopenTool: null,
          pageStatus: found.page.status,
        }
      }
      // No grant, and a re-run that would need the cloud: Clean with > Cloud,
      // or a patch the cloud rendered run again as what it was (Try again, a
      // step that lands where it is). `region.rs#rerun_mask` refuses it
      // rather than run it here, as blocked while cloud is off, so the
      // interface asks for consent first. A cloud patch re-run with a local
      // engine is that choice, and runs here.
      if (rerunNeedsCloud(found.region.mask, kind, engine)) {
        if (!cloudBlocked()) notify('notice.mask.rerunFailed', { reasonKey: 'decline.reason.rungUnavailable' }, 'warn')
        return null
      }
      const result = cleaningHere(found, () => rerunRegionMask(found.region, toolContext, { kind, engine }))
      notifyAll(result.notices)
      return {
        region: snapshot(found.region),
        mask: snapshot(result.mask),
        reopenTool: result.reopenTool,
        pageStatus: found.page.status,
      }
    },

    /**
     * Rung 3a is a native sidecar and the mock has no process to ask, so the
     * dev backend answers the honest thing about itself: not available, and
     * nothing to say about why. The picker's flux entry is therefore not shown
     * under `npm run dev`, which is the same answer a machine with nothing
     * installed gets.
     */
    async sidecarAvailable() {
      await delay(timing.method)
      return { available: false, reasonKey: null }
    },

    async listSidecarModels() {
      await delay(timing.method)
      return []
    },

    async listLocalModels() {
      await delay(timing.method)
      return [
        { id: 'flux2-klein-4b', label: 'FLUX.2 Klein 4B', legacy: false, bytes: 4_620_000_000, installed: false },
        { id: 'flux3-image-klein', label: 'FLUX 3 Image Klein', legacy: false, bytes: 6_650_397_934, installed: false },
        { id: 'qwen-image-edit-2511', label: 'Qwen-Image-Edit-2511', legacy: false, bytes: 18_162_784_045, installed: false },
        { id: 'flux2-klein-9b', label: 'FLUX.2 Klein 9B', legacy: true, bytes: 12_606_105_778, installed: false },
      ]
    },

    async installFluxHelper() {
      throw new Error('Downloading a local model requires the desktop app')
    },

    /**
     * The loaded-models tab's poll.
     *
     * **No `delay`.** Every other method here imitates a backend that has work
     * to do; this one imitates a walk over four rows under a mutex, and a
     * simulated 140 ms on a call the interface makes every couple of seconds
     * would be inventing a cost the real command does not have.
     */
    async listLoadedModels() {
      if (!runner.isRunning()) return []
      return MOCK_MODELS.filter((model) => !unloadedModels.has(model.id)).map((model) => ({
        ...model,
        idleMs: 0,
        unloading: false,
      }))
    },

    /**
     * The mock has no session to drop, so the row simply goes - which is what
     * the real backend arrives at one region boundary later. `false` for a row
     * that has already gone, the same answer `src-tauri/src/models.rs` gives.
     */
    async unloadModel({ id }) {
      const known = MOCK_MODELS.some((model) => model.id === id)
      if (!known || unloadedModels.has(id) || !runner.isRunning()) return false
      unloadedModels.add(id)
      return true
    },

    /**
     * The accelerator setting, on a machine the mock does not have.
     *
     * A **fixed** answer, and deliberately the honest shape of one: the mock
     * has no ONNX Runtime to ask, so rather than inventing a machine's worth of
     * availability it reports one particular machine in full. Every provider
     * the real backend can name is a row, so a panel written against this one
     * is written against the real shape. No `delay`, for `listLoadedModels`'s
     * reason.
     *
     * **The machine is a Windows one, and that is the whole point of the
     * choice.** It used to be the Apple machine this project was measured on,
     * where nothing is unmeasured, nothing is declined and no provider is
     * forced - so `noteKey` and `declinedKey` were null on every row and the
     * two branches of the Acceleration panel that render them could not be seen
     * at all. Those branches are where every Windows behaviour lives, this
     * repository cannot execute on Windows, and a browser run against this mock
     * is therefore the only place the behaviour can be looked at before it
     * ships. So the machine here has:
     *
     *   - **DirectML, forced and unmeasured.** The timings are Apple's; on
     *     Direct3D the choice rests on how the provider works rather than on
     *     anything anyone timed, which is exactly what `accel.chosen.unmeasured`
     *     says and what `measured: false` on every row here means.
     *   - **CUDA in the build and not on the machine.** A different remedy from
     *     "not in this runtime" - an install rather than a download - and the
     *     picker's disabled entries are the only place a user reads it.
     *   - **A provider refused for want of memory, with the two figures.** The
     *     inpainter wants more than this machine has room for, so the forced
     *     provider is declined and it lands on the CPU. The byte counts travel
     *     beside the key, and the panel formats them.
     *   - **A provider refused for being the wrong shape.** The language
     *     checker would be split across DirectML and run slower, so it is
     *     declined too - a decline with a reason and no figures, which is the
     *     other half of that branch.
     */
    async listAccelerators() {
      // id, available, measured, active, reasonKey when it is not available.
      // Nothing is measured: the timing table was taken on Apple silicon and
      // this machine is not that.
      const providers = [
        ['cpu', true, false, true, null],
        ['coreml', false, false, false, 'accel.declined.unavailable'],
        ['directml', true, false, true, null],
        // In this runtime build, and NVIDIA's own parts are not installed.
        ['cuda', false, false, false, 'accel.declined.missingRuntime'],
        ['tensorrt', false, false, false, 'accel.declined.unavailable'],
        ['rocm', false, false, false, 'accel.declined.unavailable'],
        ['openvino', false, false, false, 'accel.declined.unavailable'],
        // Loaded, usable, and not what this platform's placements pick.
        ['webgpu', true, false, false, null],
        ['xnnpack', false, false, false, 'accel.declined.unavailable'],
      ]
      // A user who went looking for the graphics card, which is what makes the
      // declines below reachable: a decline is a *forced* provider not being
      // used, and `auto` forces nothing.
      const preference = typeof state.settings.accelerator === 'string' ? state.settings.accelerator : 'directml'
      return {
        preference,
        providers: providers.map(([id, available, measured, active, reasonKey]) => ({
          id,
          labelKey: `accel.${id}`,
          available,
          reasonKey,
          measured,
          active,
          selected: id === preference,
        })),
        models: MOCK_PLACED_MODELS.map((model) => placeMockModel(model, preference, providers)),
      }
    },

    /**
     * The CUDA picture for one build, from `mockCudaReport`. `rescan` reads
     * the same fixed machine again, because there is no disk here to change.
     */
    async runtimeReport({ flavour = null, rescan = false } = {}) {
      void rescan
      await delay(timing.method)
      const major = typeof flavour === 'string' && flavour.startsWith('cuda') ? Number(flavour.slice(4)) : null
      return mockCudaReport(Number.isFinite(major) ? major : null)
    },

    async readMeasurement() {
      await delay(timing.method)
      return { saved: structuredClone(mockMeasurement), current: mockMeasurement ? true : null, running: mockMeasuring !== null }
    },

    /**
     * A measurement as a chain of timers: one `measure-progress` per backend
     * timed, then exactly one carrying `done`, then the saved answer - the
     * order `measurement::measure_backends` keeps. Models that are not
     * installed are skipped, as the native side skips them.
     */
    async measureBackends() {
      if (mockMeasuring) throw new Error('notice.measure.running')
      if (runner.isRunning()) throw new Error('notice.measure.busy')
      const plan = MOCK_PLACED_MODELS.filter((model) => model.id !== 'inpainter' || !state.missingModels.has('inpainter'))
      const total = plan.reduce((sum, model) => sum + model.timings.length, 0)
      const run = { cancelled: false }
      mockMeasuring = run
      let step = 0
      try {
        const models = []
        for (const model of plan) {
          const results = []
          for (const [backend, medianMs, loadMs, error] of model.timings) {
            if (run.cancelled) throw new Error('notice.measure.cancelled')
            step += 1
            emit({ type: 'measure-progress', model: model.id, backend, step, total, done: false })
            await delay(Math.max(1, Math.round(480 / speed)))
            results.push(medianMs === null
              ? { backend, loadMs: null, firstMs: null, medianMs: null, runs: null, error }
              : { backend, loadMs, firstMs: Math.round(medianMs * 1.4), medianMs, runs: medianMs > 1500 ? 3 : 10, error: null })
          }
          const timed = results.filter((row) => row.medianMs !== null)
          const best = Math.min(...timed.map((row) => row.medianMs))
          const winner = timed.filter((row) => row.medianMs <= best * 1.05).sort((a, b) => a.loadMs - b.loadMs)[0]?.backend ?? null
          models.push({ id: model.id, winner, results })
        }
        mockMeasurement = { version: 1, measuredAt: Date.now(), setup: 'mock', models }
        return { saved: structuredClone(mockMeasurement), current: true, running: false }
      } finally {
        mockMeasuring = null
        emit({ type: 'measure-progress', model: '', backend: '', step: 0, total: 0, done: true })
      }
    },

    async cancelMeasurement() {
      if (!mockMeasuring || mockMeasuring.cancelled) return false
      mockMeasuring.cancelled = true
      return true
    },

    async forgetMeasurement() {
      await delay(timing.method)
      mockMeasurement = null
      return { saved: null, current: null, running: mockMeasuring !== null }
    },

    /**
     * The model catalogue, on a machine that has almost everything.
     *
     * **One row is deliberately missing**, and it is the redraw model. The
     * gating this whole call exists for - an engine whose weights are absent
     * is not offered at all - is invisible in a browser if the mock reports
     * every file present, and an interface nobody can see is an interface
     * nobody checks. So `inpainter` is not installed here, the Layers picker
     * and the Shapes row show two rungs rather than three, and Settings shows
     * one Download button with something to download.
     *
     * The rescue reader's three rows are missing for a different reason: it is
     * optional in the catalogue itself (`requiredBy: []`), nothing is gated on
     * it, and absent is the state every machine starts in. It gates no engine
     * either way, which is the property `featuresFrom` should be seen holding.
     *
     * The sizes and the kind keys are the real ones from
     * `src-tauri/src/weights.rs`; a mock that invented plausible numbers would
     * be a second catalogue to disagree with the first.
     */
    async listModels({ retryStore = false } = {}) {
      await delay(timing.method)
      // `retryStore` is accepted and does nothing here, which is the honest
      // answer rather than a shrug: it asks the native side to offer the token
      // to a credential store that refused once, and a browser
      // has no store to have refused. Named rather than ignored so that the
      // mock's signature is the seam's.
      void retryStore
      return {
        models: MOCK_CATALOGUE.map((model) => ({
          ...model,
          installed: !state.missingModels.has(model.id),
          path: state.missingModels.has(model.id) ? null : `${MOCK_MODELS_DIR}/${model.fileName}`,
          readOnly: false,
          sha256Ok: state.verifiedModels.get(model.id) ?? null,
          downloading: state.downloads.has(model.id),
          partialBytes: state.partials.get(model.id) ?? null,
        })),
        runtime: {
          installed: !state.missingModels.has(MOCK_RUNTIME_ID),
          path: state.missingModels.has(MOCK_RUNTIME_ID) ? null : `${MOCK_RUNTIME_DIR}/libonnxruntime.dylib`,
          readOnly: false,
          downloading: state.downloads.has(MOCK_RUNTIME_ID),
          partialBytes: state.partials.get(MOCK_RUNTIME_ID) ?? null,
          // The machine the mock reports is an Apple silicon one, unless the
          // `machine` knob says otherwise.
          platform: MOCK_MACHINE === 'windows-nvidia' ? 'windows-x64' : 'macos-arm64',
          ...runtimeFlavour(),
          // What is *installed*, which the native side reads from a record the
          // install writes beside the libraries. The mock has
          // one build and installs it, so the two never differ here and the
          // "installed X, chosen Y" line is not drawn in a browser - the same
          // honesty as the flavour picker below.
          ...(state.missingModels.has(MOCK_RUNTIME_ID)
            ? { installedFlavour: null, installedVersion: null }
            : { installedFlavour: MOCK_MACHINE === 'windows-nvidia' ? 'directml' : 'stock', installedVersion: '1.28.0' }),
          // One flavour, because the machine the mock reports is an Apple one
          // and macOS publishes one build. The picker is therefore *not* drawn
          // in a browser, which is the honest thing: a Windows-only control
          // faked here would be a control nobody could check against a real
          // answer.
          flavours: MOCK_MACHINE === 'windows-nvidia' ? MOCK_WINDOWS_FLAVOURS : [
            {
              id: 'stock',
              ortVersion: '1.28.0',
              bytes: MOCK_RUNTIME_BYTES,
              isDefault: true,
              userInstalled: [],
            },
          ],
          available: true,
        },
        modelsDir: MOCK_MODELS_DIR,
        runtimeDir: MOCK_RUNTIME_DIR,
        hasToken: typeof state.settings.hfToken === 'string' && state.settings.hfToken.length > 0,
        // A browser has no credential store, and the mock keeps the token in
        // the same settings object every other preference lives in - which is
        // exactly the `fileNoStore` fallback the native side reports on a build
        // with no keychain, so the mock reports it honestly rather than
        // claiming a keychain it does not have. `fileStoreUnavailable` is the
        // other fallback and the mock can never truthfully give it: there is no
        // store here to have been unreachable.
        tokenStore: 'fileNoStore',
        // And with no unreachable store there is no reason to give for one.
        // `null` is what the native side answers for both other locations too.
        tokenStoreReason: null,
      }
    },

    /**
     * A download, as timers.
     *
     * Progress arrives on the event channel and nowhere else, exactly as the
     * command does it: this resolves the moment the first tick is scheduled,
     * and the row is driven from `model-progress` after that. Cancelling ends
     * it the same way a failure does - one `done` event carrying an error -
     * so the interface has one path back to "not installed" rather than three.
     */
    async downloadModel({ id }) {
      const groupId = Object.keys(MOCK_MODEL_GROUPS).find((candidate) => MOCK_MODEL_GROUPS[candidate].includes(id))
      return groupId ? startModelGroup(groupId) : startDownload(id)
    },

    async downloadModelGroup({ id }) {
      return startModelGroup(id)
    },

    async verifyModelGroup({ id }) {
      const members = MOCK_MODEL_GROUPS[id]
      if (!members) throw new Error(`no such model group: ${id}`)
      await delay(timing.method)
      if (members.some((member) => state.missingModels.has(member))) return false
      for (const member of members) state.verifiedModels.set(member, true)
      return true
    },

    async deleteModelGroup({ id }) {
      return deleteModelGroupFiles(id)
    },

    async cancelDownload({ id }) {
      return cancelDownloadById(id)
    },

    /**
     * The mock owns every copy it reports - `readOnly` is false on every row -
     * so `readOnlyElsewhere` is an answer it can never truthfully give and does
     * not. What it does mirror is the distinction that matters here: a row that
     * was already gone says so instead of failing.
     */
    async deleteModel({ id }) {
      await delay(timing.method)
      if (!MOCK_CATALOGUE.some((model) => model.id === id)) throw new Error(`no such model: ${id}`)
      const groupId = Object.keys(MOCK_MODEL_GROUPS).find((candidate) => MOCK_MODEL_GROUPS[candidate].includes(id))
      if (groupId) return deleteModelGroupFiles(groupId)
      if (state.missingModels.has(id)) return 'notFound'
      state.missingModels.add(id)
      state.verifiedModels.delete(id)
      return 'deleted'
    },

    /**
     * Throw away what a stopped download left, and say whether there was any.
     *
     * The refusal is the one that matters and it is not an error: a transfer in
     * flight is writing into those bytes, so the press answers `false` and the
     * row - refreshed on the same press - shows the download it was competing
     * with.
     */
    async discardPartial({ id }) {
      await delay(timing.method)
      const known = id === MOCK_RUNTIME_ID || MOCK_CATALOGUE.some((model) => model.id === id)
      if (!known) throw new Error(`no such model: ${id}`)
      if (state.downloads.has(id)) return false
      return state.partials.delete(id)
    },

    /** The explicit re-digest. Always agrees with the pin here; the real one may not. */
    async verifyModel({ id }) {
      await delay(timing.method)
      if (state.missingModels.has(id)) return false
      state.verifiedModels.set(id, true)
      return true
    },

    /**
     * Ready to review, as a qualified machine with both graphs imported would
     * be: no model runs here, and the page every analysis answers with is the
     * one `mockreview.js` draws. The runtime and the small RT graph follow the
     * Models section's own rows, so removing either there is felt here.
     */
    async listWorkflowCapabilities() {
      await delay(timing.method)
      const runtimeInstalled = !state.missingModels.has(MOCK_RUNTIME_ID)
      const { sam } = review.models
      const fullRt = !state.missingModels.has('fullRt')
      const backend = (id, note) => ({ id, platform: 'macOS', qualified: true, available: runtimeInstalled,
        selectable: runtimeInstalled, note })
      return { runtimeInstalled, runtimeReady: runtimeInstalled,
        localDetectionReady: runtimeInstalled && fullRt && sam,
        localDetectionNote: 'Mock: the full pipeline is drawn locally; no model runs in this browser.',
        ctdInstalled: !state.missingModels.has('textDetector'), rtInstalled: !state.missingModels.has('balloonDetector'), fullRtInstalled: fullRt,
        fullRtManaged: fullRt, fullRtRevision: '16e8a622f91fabc6b5b65c96d32d1183f8843546',
        fullRtFile: { name: 'detector.onnx', bytes: 168481531, sha256: '065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195' },
        samInstalled: sam, samMemoryReady: state.memoryRoomBytes >= 5_000_000_000, samManaged: sam,
        // This computer's memory, as the native side measures it.
        totalBytes: 16 * 1024 ** 3, roomBytes: state.memoryRoomBytes,
        samRevision: '5dd97423e0fbf2404264979136d47e8101144046',
        samFiles: sam ? [
          { name: 'koharu_samts_encoder.onnx', bytes: 1_448_391, sha256: 'c7b1cc85d2316886db1dfd22886c1988c71c317d4bcd869beca07e8b6c822141' },
          { name: 'koharu_samts_text_head.onnx', bytes: 22_704_641, sha256: 'a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281' },
          { name: 'koharu_samts_encoder.onnx.data', bytes: 1_334_022_144, sha256: 'ff610b34dcde10353f39d2499db7ca669d5f1874c64a9b89587222484dcdebc7' },
        ] : [],
        cooStatus: 'Rights unresolved. No bundled model, download, or desktop execution.',
        samWriteQualified: runtimeInstalled && sam,
        samWriteNote: 'Mock: WebGPU analysis of a PNG page can prepare approved writes.',
        rtBackends: [backend('ort-cpu', 'Mock: a drawn page, no model runs.')],
        samBackends: [backend('ort-cpu', 'Review only.'), backend('ort-webgpu', 'Mock of the qualified GPU path.')] }
    },
    async importFullRt({ sourcePath } = {}) {
      await delay(timing.method)
      if (!String(sourcePath ?? '').endsWith('.onnx')) {
        throw new Error('The Ogkalu comic text & bubble detector (Full) graph size or SHA-256 does not match the pinned manifest')
      }
      review.models.fullRt = true
      state.missingModels.delete('fullRt')
      return true
    },
    async removeFullRt() {
      await delay(timing.method)
      const had = !state.missingModels.has('fullRt')
      review.models.fullRt = false
      state.missingModels.add('fullRt')
      return had
    },
    async importSamTs({ sourceDir } = {}) {
      await delay(timing.method)
      if (!sourceDir) throw new Error('The SAM-TS-L lettering mask graphs are not installed')
      review.models.sam = true
      return true
    },
    async installSamTs() {
      // The native stages in order, the checkpoint moving within its span.
      for (const [step, percent] of [['python', 0], ['environment', 5], ['dependencies', 8], ['weights', 30], ['weights', 57], ['weights', 85], ['source', 86], ['export', 88], ['verify', 95]]) {
        emitTo(samInstallListeners, { step, percent })
        await delay(timing.method)
      }
      review.models.sam = true
      return true
    },
    async removeSamTs() {
      await delay(timing.method)
      const had = review.models.sam
      review.models.sam = false
      return had
    },
    async verifySamTs() {
      await delay(timing.method)
      if (!review.models.sam) throw new Error('The SAM-TS-L lettering mask graphs are not installed')
      return true
    },
    analyzeCapabilities({ sourcePath, workflow, rtProfile, rtBackend, samBackend, requestId } = {}) {
      return pendingAnalysis(requestId, () => analyzePage({
        page: reviewPageFor(1200, 1700), target: { key: `path:${sourcePath}`, chapterId: null, pageIndex: null, pageId: null },
        sourceMime: /\.jpe?g$/i.test(String(sourcePath ?? '')) ? 'image/jpeg' : 'image/png',
        workflow, rtProfile, rtBackend, samBackend,
      }))
    },
    analyzeChapterPage({ chapterId, pageIndex, workflow, rtProfile, rtBackend, samBackend, requestId } = {}) {
      let target
      try {
        target = reviewTarget(chapterId, pageIndex, 'Chapter model analysis currently requires a paginated chapter')
      } catch (error) {
        return Promise.reject(error)
      }
      return pendingAnalysis(requestId, () => analyzePage({
        page: target.drawn, sourceMime: target.sourceMime,
        target: { key: `${chapterId}:${target.page.id}`, chapterId, pageIndex, pageId: target.page.id },
        workflow, rtProfile, rtBackend, samBackend,
      }))
    },
    /** `false` when nothing by that id is running, as the native command answers. */
    async cancelCapabilityAnalysis(requestId) {
      const pending = pendingAnalyses.get(requestId)
      if (!pending) return false
      timers.clearTimeout(pending.timer)
      pendingAnalyses.delete(requestId)
      pending.reject('analysis cancelled')
      return true
    },

    /**
     * The write support W of one component, refused in the order
     * `model_workflows.rs#prepare_component_at` refuses it.
     */
    async prepareComponentWrite({ analysisId, chapterId, pageIndex, componentId, allowOutsideBubbles,
      paddingPx = 0, additions, removals, correctionRevision = 0 } = {}) {
      await delay(timing.method)
      const analysis = review.analysis
      if (!analysis || analysis.id !== analysisId) throw new Error('Analysis expired; analyze the page again')
      if (analysis.remote) throw new Error('Remote analysis is review-only and cannot prepare a component write')
      if (!analysis.eligible) throw new Error('This analysis is not qualified for component writing on this host and runtime')
      if (!allowOutsideBubbles && !analysis.bubbled.has(componentId)) {
        throw new Error('Outside-bubble component is held until explicitly permitted')
      }
      if (!String(componentId).startsWith('sam-')) throw new Error('Only a SAM component can grant write support')
      const pixels = analysis.page.pixels.get(componentId)
      if (!pixels) throw new Error('SAM component is absent from this analysis')
      if (!pixels.length) throw new Error('Empty component has no write support')
      if (analysis.chapterId !== chapterId || analysis.pageIndex !== pageIndex) {
        throw new Error('The selected chapter page does not match the analyzed source')
      }
      const regionId = reviewRegionId(analysis.pageId, componentId)
      const saved = review.saved.get(regionId)
      if (saved && correctionRevision <= saved.correctionRevision &&
          (JSON.stringify(additions ?? null) !== JSON.stringify(saved.additions ?? null) ||
            JSON.stringify(removals ?? null) !== JSON.stringify(saved.removals ?? null))) {
        throw new Error('Mask corrections changed without a new correction revision')
      }
      const { width, height } = analysis.page
      const support = supportOf({ pixels, width, height, paddingPx, additions, removals })
      review.count += 1
      const planId = `mock-plan-${review.count}`
      const supportSha256 = sha256Hex(`support:${componentId}:${support.count}:${support.checksum}:${JSON.stringify(support.bounds)}`)
      const plan = {
        planId,
        componentId,
        bounds: support.bounds ?? { x: 0, y: 0, w: 0, h: 0 },
        supportPixels: support.count,
        supportDataUrl: support.dataUrl ?? '',
        supportSha256,
        supportVersion: 'mask-plan-support-v1',
        planIdentitySha256: sha256Hex(`plan:${planId}:${supportSha256}`),
        paddingPx,
        correctionRevision,
        sourceSha256: analysis.sourceSha256,
        underlaySha256: sha256Hex(`underlay:${analysis.sourceSha256}:${saved?.planRevision ?? 0}`),
        renderVersion: 'bounded-ring-median-v1',
      }
      review.prepared = { ...plan, chapterId, pageIndex, pageId: analysis.pageId, width, height,
        additions: additions ?? null, removals: removals ?? null }
      return snapshot(plan)
    },

    async loadComponentCorrection({ analysisId, componentId } = {}) {
      await delay(timing.method)
      const analysis = review.analysis
      if (!analysis || analysis.id !== analysisId) throw new Error('Analysis expired; analyze the page again')
      if (analysis.remote) throw new Error('Remote analysis is review-only and cannot load a component correction')
      if (!analysis.page.pixels.has(componentId)) throw new Error('SAM component is absent from this analysis')
      const regionId = reviewRegionId(analysis.pageId, componentId)
      const saved = review.saved.get(regionId)
      if (!saved) return null
      const empty = { bounds: { x: 0, y: 0, w: 0, h: 0 }, bits: [] }
      return snapshot({ regionId, additions: saved.additions ?? empty, removals: saved.removals ?? empty,
        paddingPx: saved.paddingPx, correctionRevision: saved.correctionRevision, planRevision: saved.planRevision })
    },

    /**
     * Writes the approved W as the component's one region on the page. A
     * second write of the same component replaces that region, as the native
     * patch revision does.
     */
    async applyComponentWrite({ planId, approvedSupportSha256 } = {}) {
      await delay(timing.method)
      const plan = review.prepared
      if (!plan) throw new Error('Prepared write expired; preview the component again')
      if (plan.planId !== planId || plan.supportSha256 !== approvedSupportSha256) {
        throw new Error('Approval does not match the prepared support raster')
      }
      if (!plan.supportPixels) throw new Error('Approved support raster changed')
      const found = findChapter(plan.chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === plan.pageIndex)
      if (!page) throw new Error('Page is no longer in chapter')
      const regionId = reviewRegionId(page.id, plan.componentId)
      const index = page.regions.findIndex((region) => region.id === regionId)
      if (index >= 0) page.regions.splice(index, 1)
      const percent = (value, total) => Math.round(value / total * 10000) / 100
      const created = createHandRegion(page, toolContext, {
        id: regionId,
        bbox: { x: percent(plan.bounds.x, plan.width), y: percent(plan.bounds.y, plan.height),
          w: percent(plan.bounds.w, plan.width), h: percent(plan.bounds.h, plan.height) },
        tool: 'contentAwareFill',
        params: {},
      })
      const saved = review.saved.get(regionId)
      review.saved.set(regionId, { additions: plan.additions, removals: plan.removals, paddingPx: plan.paddingPx,
        correctionRevision: plan.correctionRevision, planRevision: (saved?.planRevision ?? 0) + 1 })
      review.prepared = null
      return { regionId, region: snapshot(created.region), pageStatus: page.status }
    },

    /**
     * An installed runtime is not a refusal: the only way to change build is to
     * fetch the chosen one over the top of the one that is there, so the press
     * is always honoured. See `download_runtime` in `src-tauri/src/weights.rs`.
     */
    async downloadRuntime() {
      return startDownload(MOCK_RUNTIME_ID, true)
    },

    async deleteRuntime() {
      await delay(timing.method)
      // The native command removes the whole runtimes tree, which holds the
      // `.part` of a transfer in flight - so a running download refuses the
      // press rather than deleting the bytes out from under it.
      if (state.downloads.has(MOCK_RUNTIME_ID)) return 'busy'
      if (state.missingModels.has(MOCK_RUNTIME_ID)) return 'notFound'
      state.missingModels.add(MOCK_RUNTIME_ID)
      return 'deleted'
    },

    async cleanAnyway({ regionId, engine, params }) {
      await delay(timing.method)
      const found = findRegion(regionId)
      if (!found) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      if (typeof params?.grantNonce === 'string') {
        if (cloudBlocked()) return null
        const ended = await renderWithGrant(params, found, (cloud) => commitCloudResult(found, cloud))
        if (!ended.mask) return null
        return { region: snapshot(found.region), mask: snapshot(ended.mask), pageStatus: found.page.status }
      }
      const result = cleaningHere(found, () => cleanRegionAnyway(found.region, found.page, toolContext, engine))
      notifyAll(result.notices)
      return {
        region: snapshot(found.region),
        mask: snapshot(result.mask),
        pageStatus: found.page.status,
      }
    },

    /**
     * A held region stored as a detection, as `region.rs#approve_held_mask`
     * stores it: nothing is cleaned, and the mask a clean would use waits on
     * the page. The mock keeps the region's id, where the native side mints
     * a detection's; a caller reads the id from the answer either way.
     */
    async approveHeldMask({ regionId, paddingPx = 0 } = {}) {
      await delay(timing.method)
      if (!validPadding(paddingPx)) throw new Error('mask_padding_invalid')
      const found = findRegion(regionId)
      if (!found || !['gate-skipped', 'candidate'].includes(found.region.outcome)) return null
      refuseBusyPage(found.chapter.id, found.page.index)
      const captured = capturePages([found.page])
      detectRegion(found.region, found.page, {}, 'local', paddingPx)
      found.page.status = settledStatus(found.page)
      recordPageStep(found.chapter.id, DETECTION_LABELS.approve, captured)
      return { regionId: found.region.id, pageStatus: found.page.status }
    },

    /**
     * The refusals `src-tauri/src/exporting.rs` decides, decided here in the
     * same order and answered in the same shape. A mock that only ever said
     * yes would leave every refusal path in the interface unexercised outside a
     * Tauri window.
     *
     * Two of the backend's refusals are not here: a PSD of an indexed or
     * sub-8-bit page (`refusedLayeredMode`) and of a page over 30 000 px
     * (`refusedLayeredSize`) are decided from the manifest's per-source mode,
     * depth and dimensions, which the mock's pages do not carry.
     *
     * `destinationUnavailable` is decided from a disk the mock does not have,
     * so it is a knob: `?exportDrive=missing` (or `options.exportDrive`) says
     * the chapter's scan folder is on a drive that is not connected. The
     * default destination is then refused, and a named folder is not.
     */
    async planExportChapter(spec) { return this.exportChapter({ ...spec, planning: true }) },

    async exportChapter({
      chapterId,
      format = 'PNG',
      destination = 'new-folder',
      masks = 'flattened',
      layout = 'per-page',
      pages,
      grayscale = false,
      planning = false,
      planRevision,
    }) {
      const found = findChapter(chapterId)
      if (!found) return null

      /** @param {string} reasonKey @param {Object} [params] */
      const refuse = async (reasonKey, params = {}) => {
        await delay(timing.method)
        if (!planning) notify(reasonKey, { format, ...params }, 'warn')
        return { status: 'refused', reasonKey, ...(params.path ? { path: params.path } : {}), actualFormats: [], declared: [], outputs: [] }
      }

      const container = EXPORT_FORMATS[String(format).toUpperCase()]
      if (!container) {
        const lossy = /^(JPE?G|WEBP)$/i.test(String(format))
        const layered = /^PSB$/i.test(String(format))
        return refuse(
          lossy
            ? 'notice.export.refusedLossyFormat'
            : layered
              ? 'notice.export.refusedLayeredFormat'
              : 'notice.export.refusedUnknownFormat',
        )
      }

      // A mask file inside a reader's archive would be read as a page.
      if (masks === 'separate-layer' && container === 'cbz') {
        return refuse('notice.export.refusedMaskLayers')
      }

      const stitched = layout === 'stitched'
      if (stitched && container === 'cbz') return refuse('notice.export.refusedStitchedArchive')
      if (stitched && container === 'psd') return refuse('notice.export.refusedStitchedLayered')
      if (stitched && found.project.mode !== 'longstrip') {
        return refuse('notice.export.refusedStitchPaginated')
      }

      const count = found.chapter.pages.length
      const selected = pages ? [...new Set(pages)].sort((a, b) => a - b) : null
      if (selected && (!selected.length || selected.some((index) => !Number.isInteger(index) || index < 0 || index >= count) || ((stitched || container === 'cbz') && selected.length !== count))) {
        return refuse('notice.export.refusedPageSelection')
      }

      // Two sentinels and an absolute path; a relative one has no directory
      // both ends of the seam would agree it is relative *to*.
      if (destination === 'source-folder') {
        return refuse('notice.export.refusedOverwrite', { path: found.project.sourcePath })
      }
      if (destination !== 'new-folder' && !isAbsolutePath(destination)) {
        return refuse('notice.export.refusedDestination')
      }
      if (destination === 'new-folder' && exportDrive === 'missing') {
        return refuse('notice.export.destinationUnavailable', { path: `${found.project.sourcePath}_cleaned` })
      }

      const revision = JSON.stringify([found.chapter, format, destination, masks, layout, selected, grayscale])
      if (planRevision && planRevision !== revision) return refuse('notice.export.stalePlan')
      if (!planning) await delay(timing.export)
      const folder =
        destination === 'new-folder'
          ? `${found.project.sourcePath}_cleaned/ch${found.chapter.number}`
          : destination
      // Chapter positions, as the backend reports them, not places in the filtered list.
      const written = found.chapter.pages
        .map((page, position) => ({ page, position }))
        .filter(({ page, position }) => page.status !== 'skipped' && (!selected || selected.includes(position)))

      const actualFormats = [format === 'CBZ' ? 'PNG' : format]
      const outputs = written.map(({ page, position }) => ({ page: position, filename: `${page.fileName ?? String(position + 1).padStart(3, '0')}.${actualFormats[0].toLowerCase()}`, format: actualFormats[0] }))
      const details = { actualFormats, declared: [], outputs, revision }
      if (stitched) {
        const path = `${folder}/ch${found.chapter.number}.${container}`
        const count = gutterPixels(written.map(({ page }) => page))
        if (!planning) notify('notice.export.stitched', { count, format: actualFormats.join(' / '), path })
        return { status: planning ? 'planned' : 'exported', fileCount: 1, path, gutterPixels: count, ...details }
      }

      const path = container === 'cbz' ? `${folder}/ch${found.chapter.number}.cbz` : folder
      if (!planning) notify('notice.export.finished', { count: written.length, format: actualFormats.join(' / '), path })
      return { status: planning ? 'planned' : 'exported', fileCount: written.length, path, ...details }
    },

    async readSettings() {
      await delay(timing.method)
      return withoutToken(snapshot(state.settings))
    },

    async writeSettings(patch) {
      await delay(timing.method)
      // As `settings::check_undo_limit`: a refused patch writes nothing.
      if (patch?.undoLimit != null && !validUndoLimit(patch.undoLimit)) {
        throw new Error(`undoLimit must be a whole number from ${UNDO_LIMIT.min} to ${UNDO_LIMIT.max}`)
      }
      Object.assign(state.settings, patch)
      return withoutToken(snapshot(state.settings))
    },

    async readInferenceConfig() {
      await delay(timing.method)
      return snapshot(state.inferenceConfig)
    },

    async writeInferenceConfig({ config }) {
      await delay(timing.method)
      const projected = projectPublicInferenceConfig(config)
      replaceInferenceConfig(projected)
      return snapshot(state.inferenceConfig)
    },

    /** As `select_cloud_profile` in `commands.rs`: only a configured, unpaused profile. */
    async selectCloudProfile({ provider, profileId } = {}) {
      await delay(timing.method)
      if (provider !== 'modal' && provider !== 'beam') throw new Error('invalid provider')
      if (provider === 'beam') throw new Error('provider_paused')
      if (!profileOf(provider, profileId)) throw new Error('profile_missing')
      replaceInferenceConfig(projectPublicInferenceConfig({
        ...state.inferenceConfig, selectedTarget: { type: provider, profile_id: profileId },
      }))
      return snapshot(state.inferenceConfig)
    },

    /**
     * A session-only secret store: it records that a secret was stored and
     * never the secret, so the readiness check and the endpoint list can be
     * exercised in a browser without a value ever sitting in page memory.
     */
    async storeCloudSecret(spec = {}) {
      await delay(timing.method)
      const key = requireSecretSpec(spec)
      if (typeof spec.secret !== 'string' || spec.secret.trim() === '') {
        throw new Error('secret must not be empty')
      }
      state.secrets.add(secretKey(key.provider, key.profileId, key.role))
      return secretSummary(key)
    },

    async deleteCloudSecret(spec = {}) {
      await delay(timing.method)
      const key = requireSecretSpec(spec)
      state.secrets.delete(secretKey(key.provider, key.profileId, key.role))
      return secretSummary(key)
    },

    async getCloudSecretSummary(spec = {}) {
      await delay(timing.method)
      return secretSummary(requireSecretSpec(spec))
    },

    // The mock's secrets never prompt, so there is no cancel to forget.
    async forgetCloudSecretDenials() {
      await delay(timing.method)
    },

    async checkCloudConnection({ provider, profileId }) {
      await delay(timing.method)
      if (!provider || !profileId) {
        throw new Error('provider and profileId are required for checkCloudConnection')
      }
      const profile = provider === 'beam'
        ? state.inferenceConfig.beamProfiles?.[profileId]
        : state.inferenceConfig.modalProfiles?.[profileId]
      if (!profile) {
        throw new Error(`${provider} profile '${profileId}' does not exist in inference configuration`)
      }
      // Ordinary connection check: reachability of control-plane endpoint only.
      // Never triggers GPU work, worker warmup, or model downloads.
      // A profile with no runtime token is not asked at all, as the native check does.
      if (!state.secrets.has(secretKey(provider, profileId, 'runtime'))) {
        return { ok: false, status: 'credential_missing', provider, profileId }
      }
      if (profile.endpointUrl.includes('unreachable') || profile.endpointUrl.includes('offline')) {
        return {
          ok: false,
          status: 'unreachable',
          provider,
          profileId,
          message: 'Endpoint unreachable',
        }
      }
      return {
        ok: true,
        status: 'reachable',
        provider,
        profileId,
        latencyMs: 42,
      }
    },

    /** `get_cloud_gpu_status` (`inference/gpu.rs`), from the `?cloudGpu=` knob. */
    async getCloudGpuStatus({ provider, profileId }) {
      await delay(timing.method)
      requireConfiguredCloud(provider, profileId)
      if (cloudGpu.scenario === 'unsupported' || cloudGpu.scenario === 'outdated') {
        return { supported: false, unsupported: cloudGpu.scenario === 'outdated' ? 'outdated' : 'provider', containers: [] }
      }
      const now = timers.now()
      const containers = []
      const roles = provider === 'modal' && profileId === 'mc-mock-gpu' ? cloudGpu.roles : new Map()
      for (const [role, entry] of roles) {
        // The provider scales an idle container down on its own.
        if (entry.state === 'idle' && now - entry.lastActive >= MOCK_GPU_IDLE_MS) {
          roles.delete(role)
          continue
        }
        containers.push({
          role, gpu: 'L4', state: entry.state, idleSeconds: MOCK_GPU_IDLE_MS / 1000,
          upForMs: now - entry.since,
          scaledownInMs: entry.state === 'idle' ? MOCK_GPU_IDLE_MS - (now - entry.lastActive) : null,
          listPriceUsdPerHour: 0.8,
        })
      }
      return { supported: true, unsupported: null, containers }
    },

    /**
     * `stop_cloud_gpu`: one role's container, or all without `role`. Idempotent,
     * like the gateway route it calls. An unknown role fails, as it fails to
     * deserialize in Rust.
     */
    async stopCloudGpu({ provider, profileId, role }) {
      await delay(timing.method)
      if (role !== undefined && role !== 'render' && role !== 'analysis') throw new Error('invalid_role')
      requireConfiguredCloud(provider, profileId)
      if (cloudGpu.scenario === 'unsupported' || cloudGpu.scenario === 'outdated') {
        return { supported: false, stopped: [], cancelledJobs: 0 }
      }
      const roles = provider === 'modal' && profileId === 'mc-mock-gpu' ? cloudGpu.roles : new Map()
      const stopped = [...roles.keys()].filter((up) => role === undefined || up === role)
      for (const up of stopped) roles.delete(up)
      // A stop cancels the analysis that is out, GPU or no GPU.
      if (cloudGpu.scenario === 'waiting' && role !== 'render') {
        cloudGpu.scenario = 'stopped'
        emitTo(remoteAnalysisListeners, waitingAnalysis({ phase: 'cancelled' }))
      }
      return { supported: true, stopped, cancelledJobs: 0 }
    },

    /**
     * Which cloud code a setup runs. With `?release=old` every setup runs older
     * code until an Update (a Resume) has finished for it.
     */
    async checkCloudRelease({ provider, profileId }) {
      await delay(timing.method)
      const old = (options.releaseScenario ?? new URLSearchParams(globalThis.location?.search ?? '').get('release')) === 'old'
      const expected = 'a'.repeat(64)
      const current = !old || state.updatedReleases.has(installationKey(provider, profileId))
      return { expected, deployed: current ? expected : 'b'.repeat(64), current }
    },

    async getCloudModelInfo({ provider, profileId }) {
      await delay(timing.method)
      if (!provider || !profileId) {
        throw new Error('provider and profileId are required for getCloudModelInfo')
      }
      const profile = provider === 'beam'
        ? state.inferenceConfig.beamProfiles?.[profileId]
        : state.inferenceConfig.modalProfiles?.[profileId]
      if (!profile) {
        throw new Error(`${provider} profile '${profileId}' does not exist in inference configuration`)
      }
      // A setup made with no cleaning model answers as the native side does.
      if (state.installations.get(installationKey(provider, profileId))?.plan.resource_allocation.model_id === 'none') {
        throw new Error('capability_unavailable: gateway render is not configured')
      }
      return {
        supportedProtocolVersion: '1.0.0',
        pinnedModelId: PINNED_CLOUD_MODEL_ID,
        pinnedModelRevision: PINNED_CLOUD_MODEL_REVISION,
        pinnedRecipeId: PINNED_CLOUD_RECIPE_ID,
        preprocessingVersion: CLOUD_PREPROCESSING_VERSION,
        nativeMaskConditioning: false,
        limits: {
          maxDimensions: [2048, 2048],
          maxMegapixels: 4.19,
          maxPngBytes: 16777216,
          maxMultipartBytes: 33554432,
          defaultWorkerDeadlineSec: 120,
        },
      }
    },

    async listRemoteAnalysisCapabilities({ provider, profileId }) {
      await delay(timing.method)
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      if (state.inferenceConfig.selectedTarget?.type !== provider ||
          state.inferenceConfig.selectedTarget?.profile_id !== profileId) throw new Error('analysis_profile_not_active')
      const profiles = provider === 'beam' ? state.inferenceConfig.beamProfiles : state.inferenceConfig.modalProfiles
      if (!profiles?.[profileId]) throw new Error('analysis_profile_missing')
      const capabilities = remoteScenario() === 'missingCapability' ? [] : [
        { capability: 'text_mask_sam_ts@1', graph_sha256s: ['a'.repeat(64), 'b'.repeat(64)], model_revision: 'c'.repeat(40) },
        { capability: 'text_regions_rt@1', graph_sha256s: ['d'.repeat(64)], model_revision: 'e'.repeat(40) },
      ]
      return { protocol_version: ANALYSIS_PROTOCOL_VERSION, capabilities,
        limits: { max_tile_side: 1024, max_tile_pixels: 1048576, max_png_bytes: 4194304,
          max_components: 4096, max_boxes: 4096 } }
    },

    async proposeRemoteAnalysis({ chapterId, pageIndex, provider, profileId, capability, companion = null, regions = [] }) {
      const known = ['text_mask_sam_ts@1', 'text_regions_rt@1']
      if (!known.includes(capability) || (companion != null && (!known.includes(companion) || companion === capability))) {
        throw new Error('capability_unavailable: unsupported analysis capability')
      }
      const available = await this.listRemoteAnalysisCapabilities({ provider, profileId })
      const model = available.capabilities.find((entry) => entry.capability === capability)
      if (!model) throw new Error('capability_unavailable: gateway does not advertise this analysis model')
      const second = companion == null ? null : available.capabilities.find((entry) => entry.capability === companion)
      if (companion != null && !second) throw new Error('capability_unavailable: gateway does not advertise this analysis model')
      const models = [model, ...(second ? [second] : [])].map((entry) => ({ capability: entry.capability,
        graphSha256s: entry.graph_sha256s, modelRevision: entry.model_revision }))
      const { page } = reviewTarget(chapterId, pageIndex, 'Remote analysis currently requires a paginated chapter')
      const selected = regions.length ? regions : [{ x: 0, y: 0, width: page.width, height: page.height }]
      const tiles = []
      // A tile is sent when the part of the page it owns meets a region.
      for (const { tile: rect, core } of cloudTilePlan(page.width, page.height)) {
        if (!selected.some((r) => r.x < core.x + core.width && core.x < r.x + r.width
          && r.y < core.y + core.height && core.y < r.y + r.height)) continue
        const digest = sha256Hex(`${chapterId}:${pageIndex}:${rect.x}:${rect.y}`)
        tiles.push({ rect, pngSha256: digest, encodedBytes: Math.min(4194304, rect.width * rect.height),
          inputSha256: digest, predecessorsSha256: sha256Hex(`underlay:${digest}`) })
      }
      if (!tiles.length) throw new Error('analysis region outside page')
      if (tiles.length > 256) throw new Error('analysis tile count exceeded')
      const proposalId = sha256Hex(`analysis:${chapterId}:${pageIndex}:${Date.now()}:${remoteAnalyses.size}`).slice(0, 32)
      const proposal = { proposalId, chapterId, pageIndex, provider, profileId,
        profileName: (provider === 'beam' ? state.inferenceConfig.beamProfiles : state.inferenceConfig.modalProfiles)[profileId].name,
        capability, graphSha256s: model.graph_sha256s, modelRevision: model.model_revision,
        sourcePageSha256: sha256Hex(`${chapterId}:${pageIndex}:source`),
        underlaySha256: sha256Hex(tiles.map((t) => t.inputSha256).join(':')),
        predecessorsSha256: sha256Hex(tiles.map((t) => t.predecessorsSha256).join(':')),
        projectRevisionSha256: sha256Hex(`${chapterId}:${pageIndex}:project`),
        pageWidth: page.width, pageHeight: page.height, pages: 1,
        totalTilePixels: tiles.reduce((sum, t) => sum + t.rect.width * t.rect.height, 0),
        totalEncodedBytes: tiles.reduce((sum, t) => sum + t.encodedBytes, 0),
        includesSurroundingArt: true, costEstimateUsd: null, tiles, models }
      proposal.issuedAtMs = Date.now()
      proposal.expiresAtMs = proposal.issuedAtMs + 300000
      const record = { proposal, phase: 'proposed', completedTiles: 0, tileSubmissionStarted: false, cancelled: false }
      remoteAnalyses.set(proposalId, record)
      return structuredClone(proposal)
    },

    /**
     * Scenarios: `stale` fails the first tile as the page changing would;
     * `unknown` loses the connection while the second tile is out, leaving it
     * in the state a crash leaves it in; `missingCapability` advertises
     * nothing. The default runs every tile and attaches review-only evidence
     * drawn by `mockreview.js`.
     */
    async confirmRemoteAnalysis({ proposalId, rightsAttested, retentionAcknowledged }) {
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      if (!rightsAttested) throw new Error('rights_attestation_required')
      if (!retentionAcknowledged) throw new Error('retention_acknowledgement_required')
      const record = remoteAnalyses.get(proposalId)
      if (!record) throw new Error('analysis_proposal_missing')
      if (record.phase !== 'proposed') throw new Error('analysis_proposal_consumed')
      if (Date.now() >= record.proposal.expiresAtMs) throw new Error('analysis_proposal_expired')
      const progress = (phase, extra = {}) => {
        record.phase = phase
        if (phase === 'submitted_tile') record.tileSubmissionStarted = true
        record.index = extra.index ?? null
        record.code = extra.code ?? null
        emitTo(remoteAnalysisListeners, statusOf(proposalId, record))
      }
      progress('confirmed')
      const steps = record.proposal.tiles.length * (record.proposal.models?.length || 1)
      for (let index = 0; index < steps; index += 1) {
        if (record.cancelled) { progress('cancelled'); throw new Error('analysis_cancelled') }
        progress('submitted_tile', { index })
        await delay(timing.cloud)
        if (remoteScenario() === 'unknown' && index === 1) {
          progress('unknown_remote_state', { index })
          throw new Error('transport error: connection reset while the tile was in flight')
        }
        if (record.cancelled) { progress('cancelled'); throw new Error('analysis_cancelled') }
        if (remoteScenario() === 'stale') {
          progress('failed', { code: 'analysis_stale' })
          throw new Error('analysis_stale: source or underlay changed during batch')
        }
        record.completedTiles += 1
        progress('result_cached_tile', { index })
      }
      progress('attached_evidence')
      const { proposal } = record
      const found = findChapter(proposal.chapterId)
      const page = found?.chapter.pages.find((candidate) => candidate.index === proposal.pageIndex)
      const drawn = reviewPageFor(proposal.pageWidth, proposal.pageHeight, page?.panels)
      const ids = (proposal.models?.length ? proposal.models : [proposal]).map((entry) => entry.capability)
      const sam = ids.includes('text_mask_sam_ts@1')
      const rt = ids.includes('text_regions_rt@1')
      const label = ids.join('+')
      const analysisId = `remote:${proposal.provider}:${label}:${proposalId.slice(0, 24)}`
      // The native side replaces its one stored analysis with this one, so
      // a plan prepared from a local analysis is gone too.
      review.analysis = { id: analysisId, remote: true, eligible: false, page: drawn,
        sourceSha256: proposal.sourcePageSha256, chapterId: proposal.chapterId, pageIndex: proposal.pageIndex,
        pageId: page?.id ?? null, bubbled: new Set() }
      review.prepared = null
      return { analysisId, sourceSha256: proposal.sourcePageSha256,
        rtModelSha256: null, samEncoderSha256: null, samHeadSha256: null,
        maskSha256: sam ? sha256Hex(`remote-mask:${proposalId}`) : null,
        workflow: label, rtProfile: null,
        rtBackend: rt ? 'remote' : null, samBackend: sam ? 'remote' : null,
        remoteSource: `remote:${proposal.provider}:${label}`, samWriteEligible: false,
        samWebgpuNodes: null, samCpuFallbackNodes: null,
        evidence: evidenceFor(drawn, { rt, sam, cloud: true }),
        sourceDataUrl: drawn.sourceDataUrl, maskDataUrl: sam ? drawn.maskDataUrl : null,
        timingsMs: { rtLoad: 0, rtPage: 0, samLoad: 0, samPrepare: 0, samEncoder: 0, samHead: 0, samRestore: 0 } }
    },

    async cancelRemoteAnalysis({ proposalId }) {
      const record = remoteAnalyses.get(proposalId)
      if (!record) return false
      if (['attached_evidence', 'cancelled', 'failed'].includes(record.phase)) return false
      record.cancelled = true
      if (record.phase === 'proposed') record.phase = 'cancelled'
      return true
    },

    async getRemoteAnalysisStatus({ proposalId }) {
      const record = remoteAnalyses.get(proposalId)
      if (!record) throw new Error('analysis_proposal_missing')
      return statusOf(proposalId, record)
    },

    async proposeRunAnalysis({ chapterId, scope = null, pageIndices = null, capabilities, provider, profileId }) {
      const wanted = [...new Set(capabilities ?? [])].sort()
      if (!wanted.length || wanted.some((id) => !['text_mask_sam_ts@1', 'text_regions_rt@1'].includes(id))) {
        throw new Error('capability_unavailable: unsupported analysis capability')
      }
      // As the native side: a scope is planned the way the run will walk it.
      if (scope != null && scope !== 'page' && scope !== 'chapter') throw new Error('cloud_run_scope_unsupported')
      if (scope === 'page' && pageIndices?.length !== 1) throw new Error('cloud_run_no_pages')
      const planned = scope == null ? (pageIndices ?? [])
        : queueFor({ scope, chapterId, pageIndex: pageIndices?.[0], outsideBubbles: 'review' }).map((entry) => entry.page.index)
      const indices = [...new Set(planned)].sort((a, b) => a - b)
      if (!indices.length) throw new Error('cloud_run_no_pages')
      if (indices.length > 256) throw new Error('cloud_run_too_many_pages')
      const available = await this.listRemoteAnalysisCapabilities({ provider, profileId })
      const models = wanted.map((id) => {
        const entry = available.capabilities.find((candidate) => candidate.capability === id)
        if (!entry) throw new Error('capability_unavailable: gateway does not advertise this analysis model')
        return { capability: id, graphSha256s: entry.graph_sha256s, modelRevision: entry.model_revision }
      })
      let totalTiles = 0
      let totalTilePixels = 0
      let pagePixels = 0
      for (const index of indices) {
        const { page } = reviewTarget(chapterId, index, null)
        const plan = cloudTilePlan(page.width, page.height)
        totalTiles += plan.length
        totalTilePixels += plan.reduce((sum, { tile }) => sum + tile.width * tile.height, 0)
        pagePixels += page.width * page.height
        if (totalTilePixels > 16 * 64 * 1024 * 1024) throw new Error('cloud_run_too_large')
      }
      const profiles = provider === 'beam' ? state.inferenceConfig.beamProfiles : state.inferenceConfig.modalProfiles
      const fingerprint = profiles?.[profileId]?.canonicalOriginFingerprint ?? ''
      const issuedAtMs = Date.now()
      const proposal = {
        proposalId: sha256Hex(`run-analysis:${chapterId}:${indices.join(',')}:${issuedAtMs}:${runProposals.size}`).slice(0, 32),
        chapterId, pageIndices: indices, provider, profileId, profileName: profiles?.[profileId]?.name ?? profileId,
        capabilities: wanted, models, pages: indices.length, totalTiles, totalTilePixels,
        sourceBytes: pagePixels * 3, includesSurroundingArt: true, costEstimateUsd: null,
        issuedAtMs, expiresAtMs: issuedAtMs + 300000,
        standing: standingFor(chapterId, provider, profileId, fingerprint),
      }
      runProposals.set(proposal.proposalId, proposal)
      return structuredClone(proposal)
    },

    async confirmRunAnalysis({ proposalId, rightsAttested, retentionAcknowledged }) {
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const open = runProposals.get(proposalId)
      const fingerprint = open ? profileOf(open.provider, open.profileId)?.canonicalOriginFingerprint ?? '' : ''
      requireStatements(!!open && standingFor(open.chapterId, open.provider, open.profileId, fingerprint),
        rightsAttested, retentionAcknowledged)
      const proposal = runProposals.get(proposalId)
      runProposals.delete(proposalId)
      if (!proposal) throw new Error('analysis_proposal_missing')
      if (Date.now() >= proposal.expiresAtMs) throw new Error('analysis_proposal_expired')
      const grant = { grantId: sha256Hex(`run-grant:${proposalId}`).slice(0, 32), chapterId: proposal.chapterId,
        pageIndices: proposal.pageIndices, capabilities: proposal.capabilities, expiresAtMs: Date.now() + 120000 }
      runGrants.set(grant.grantId, grant)
      recordStanding(proposal.chapterId, proposal.provider, proposal.profileId, fingerprint,
        rightsAttested === true && retentionAcknowledged === true)
      return structuredClone(grant)
    },

    async cancelRunAnalysis({ proposalId }) {
      return runProposals.delete(proposalId)
    },

    /**
     * What a cloud clean would send (docs/detect-clean.md §3): the plan, in
     * order, cut into batches of 256, and the estimate. Nothing is cleaned,
     * written or sent. `localFirst` asks for mixed execution: the start then
     * tries `fill` and `solid` picks here first, and the mock's quality check
     * accepts every second one.
     */
    async prepareCloudClean({ chapterId, scope, pageIndices = null, regionIds = null, localFirst = false,
      bubbleEngine = null, outsideEngine = null }) {
      await delay(timing.method)
      // The native refusals, in the native order and with its codes.
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const target = state.inferenceConfig.selectedTarget
      if (!target || target.type === 'local' || !profileOf(target.type, target.profile_id)) {
        throw new Error('cloud_clean_profile_not_active')
      }
      if (runner.runFor(chapterId)) throw new Error('cloud_clean_run_active')
      const found = findChapter(chapterId)
      if (!found) throw new Error('chapter_not_found')
      /** @type {Array<{region: any, page: any}>} */
      let inScope
      if (scope === 'chapter') {
        inScope = found.chapter.pages.flatMap((page) => detectionsOf(page).map((region) => ({ region, page })))
      } else if (scope === 'page') {
        if (!pageIndices?.length) throw new Error('cloud_clean_no_pages')
        inScope = pageIndices.flatMap((index) => {
          const page = found.chapter.pages.find((candidate) => candidate.index === index)
          if (!page) throw new Error('cloud_clean_page_not_found')
          return detectionsOf(page).map((region) => ({ region, page }))
        })
      } else if (scope === 'regions') {
        if (!regionIds?.length) throw new Error('cloud_clean_no_regions')
        inScope = regionIds.map((id) => {
          const page = found.chapter.pages.find((candidate) => candidate.regions.some((region) => region.id === id && isDetection(region)))
          if (!page) throw new Error('cloud_clean_region_not_found')
          return { region: page.regions.find((/** @type {any} */ region) => region.id === id), page }
        })
      } else {
        throw new Error('cloud_clean_scope_unsupported')
      }
      if (inScope.length > 16384) throw new Error('cloud_clean_too_many_regions')
      // This clean's picks are saved onto the regions first; a side not sent
      // keeps each region's own. The retired `denoise` is saved as `fill`, and
      // so is a region's own pick saved while it existed.
      for (const { region } of inScope) {
        const pick = region.inside === false ? outsideEngine : bubbleEngine
        if (pick) region.pick = currentRung(pick)
        else if (region.pick) region.pick = currentRung(region.pick)
      }
      // A region the render service would refuse is held out, never planned.
      const tooLargeIds = inScope.filter(({ region, page }) => !cloudCropFits(region, page)).map(({ region }) => region.id)
      inScope = inScope.filter(({ region, page }) => cloudCropFits(region, page))
      const profile = profileOf(target.type, target.profile_id)
      const execution = localFirst ? 'mixed' : 'cloud'
      const base = { chapterId, provider: target.type, profileName: profile?.name ?? target.profile_id, localCleaned: 0,
        execution, chunkRegions: 256, unresolvedIds: [], tooLargeIds,
        localCandidates: localFirst ? inScope.filter(({ region }) => region.pick === 'fill' || region.pick === 'solid').length : 0 }
      if (!inScope.length) {
        return { ...base, proposalId: null, regionIds: [], regions: 0, pages: 0, pageIndices: [], totalCropPixels: 0,
          totalWorkPixels: 0, chunks: 0, estimatedGpuSeconds: null, estimatedCostUsd: null, gpu: null, planDigest: null,
          expiresAtMs: 0 }
      }
      if (cleanProposals.size >= 16) throw new Error('cloud_clean_proposal_limit')
      const indices = [...new Set(inScope.map(({ page }) => page.index))].sort((a, b) => a - b)
      const totalCropPixels = inScope.reduce((sum, { region, page }) =>
        sum + (cloudCropPixels(region, page) ?? 0), 0)
      // regions * seconds per region + cold start + idle tail, at an L4's list price.
      const busy = inScope.length * 8
      const seconds = busy + 30 + 120
      const dollars = (seconds / 3600) * 0.8
      const issuedAtMs = Date.now()
      const ids = inScope.map(({ region }) => region.id)
      const proposal = {
        ...base,
        proposalId: sha256Hex(`cloud-clean:${chapterId}:${ids.join(',')}:${issuedAtMs}:${cleanProposals.size}`).slice(0, 32),
        regionIds: ids,
        regions: inScope.length,
        pages: indices.length,
        pageIndices: indices,
        totalCropPixels,
        totalWorkPixels: inScope.length * 768 * 768,
        chunks: Math.ceil(inScope.length / 256),
        estimatedGpuSeconds: { low: busy, high: busy + 655 },
        estimatedCostUsd: { low: Math.round(dollars * 80) / 100, high: Math.round(dollars * 150) / 100 },
        gpu: profile?.gpu ?? 'L4',
        planDigest: sha256Hex(`cloud-clean-plan:${chapterId}:${execution}:256:${ids.join(',')}`),
        expiresAtMs: issuedAtMs + 300000,
        standing: standingFor(chapterId, target.type, target.profile_id, profile?.canonicalOriginFingerprint ?? ''),
      }
      const regionPages = Object.fromEntries(inScope.map(({ region, page }) => [region.id, page.index]))
      cleanProposals.set(proposal.proposalId, { ...proposal, target: { ...target }, regionPages })
      return structuredClone(proposal)
    },

    async confirmCloudClean({ proposalId, planDigest, rightsAttested, retentionAcknowledged }) {
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const open = cleanProposals.get(proposalId)
      const fingerprint = open ? profileOf(open.target.type, open.target.profile_id)?.canonicalOriginFingerprint ?? '' : ''
      // As `confirm` in `cloud_clean.rs`: a plan that sends nothing asks for
      // no statement.
      const sends = !open || Number(open.regions) > 0
      requireStatements(!!open && (!sends || standingFor(open.chapterId, open.target.type, open.target.profile_id,
        fingerprint)),
        rightsAttested, retentionAcknowledged)
      const proposal = cleanProposals.get(proposalId)
      cleanProposals.delete(proposalId)
      if (!proposal) throw new Error('cloud_clean_proposal_missing')
      if (Date.now() >= proposal.expiresAtMs) throw new Error('cloud_clean_proposal_expired')
      // As `confirm` in `cloud_clean.rs`: consent is for the plan it showed,
      // and a stream is not a plan.
      if (proposal.stream || planDigest !== proposal.planDigest) throw new Error('cloud_clean_plan_mismatch')
      if (!profileOf(proposal.target.type, proposal.target.profile_id)) throw new Error('cloud_clean_profile_changed')
      const grant = { grantId: sha256Hex(`cloud-clean-grant:${proposalId}`).slice(0, 32), expiresAtMs: Date.now() + 120000 }
      cleanGrants.set(grant.grantId, { ...grant, chapterId: proposal.chapterId, regionIds: proposal.regionIds, regionPages: proposal.regionPages, target: proposal.target, execution: proposal.execution })
      recordStanding(proposal.chapterId, proposal.target.type, proposal.target.profile_id, fingerprint,
        sends && rightsAttested === true && retentionAcknowledged === true)
      return structuredClone(grant)
    },

    /**
     * The batch, in the ordinary run slot: one `region-done` per rendered
     * region, then the ordinary finish. A region that fails stays detected,
     * with a notice, and is not cleaned here instead.
     */
    async startCloudClean({ grantId }) {
      await delay(timing.method)
      // As `start` in `cloud_clean.rs`: a busy slot is answered with the run
      // in it, before the grant is spent, so the consent can start the batch
      // once that run is over.
      // An unknown grant is refused below, before any busy answer, as natively.
      const pending = cleanGrants.get(grantId)
      if (pending) {
        const own = runner.runFor(pending.chapterId)
        if (own) return { runId: own, pages: [], alreadyRunning: true }
        if (runner.atCapacity()) return { runId: null, pages: [], alreadyRunning: true, atCapacity: true }
      }
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      if (cloudCleanScenario() === 'startFail') throw new Error('credential_missing')
      const grant = cleanGrants.get(grantId)
      cleanGrants.delete(grantId)
      if (!grant) throw new Error('cloud_clean_grant_missing')
      if (Date.now() >= grant.expiresAtMs) throw new Error('cloud_clean_grant_expired')
      if (grant.stream) throw new Error('cloud_clean_grant_mismatch: stream')
      // As `start` in `cloud_clean.rs`: the grant's profile, selected now or not.
      if (!profileOf(grant.target.type, grant.target.profile_id)) throw new Error('cloud_clean_profile_not_active')
      const found = findChapter(grant.chapterId)
      if (!found) throw new Error('chapter_not_found')
      const ids = new Set(grant.regionIds)
      // Every granted page is queued; a region changed or deleted since is
      // left out with the native notice, not failed.
      const granted = new Set(Object.values(grant.regionPages))
      const queue = found.chapter.pages
        .filter((page) => granted.has(page.index))
        .map((page) => ({ projectId: found.project.id, chapterId: grant.chapterId, page }))
      const target = grant.target
      let failed = false
      let stopped = false
      // Mixed only: every second flat-colour pick is settled here, after the start.
      let flat = 0
      return runner.start(queue, {
        chapterId: grant.chapterId,
        mode: 'clean',
        kind: 'cloudClean',
        pick: (page) => {
          for (const id of grant.regionIds) {
            if (grant.regionPages[id] !== page.index) continue
            const region = page.regions.find((candidate) => candidate.id === id)
            if (!region || !isDetection(region)) {
              notify('notice.cloudClean.regionSkipped', { regionId: id, page: page.index + 1, reason: region ? 'changed' : 'gone' }, 'warn')
            }
          }
          return page.regions.filter((region) => ids.has(region.id) && isDetection(region))
        },
        apply: (region, page) => {
          // The knobs: `stop` ends the batch at its first region, as a lost
          // key or a switched-off cloud does; `regionFail` fails that one.
          if (stopped) return false
          if (cloudCleanScenario() === 'stop') {
            stopped = true
            notify('notice.cloudClean.stopped', { page: page.index + 1, code: 'gateway_unauthorized' }, 'warn')
            return false
          }
          if (cloudCleanScenario() === 'regionFail' && !failed) {
            failed = true
            notify('notice.cloudClean.regionFailed', { regionId: region.id, page: page.index + 1, code: 'gateway_unreachable' }, 'warn')
            return false
          }
          if (grant.execution === 'mixed' && (region.pick === 'fill' || region.pick === 'solid') && flat++ % 2 === 1) {
            cleanDetection(region, page)
            return true
          }
          // Committed as every cloud render is here (FLUX, reconstructed, with
          // the cloud record), which is what marks the patch as the cloud's.
          commitCloudResult({ region, page }, cloudRecordFor(cloudAttemptId(`${grantId}:${region.id}`), target))
          delete region.pick
          delete region.detector
          delete region.insideBubble
          return true
        },
        pageStatus: settledStatus,
      })
    },

    async cancelCloudClean({ proposalId }) {
      return cleanProposals.delete(proposalId)
    },

    /**
     * A streaming cloud clean (`cloud_clean.rs#prepare_stream`): the pages a
     * Detect and clean run would clean on the cloud GPU as it detects each,
     * the price, and what a cap must at least be. Nothing is cleaned, written
     * or sent. `?cloudClean=unpriced` answers as a gateway with no price.
     */
    async prepareCloudCleanStream({ chapterId, scope, pageIndices = null, localFirst = false }) {
      await delay(timing.method)
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const target = state.inferenceConfig.selectedTarget
      if (!target || target.type === 'local' || !profileOf(target.type, target.profile_id)) {
        throw new Error('cloud_clean_profile_not_active')
      }
      if (runner.runFor(chapterId)) throw new Error('cloud_clean_run_active')
      const found = findChapter(chapterId)
      if (!found) throw new Error('chapter_not_found')
      let indices
      if (scope === 'chapter') indices = found.chapter.pages.map((page) => page.index)
      else if (scope === 'page') {
        if (!pageIndices?.length) throw new Error('cloud_clean_no_pages')
        if (pageIndices.some((index) => !found.chapter.pages.some((page) => page.index === index))) {
          throw new Error('cloud_clean_page_not_found')
        }
        indices = [...new Set(pageIndices)].sort((a, b) => a - b)
      } else throw new Error('cloud_clean_scope_unsupported')
      if (!indices.length) throw new Error('cloud_clean_no_pages')
      if (cloudCleanScenario() === 'unpriced') throw new Error('cloud_clean_stream_unpriced')
      if (cleanProposals.size >= 16) throw new Error('cloud_clean_proposal_limit')
      const profile = profileOf(target.type, target.profile_id)
      const execution = localFirst ? 'mixed' : 'cloud'
      const issuedAtMs = Date.now()
      const suggested = MOCK_STREAM_FIXED_USD + indices.length * 8 * MOCK_STREAM_REGION_USD
      const proposal = {
        proposalId: sha256Hex(`cloud-clean-stream:${chapterId}:${indices.join(',')}:${issuedAtMs}:${cleanProposals.size}`).slice(0, 32),
        chapterId,
        pages: indices.length,
        pageIndices: indices,
        execution,
        gpu: profile?.gpu ?? 'L4',
        gpuUsdPerHour: 0.8,
        fixedCostUsd: MOCK_STREAM_FIXED_USD,
        regionCostUsd: MOCK_STREAM_REGION_USD,
        suggestedCapUsd: Math.min(100, Math.ceil(suggested * 10) / 10),
        maxCapUsd: 100,
        provider: target.type,
        profileName: profile?.name ?? target.profile_id,
        streamDigest: sha256Hex(`cloud-clean-stream:${chapterId}:${execution}:${indices.join(',')}`),
        expiresAtMs: issuedAtMs + 300000,
        standing: standingFor(chapterId, target.type, target.profile_id, profile?.canonicalOriginFingerprint ?? ''),
      }
      cleanProposals.set(proposal.proposalId, { ...proposal, stream: true, target: { ...target } })
      return structuredClone(proposal)
    },

    async confirmCloudCleanStream({ proposalId, streamDigest, capUsd, rightsAttested, retentionAcknowledged }) {
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const open = cleanProposals.get(proposalId)
      const fingerprint = open ? profileOf(open.target.type, open.target.profile_id)?.canonicalOriginFingerprint ?? '' : ''
      requireStatements(!!open && standingFor(open.chapterId, open.target.type, open.target.profile_id, fingerprint),
        rightsAttested, retentionAcknowledged)
      cleanProposals.delete(proposalId)
      if (!open) throw new Error('cloud_clean_proposal_missing')
      if (Date.now() >= open.expiresAtMs) throw new Error('cloud_clean_proposal_expired')
      if (!open.stream || streamDigest !== open.streamDigest) throw new Error('cloud_clean_plan_mismatch')
      if (!Number.isFinite(capUsd) || capUsd < open.fixedCostUsd || capUsd > open.maxCapUsd) {
        throw new Error('cloud_clean_cap_invalid')
      }
      if (!profileOf(open.target.type, open.target.profile_id)) throw new Error('cloud_clean_profile_changed')
      const grant = { grantId: sha256Hex(`cloud-clean-stream-grant:${proposalId}`).slice(0, 32), expiresAtMs: Date.now() + 120000 }
      cleanGrants.set(grant.grantId, { ...grant, stream: true, chapterId: open.chapterId, pageIndices: open.pageIndices,
        capUsd, target: open.target, execution: open.execution })
      recordStanding(open.chapterId, open.target.type, open.target.profile_id, fingerprint,
        rightsAttested === true && retentionAcknowledged === true)
      return structuredClone(grant)
    },

    /* ---------- page denoise ---------- */

    /** The presets the native side knows: the shared table's ids, targets and recipes. */
    async denoisePresets() {
      await delay(timing.method)
      return DENOISE_PRESETS.map(({ id, targets, recipe }) => structuredClone({ id, targets, recipe }))
    },

    /**
     * One reference page through the local model. The mock's answer is a
     * fixed, plausible CPU figure after a short wait; it needs the model
     * installed, as the native command does.
     */
    async benchmarkDenoiseLocal() {
      await delay(timing.analysis)
      if (denoiseMissing()) throw new Error('denoise_models_missing')
      return { secondsPerPage: 38.4 }
    },

    /**
     * Four tiles a page, a progress event after each, and a stop heard at the
     * next tile, as the native run does. The page in flight when it stops is
     * in neither list. `outputScale` is 1 to the preset's `maxScale`, null or
     * left out for 1.
     */
    async denoiseChapterLocal({ runId, chapterId, pageIndices = null, presetId, outDir, outputScale = null }) {
      if (typeof runId !== 'string' || !runId || denoiseRuns.has(runId)) throw new Error('denoise_run_id_invalid')
      const found = findChapter(chapterId)
      if (!found) throw new Error('chapter_not_found')
      if (!presetById(presetId)?.targets.includes('local')) throw new Error('denoise_preset_unknown')
      const scale = outputScale ?? 1
      if (!outputScalesFor(presetById(presetId)).includes(scale)) throw new Error('denoise_output_scale_invalid')
      if (denoiseMissing()) throw new Error('denoise_models_missing')
      checkOutDir(outDir)
      const pages = denoisePages(found.chapter, pageIndices)
      const run = { stopped: false, kind: /** @type {const} */ ('denoise'), chapterId, done: 0, total: pages.length }
      denoiseRuns.set(runId, run)
      const tick = Math.min(timing.analysis * 2, timing.region * 4 * pages.length) / Math.max(1, pages.length * 4)
      const progress = (done, page) => {
        run.done = done
        emitTo(denoiseProgressListeners, { runId, done, total: pages.length, page })
      }
      try {
        let reached = 0
        for (; reached < pages.length && !run.stopped; reached += 1) {
          for (let tile = 0; tile < 4 && !run.stopped; tile += 1) {
            progress(reached, tile / 4)
            await delay(tick)
          }
          if (!run.stopped) progress(reached + 1, 0)
        }
        const report = denoiseReport(pages.slice(0, reached), outDir, 'denoise_')
        recordDenoised(found.chapter, report, { preset: presetId, target: 'local', scale })
        return run.stopped ? { ...report, cancelled: true } : report
      } finally {
        denoiseRuns.delete(runId)
      }
    },

    async cancelDenoiseLocal({ runId }) {
      const run = denoiseRuns.get(runId)
      if (run) run.stopped = true
      return Boolean(run)
    },

    /**
     * Each page with no cleaning takes its denoised file as its source: a new
     * `sourceSha`, its regions untouched, and the row kept as history, taken.
     * Cleaned pages are kept and listed, and pages with no denoised file
     * stay raw and are listed as missing.
     */
    async replaceWithDenoised({ chapterId }) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      if (!found) throw new Error('chapter_not_found')
      const plan = replacementPlan(found.chapter)
      for (const index of plan.ready) {
        const page = found.chapter.pages.find((/** @type {any} */ entry) => entry.index === index)
        const row = /** @type {MockDenoiseRow} */ (currentDenoised(chapterId, index))
        page.sourceSha = sha256Hex(`denoised:${row.path}:${page.sourceSha}`)
        // `Job::replace_source`: a file saved larger is the page at that
        // size. Boxes here are in page percent, so they grow with it.
        if (row.scale > 1 && page.width && page.height) {
          page.width *= row.scale
          page.height *= row.scale
        }
        row.taken = true
      }
      return { replaced: plan.ready, kept: plan.kept, missing: plan.missing, failed: [] }
    },

    /** `denoise_history`: every run held for the chapter, newest first. */
    async denoiseHistory({ chapterId }) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      if (!found) throw new Error('chapter_not_found')
      const rows = [...(denoiseRows.get(chapterId) ?? [])].sort((a, b) => b.created - a.created || a.pageIndex - b.pageIndex)
      /** @type {Array<any>} */
      const runs = []
      for (const row of rows) {
        let run = runs[runs.length - 1]
        if (run?.created !== row.created) {
          run = { created: row.created, preset: row.preset, scale: row.scale, target: row.target, fromCleaned: false,
            folder: row.path.replace(/[\\/][^\\/]*$/, ''), pages: [] }
          runs.push(run)
        }
        run.fromCleaned ||= row.fromCleaned
        run.pages.push({ pageIndex: row.pageIndex, exists: true, taken: row.taken,
          current: currentDenoised(chapterId, row.pageIndex) === row, fromCleaned: row.fromCleaned })
      }
      return structuredClone(runs)
    },

    /**
     * One side of the compare view, as bytes: only a page and run the history
     * holds, as the native command serves only recorded files.
     */
    async denoiseCompareImage({ chapterId, pageIndex, run, side }) {
      await delay(timing.method)
      const found = findChapter(chapterId)
      const page = found?.chapter.pages.find((/** @type {any} */ entry) => entry.index === pageIndex)
      if (!page) throw new Error('denoise_page_missing')
      const row = (denoiseRows.get(chapterId) ?? []).find((entry) => entry.pageIndex === pageIndex && entry.created === run)
      if (!row) throw new Error('denoise_run_missing')
      if (side !== 'raw' && side !== 'denoised') throw new Error('denoise_side_invalid')
      return compareArt(page, side === 'raw', side === 'raw' ? 1 : row.scale)
    },

    /** The presets the selected deployment advertises. A setup made with Page denoise off has none. */
    async cloudDenoisePresets(explicit = null) {
      await delay(timing.method)
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const target = explicit ? { type: explicit.provider, profile_id: explicit.profileId } : state.inferenceConfig.selectedTarget
      if (!target || target.type === 'local' || !profileOf(target.type, target.profile_id)) {
        throw new Error('cloud_denoise_profile_not_active')
      }
      const setUp = state.installations.get(installationKey(target.type, target.profile_id))
      if (setUp?.stage === 'completed' && setUp.plan.resource_allocation.denoise !== true) {
        throw new Error('capability_unavailable: gateway denoise is not configured')
      }
      return DENOISE_PRESETS.map((preset) => preset.id)
    },

    /**
     * What a cloud denoise would send: pages, models, GPU time and cost. Sends
     * nothing. The preset is found by the recipe's steps, so a recipe that
     * asks for a larger result (`output_scale`) is still its preset's; the
     * scale must be one the preset can give, and never a stated 1, which the
     * gateway refuses. A preset with no measured time gives no estimate.
     */
    async prepareCloudDenoise({ chapterId, pageIndices = null, recipe }) {
      await delay(timing.method)
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const target = state.inferenceConfig.selectedTarget
      if (!target || target.type === 'local' || !profileOf(target.type, target.profile_id)) {
        throw new Error('cloud_denoise_profile_not_active')
      }
      // A mock setup made with Page denoise off serves no denoise route, as the
      // real gateway answers 404. Profiles added by hand are left as they were.
      const setUp = state.installations.get(installationKey(target.type, target.profile_id))
      if (setUp?.stage === 'completed' && setUp.plan.resource_allocation.denoise !== true) {
        throw new Error('capability_unavailable: gateway denoise is not configured')
      }
      const found = findChapter(chapterId)
      if (!found) throw new Error('chapter_not_found')
      const preset = DENOISE_PRESETS.find((entry) => entry.recipe.schema === recipe?.schema
        && JSON.stringify(entry.recipe.steps) === JSON.stringify(recipe?.steps))
      if (!preset) throw new Error('cloud_denoise_invalid_recipe: unknown recipe')
      const scale = recipe.output_scale ?? 1
      if (!outputScalesFor(preset).includes(scale) || recipe.output_scale === 1) {
        throw new Error('cloud_denoise_invalid_recipe: output_scale is not one this recipe can give')
      }
      const pages = denoisePages(found.chapter, pageIndices)
      const totalPixels = pages.reduce((sum, page) => sum + (page.width ?? 0) * (page.height ?? 0), 0)
      const measured = typeof preset.cloudSecondsPerMegapixel === 'number'
      const busy = measured ? (totalPixels / 1e6) * preset.cloudSecondsPerMegapixel : 0
      const profile = profileOf(target.type, target.profile_id)
      const issuedAtMs = Date.now()
      const low = Math.round(busy)
      const high = Math.round(busy * 1.4 + 150)
      const proposal = {
        proposalId: sha256Hex(`cloud-denoise:${chapterId}:${preset.id}:${scale}:${issuedAtMs}:${denoiseProposals.size}`).slice(0, 32),
        chapterId,
        pageIndices: pages.map((page) => page.index),
        pages: pages.length,
        tooLargePages: [],
        totalPixels,
        recipe: structuredClone(recipeAt(preset, scale)),
        modelIds: preset.recipe.steps.map((step) => `${step.engine}.${step.op}.x${step.scale ?? 1}`),
        estimatedGpuSeconds: measured ? { low, high } : null,
        estimatedCostUsd: measured
          ? { low: Math.round((low / 3600) * 0.8 * 100) / 100, high: Math.round((high / 3600) * 0.8 * 100) / 100 }
          : null,
        gpu: profile?.gpu ?? 'L4',
        provider: target.type,
        profileName: profile?.name ?? target.profile_id,
        profileId: target.profile_id,
        planDigest: sha256Hex(`cloud-denoise-plan:${chapterId}:${preset.id}:${scale}:${pages.map((page) => page.index).join(',')}`),
        expiresAtMs: issuedAtMs + 300000,
        standing: standingFor(chapterId, target.type, target.profile_id, profile?.canonicalOriginFingerprint ?? ''),
      }
      denoiseProposals.set(proposal.proposalId, { ...proposal, target: { ...target }, presetId: preset.id, scale })
      return structuredClone(proposal)
    },

    async confirmCloudDenoise({ proposalId, planDigest, rightsAttested, retentionAcknowledged }) {
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const proposal = denoiseProposals.get(proposalId)
      const fingerprint = proposal ? profileOf(proposal.target.type, proposal.target.profile_id)?.canonicalOriginFingerprint ?? '' : ''
      requireStatements(!!proposal && standingFor(proposal.chapterId, proposal.target.type, proposal.target.profile_id, fingerprint),
        rightsAttested, retentionAcknowledged)
      denoiseProposals.delete(proposalId)
      if (!proposal) throw new Error('cloud_denoise_proposal_missing')
      if (Date.now() >= proposal.expiresAtMs) throw new Error('cloud_denoise_proposal_expired')
      if (planDigest !== proposal.planDigest) throw new Error('cloud_denoise_plan_mismatch')
      const grant = { grantId: sha256Hex(`cloud-denoise-grant:${proposalId}`).slice(0, 32), expiresAtMs: Date.now() + 120000 }
      denoiseGrants.set(grant.grantId, { ...grant, chapterId: proposal.chapterId, pageIndices: proposal.pageIndices,
        presetId: proposal.presetId, scale: proposal.scale })
      recordStanding(proposal.chapterId, proposal.target.type, proposal.target.profile_id, fingerprint,
        rightsAttested === true && retentionAcknowledged === true)
      return structuredClone(grant)
    },

    /**
     * A progress event as each page starts, half way through it and after it,
     * and a stop heard between pages: the page in flight is finished and
     * saved, as the native run finishes the pages it has sent. As natively,
     * `page` is 0 after each page and 1 only after the last, so
     * `(done + page) / total` never runs ahead, and the report says
     * `cancelled` only when some page was never started. A caller that names
     * no run gets one named for it, as an older caller did.
     */
    async startCloudDenoise({ grantId, outDir, runId }) {
      checkOutDir(outDir)
      const id = typeof runId === 'string' && runId ? runId : `den-${Date.now().toString(36)}-${denoiseRuns.size}`
      if (denoiseRuns.has(id)) throw new Error('denoise_run_id_invalid')
      if (state.settings.cloudEngines !== 'allowed') throw new Error('cloud_disabled')
      const grant = denoiseGrants.get(grantId)
      denoiseGrants.delete(grantId)
      if (!grant) throw new Error('cloud_denoise_grant_missing')
      if (Date.now() >= grant.expiresAtMs) throw new Error('cloud_denoise_grant_expired')
      const found = findChapter(grant.chapterId)
      if (!found) throw new Error('chapter_not_found')
      const pages = denoisePages(found.chapter, grant.pageIndices)
      const run = { stopped: false, kind: /** @type {const} */ ('cloudDenoise'), chapterId: grant.chapterId, done: 0, total: pages.length }
      denoiseRuns.set(id, run)
      const half = Math.min(timing.cloud * 2, timing.region * 3 * pages.length + timing.cloud) / Math.max(1, pages.length * 2)
      const progress = (done, page) => {
        run.done = done
        emitTo(denoiseProgressListeners, { runId: id, done, total: pages.length, page })
      }
      try {
        let reached = 0
        for (; reached < pages.length && !run.stopped; reached += 1) {
          progress(reached, 0)
          await delay(half)
          progress(reached, 0.5)
          await delay(half)
          progress(reached + 1, reached + 1 === pages.length ? 1 : 0)
        }
        const report = denoiseReport(pages.slice(0, reached), outDir, 'cloud_denoise_')
        recordDenoised(found.chapter, report, { preset: grant.presetId ?? null, target: 'cloud', scale: grant.scale ?? 1 })
        return run.stopped && reached < pages.length ? { ...report, cancelled: true } : report
      } finally {
        denoiseRuns.delete(id)
      }
    },

    async cancelCloudDenoise({ proposalId }) {
      return denoiseProposals.delete(proposalId)
    },

    async prepareCloudConsent(spec) {
      await delay(timing.method)
      const { target, recipe, intent, simulateBlocked } = spec ?? {}
      if (simulateBlocked || state.settings.cloudEngines !== 'allowed') {
        throw new Error('Backend authorization blocked: cloudEngines permission denied')
      }
      if (!target || target.type === 'local') {
        throw new Error('Consent proposal requires a remote execution target (modal or beam)')
      }
      const profile = target.type === 'beam'
        ? state.inferenceConfig.beamProfiles?.[target.profile_id]
        : state.inferenceConfig.modalProfiles?.[target.profile_id]
      if (!profile) {
        throw new Error(`${target.type} profile '${target.profile_id}' does not exist in inference configuration`)
      }

      const proposalId = `prop-${rng.sha256().slice(0, 16)}`
      // A crop around the region, not the page: the bounding box and a margin
      // for context, clamped to the page, the way the native side cuts it.
      const regionFound = spec.regionId ? findRegion(spec.regionId) : null
      if (regionFound && !cloudCropFits(regionFound.region, regionFound.page)) {
        throw new Error('cloud_consent_crop_too_large')
      }
      const cropRect = (() => {
        const bbox = regionFound?.region?.bbox
        if (!bbox) return { x: 0, y: 0, w: 256, h: 256 }
        const margin = 32
        const x = Math.max(0, Math.floor(bbox.x - margin))
        const y = Math.max(0, Math.floor(bbox.y - margin))
        const right = Math.min(regionFound.page.width ?? bbox.x + bbox.w + margin, Math.ceil(bbox.x + bbox.w + margin))
        const bottom = Math.min(regionFound.page.height ?? bbox.y + bbox.h + margin, Math.ceil(bbox.y + bbox.h + margin))
        return { x, y, w: Math.max(1, right - x), h: Math.max(1, bottom - y) }
      })()
      const profileEpoch = state.profileEpochs.get(target.profile_id) ?? 1
      const proposal = {
        proposalId,
        profileId: target.profile_id,
        provider: target.type,
        endpointUrl: profile.endpointUrl,
        canonicalOriginFingerprint: profile.canonicalOriginFingerprint,
        profileEpoch,
        cropSha256: rng.sha256(),
        hintSha256: rng.sha256(),
        sourceHash: rng.sha256(),
        maskHash: rng.sha256(),
        regionRevision: spec.regionRevision ?? 1,
        rect: spec.rect ?? cropRect,
        recipe: recipe ?? {
          recipe_id: PINNED_CLOUD_RECIPE_ID,
          preprocessing_version: CLOUD_PREPROCESSING_VERSION,
          model_id: PINNED_CLOUD_MODEL_ID,
          model_revision: PINNED_CLOUD_MODEL_REVISION,
          native_mask_conditioning: false,
        },
        intent: intent ?? { action: 'applyTool', tool: 'contentAwareFill' },
        createdAtMs: timers.now(),
        expiresAtMs: timers.now() + 300000,
        estimatedCostUsd: null, // Unknown costs stay unknown!
        standing: standingFor(spec.chapterId, target.type, target.profile_id, profile.canonicalOriginFingerprint),
      }

      if (state.cachedProposals.size >= 256) {
        const oldest = state.cachedProposals.keys().next().value
        state.cachedProposals.delete(oldest)
      }
      // The chapter stays here, as the native proposal keeps it: the answer
      // does not carry it.
      state.cachedProposals.set(proposalId, { ...proposal, chapterId: spec.chapterId })
      return snapshot(proposal)
    },

    async confirmCloudConsent({ proposalId, intent, rightsAttested, retentionAcknowledged, simulateEpochMismatch }) {
      await delay(timing.method)
      if (!proposalId) {
        throw new Error('proposalId is required for confirmCloudConsent')
      }
      const proposal = state.cachedProposals.get(proposalId)
      if (!proposal) {
        throw new Error('Proposal not found or expired')
      }
      if (proposal.consumed) {
        throw new Error('Proposal already consumed')
      }
      if (timers.now() > proposal.expiresAtMs) {
        state.cachedProposals.delete(proposalId)
        throw new Error('Proposal expired')
      }

      const currentEpoch = state.profileEpochs.get(proposal.profileId) ?? 1
      if (simulateEpochMismatch || currentEpoch !== proposal.profileEpoch) {
        throw new Error('Profile mutated: epoch mismatch')
      }

      if (intent && JSON.stringify(intent) !== JSON.stringify(proposal.intent)) {
        throw new Error('Intent mismatch')
      }
      requireStatements(standingFor(proposal.chapterId, proposal.provider, proposal.profileId,
        proposal.canonicalOriginFingerprint), rightsAttested, retentionAcknowledged)

      proposal.consumed = true
      const grant = {
        nonce: `grant-${rng.sha256().slice(0, 16)}`,
        scope: {
          provider: proposal.provider,
          profileId: proposal.profileId,
          endpointFingerprint: proposal.canonicalOriginFingerprint,
          cropSha256: proposal.cropSha256,
          maskHash: proposal.maskHash,
          revision: proposal.regionRevision,
          recipe: proposal.recipe,
          operationDigest: rng.sha256(),
        },
        issuedAtMs: timers.now(),
        expiresAtMs: timers.now() + 300000,
        allowedAttempts: 1,
        usedAttempts: 0,
      }

      if (state.cachedGrants.size >= 256) {
        const oldest = state.cachedGrants.keys().next().value
        state.cachedGrants.delete(oldest)
      }
      state.cachedGrants.set(grant.nonce, grant)
      recordStanding(proposal.chapterId, proposal.provider, proposal.profileId, proposal.canonicalOriginFingerprint,
        rightsAttested === true && retentionAcknowledged === true)
      return snapshot(grant)
    },

    async submitCloudAttempt(spec) {
      await delay(timing.method)
      const { attemptId, grantNonce, simulateMode, snapshot: regionSnapshot } = spec ?? {}
      if (!attemptId || typeof attemptId !== 'string') {
        throw new Error('Valid attemptId is required for submitCloudAttempt')
      }

      // Check authorization: non-empty grantNonce present in cachedGrants, fail closed when missing/unknown
      if (
        simulateMode === 'blocked_authorization' ||
        !grantNonce ||
        typeof grantNonce !== 'string' ||
        grantNonce.trim().length === 0 ||
        !state.cachedGrants.has(grantNonce)
      ) {
        throw new Error('Authorization blocked: invalid or missing grant')
      }

      const grant = state.cachedGrants.get(grantNonce)
      if (grant.expiresAtMs && timers.now() > grant.expiresAtMs) {
        state.cachedGrants.delete(grantNonce)
        throw new Error('Authorization blocked: grant expired')
      }

      // Atomically enforce allowedAttempts and increment usedAttempts so replay cannot submit
      if (typeof grant.allowedAttempts === 'number' && (grant.usedAttempts ?? 0) >= grant.allowedAttempts) {
        throw new Error('Authorization blocked: grant attempt limit reached (replay detected)')
      }
      grant.usedAttempts = (grant.usedAttempts ?? 0) + 1

      // Ambiguous acceptance test: transport error during dispatch
      if (simulateMode === 'ambiguous_acceptance') {
        const record = {
          attemptId,
          grantNonce,
          phase: 'unknown',
          handle: null,
          autoRetryable: false,
          snapshot: regionSnapshot ?? { regionRevision: 1, sourceImageHash: 'src-hash' },
          createdAtMs: timers.now(),
        }
        state.attemptJournal.set(attemptId, record)
        return {
          attemptId,
          handle: null,
          status: 'unknown',
          autoRetryable: false,
          error: 'Transport error during dispatch: ambiguous acceptance',
        }
      }

      const handle = `handle-mock-${attemptId}`
      const record = {
        attemptId,
        grantNonce,
        phase: 'accepted',
        handle,
        autoRetryable: false,
        snapshot: regionSnapshot ?? { regionRevision: 1, sourceImageHash: 'src-hash' },
        resultDigest: 'res-digest-' + attemptId,
        status: 'pending',
        reportedCostUsd: null,
        createdAtMs: timers.now(),
      }
      state.attemptJournal.set(attemptId, record)
      return {
        attemptId,
        handle,
        status: 'accepted',
        requestDigest: 'req-digest-' + attemptId,
        autoRetryable: false,
      }
    },

    async getCloudAttemptStatus({ attemptId, handle }) {
      await delay(timing.method)
      let record = null
      if (attemptId && state.attemptJournal.has(attemptId)) {
        record = state.attemptJournal.get(attemptId)
      } else if (handle) {
        for (const r of state.attemptJournal.values()) {
          if (r.handle === handle) {
            record = r
            break
          }
        }
      }
      if (!record) {
        throw new Error(`Attempt '${attemptId ?? handle}' not found in journal`)
      }

      if (record.phase === 'abandoned') {
        return { attemptId: record.attemptId, handle: null, status: 'abandoned', acknowledged: true, reportedCostUsd: null }
      }

      if (record.phase === 'unknown') {
        return {
          attemptId: record.attemptId,
          handle: null,
          status: 'unknown',
          reportedCostUsd: null,
        }
      }

      if (record.phase === 'cancel_requested') {
        // Non-terminal cancellation request
        return {
          attemptId: record.attemptId,
          handle: record.handle,
          status: 'cancel_requested',
          reportedCostUsd: null,
          acknowledged: true,
        }
      }

      // Normal progress simulation
      if (record.status === 'pending') {
        record.status = 'running'
      } else if (record.status === 'running') {
        record.status = 'completed'
        record.phase = 'result_cached'
      }

      return {
        attemptId: record.attemptId,
        handle: record.handle,
        status: record.status,
        reportedCostUsd: null, // Unknown costs stay unknown
        createdAtMs: record.createdAtMs,
      }
    },

    async getCloudAttemptResult({ attemptId, handle }) {
      await delay(timing.method)
      let record = null
      if (attemptId && state.attemptJournal.has(attemptId)) {
        record = state.attemptJournal.get(attemptId)
      } else if (handle) {
        for (const r of state.attemptJournal.values()) {
          if (r.handle === handle) {
            record = r
            break
          }
        }
      }
      if (!record) {
        throw new Error(`Attempt '${attemptId ?? handle}' not found in journal`)
      }

      // Idempotent and retry-safe: can be retrieved repeatedly without creating new jobs
      return {
        attemptId: record.attemptId,
        handle: record.handle ?? `handle-mock-${record.attemptId}`,
        resultDigest: record.resultDigest ?? 'mock-result-digest',
        reportedCostUsd: null,
        width: 256,
        height: 256,
        cached: true,
      }
    },

    async cancelCloudAttempt({ attemptId, handle }) {
      const live = attemptId ? state.cloudJobs.get(attemptId) : null
      if (live) {
        // A result already downloaded is past cancelling, and the native
        // side refuses the cancel as its journal says
        // (`commands.rs#past_cancelling`).
        if (live.phase === 'compositing') throw new Error('cannot cancel attempt: job is already completed')
        // While the result downloads the cancel is asked for and comes too
        // late: the render commits anyway, and its last event says so. That
        // is the late cancel the interface shows as a result, not as
        // cancelled. Before that the render notices at once rather than after
        // its current phase, as the native loop checks its cancel flag
        // between polls.
        if (live.phase !== 'downloading') {
          live.cancelled = true
          if (live.timer !== null) {
            timers.clearTimeout(live.timer)
            live.timer = timers.setTimeout(live.step, 0)
          }
        }
        // `commands.rs#cancel_cloud_attempt` for a render in this process:
        // asked for, not confirmed. How it ended is the `cloud://attempt`
        // event's to say.
        return { handle: '', status: 'cancel_requested', acknowledged: false }
      }
      await delay(timing.method)
      let record = null
      if (attemptId && state.attemptJournal.has(attemptId)) {
        record = state.attemptJournal.get(attemptId)
      } else if (handle) {
        for (const r of state.attemptJournal.values()) {
          if (r.handle === handle) {
            record = r
            break
          }
        }
      }
      if (!record) {
        throw new Error(`Attempt '${attemptId ?? handle}' not found in journal`)
      }

      // Nonterminal cancellation acknowledgement
      record.phase = 'cancel_requested'
      record.status = 'cancel_requested'
      return {
        handle: record.handle ?? `handle-mock-${record.attemptId}`,
        status: 'cancel_requested',
        acknowledged: true,
      }
    },

    async reconcileCloudRecovery(spec = {}) {
      await delay(timing.method)
      if (spec.apply === true) return recoverCloudAttempts()
      const { attemptId, simulateStale, regionRevision, sourceImageHash } = spec

      if (spec.action && (!attemptId || !state.attemptJournal.has(attemptId))) {
        throw new Error('attempt not found')
      }
      let record = null
      if (attemptId && state.attemptJournal.has(attemptId)) {
        record = state.attemptJournal.get(attemptId)
      } else {
        // Pick the most recent attempt in journal if none specified
        for (const r of state.attemptJournal.values()) {
          record = r
        }
      }

      if (!record) {
        return {
          decision: 'terminal',
          message: 'No uncommitted attempts found in journal',
        }
      }

      if (spec.action === 'abandon') {
        if (spec.acceptDuplicateRisk !== true || !['unknown', 'accepted', 'cancel_requested', 'result_cached', 'attachment_pending'].includes(record.phase)) {
          throw new Error('duplicate risk confirmation required for an unresolved attempt')
        }
        record.abandonedEvidence = { ...record }
        record.duplicateRiskAcceptedAtMs = timers.now()
        record.phase = 'abandoned'
        return { decision: 'abandon', attemptId: record.attemptId }
      }
      if (spec.action === 'acknowledge') {
        record.repairAcknowledged = true
        return { decision: 'acknowledge', attemptId: record.attemptId }
      }

      // Ambiguous unknown: crash or disconnect during dispatch -> NEVER auto-retried
      if (record.phase === 'unknown') {
        return {
          decision: 'ambiguous_unknown',
          attemptId: record.attemptId,
          autoRetryable: false,
          message: 'Crash or transport error during dispatch. Remote execution ambiguous; automatic resubmission is forbidden.',
        }
      }

      // Known handle recovery
      if (record.phase === 'accepted' && record.handle) {
        return {
          decision: 'resume_polling',
          attemptId: record.attemptId,
          handle: record.handle,
          message: 'Discovered known accepted handle; resuming status polling without creating a new job.',
        }
      }

      // Nonterminal cancel polling
      if (record.phase === 'cancel_requested' && record.handle) {
        return {
          decision: 'resume_cancel_polling',
          attemptId: record.attemptId,
          handle: record.handle,
          message: 'Discovered cancel-requested attempt; resuming status polling to reconcile terminal state.',
        }
      }

      // Stale attachment check vs result cached
      if (record.phase === 'result_cached' || record.status === 'completed') {
        const storedSnap = record.snapshot ?? {}
        const isStale = simulateStale ||
          (regionRevision !== undefined && storedSnap.regionRevision !== undefined && regionRevision !== storedSnap.regionRevision) ||
          (sourceImageHash !== undefined && storedSnap.sourceImageHash !== undefined && sourceImageHash !== storedSnap.sourceImageHash)

        if (isStale) {
          return {
            decision: 'stale_attachment',
            attemptId: record.attemptId,
            handle: record.handle,
            resultDigest: record.resultDigest,
            message: 'Region revision or source hash drifted since submission. Attachment rejected; validated result retained in cache for inspection.',
          }
        }

        return {
          decision: 'result_cached_ready',
          attemptId: record.attemptId,
          handle: record.handle,
          resultDigest: record.resultDigest,
          message: 'Validated cached result ready for attachment.',
        }
      }

      if (record.phase === 'committed') {
        return {
          decision: 'already_committed',
          attemptId: record.attemptId,
          patchId: record.patchId,
        }
      }

      return {
        decision: 'terminal',
        attemptId: record.attemptId,
        message: 'Attempt reached terminal state.',
      }
    },

    /**
     * The provisioning helper, simulated: the same ops, the same envelope, IC-2
     * progress on timers and the IC-1 answer `provision.rs` gives the webview.
     * Credentials are checked for presence and dropped; nothing keeps them.
     */
    async runCloudProvisioner(spec = {}) {
      await delay(timing.method)
      const { op = 'inspect', provider = 'modal', params = {} } = spec ?? {}
      const requestId = params.request_id
      if (params.simulateMissingHelper) {
        return {
          protocol_version: PROVISION_PROTOCOL,
          request_id: requestId || 'req-missing-helper',
          success: false,
          data: null,
          error: {
            code: 'ERR_PROVIDER_UNAVAILABLE',
            message: 'Cloud provisioner helper binary not found: no executable discovered in MANGA_CLEANER_PROVISIONER_BIN, Tauri resources, or dev environment',
            actionable_guidance: 'Install or package the cloud provisioner helper binary, or set MANGA_CLEANER_PROVISIONER_BIN to its path.',
            remedy_steps: [
              'Set MANGA_CLEANER_PROVISIONER_BIN to the absolute path of the provisioner executable',
              'Ensure Python 3 with the provisioner package is available at repository root during development',
            ],
          },
        }
      }
      if (provider !== 'modal' && provider !== 'beam') {
        return helperFailure(op, requestId, 'ERR_UNSUPPORTED_PROVIDER', `Unsupported provider '${provider}'`)
      }
      // `saved_setup_credential` names a profile whose Modal key this computer
      // kept (`take_setup_key` in `provision.rs`): it stands in for a paste.
      const saved = params.saved_setup_credential
      if (saved !== undefined) {
        if (provider !== 'modal' || params.credentials || typeof saved !== 'string' || !state.secrets.has(secretKey('modal', saved, 'setup'))) {
          return helperFailure(op, requestId, 'ERR_VALIDATION_ERROR', 'No saved Modal key for this setup on this computer.')
        }
      }
      const credentials = saved !== undefined ? { token_id: 'saved-id', token_secret: 'saved-secret' } : params.credentials ?? {}
      const installationId = params.installation_id
      const needsCredentials = ['inspect', 'plan', 'apply', 'resume', 'cleanup_apply'].includes(op)
      if (needsCredentials && !credentialsPresent(provider, credentials)) {
        return helperFailure(op, requestId, 'ERR_VALIDATION_ERROR', 'Missing provider credentials')
      }
      if (needsCredentials && credentialsDenied(credentials)) {
        return helperFailure(op, requestId, 'ERR_ACTIONABLE_MISSING_PERMISSION', 'The token was refused by the provider')
      }
      if (op !== 'inspect' && !isInstallationId(installationId)) {
        return helperFailure(op, requestId, 'ERR_VALIDATION_ERROR', "Missing or invalid parameter: 'installation_id'")
      }
      const key = installationKey(provider, installationId)

      switch (op) {
        case 'inspect':
          return helperSuccess(op, requestId, {
            account_id: 'mock-account',
            workspace_name: 'mock-workspace',
            authenticated: true,
            permissions_granted: ['apps', 'volumes', 'tokens'],
            permissions_missing: [],
            eligible: true,
            actionable_remedy: null,
            platform_supported: true,
            platform_notes: '',
            existing_installations: provider === 'modal' ? existingInstallations() : [],
            existing_installations_complete: provider === 'modal',
          })
        case 'plan': {
          const { plan, error } = planFor(provider, installationId, params.options ?? {})
          if (error) return helperFailure(op, requestId, ...error)
          const existing = state.installations.get(key)
          // A finished installation keeps its plan until an update approves this one.
          if (existing?.stage === 'completed') {
            existing.proposed = plan
            return helperSuccess(op, requestId, plan)
          }
          state.installations.set(key, {
            provider,
            installationId,
            plan,
            stage: existing?.stage === 'completed' ? 'completed' : 'planned',
            done: existing?.done ?? new Set(),
            failedOnce: existing?.failedOnce ?? false,
          })
          return helperSuccess(op, requestId, plan)
        }
        case 'apply':
        case 'resume': {
          const installation = state.installations.get(key)
          if (!installation) {
            return helperFailure(op, requestId, 'ERR_VALIDATION_ERROR', `No plan for installation '${installationId}'`)
          }
          if (op === 'apply' && params.approved_plan_hash !== installation.plan.plan_hash) {
            return helperFailure(op, requestId, 'ERR_UNAPPROVED_PLAN', 'Approved plan hash mismatch')
          }
          // Resume with a newly approved plan updates the options, as the helper does.
          const update = op === 'resume' && typeof params.approved_plan_hash === 'string'
          if (update && params.approved_plan_hash !== installation.proposed?.plan_hash) {
            return helperFailure(op, requestId, 'ERR_UNAPPROVED_PLAN', 'Approved plan hash mismatch')
          }
          if (state.provisionRun) {
            return helperFailure(op, requestId, 'ERR_EXECUTION_FAILED', 'Another setup is already running')
          }
          if (update) {
            installation.plan = installation.proposed
            installation.proposed = null
          }
          const answer = await runInstallation(op, provider, params, installation)
          if (answer?.success && op === 'resume') state.updatedReleases.add(key)
          if (answer?.success && provider === 'modal') {
            const setupKey = secretKey('modal', installationId, 'setup')
            if (saved !== undefined || params.remember_setup_credential === true) state.secrets.add(setupKey)
            else state.secrets.delete(setupKey)
          }
          return answer
        }
        case 'cleanup_plan': {
          const plan = cleanupPlanFor(provider, installationId)
          const installation = state.installations.get(key)
          if (installation) installation.cleanupHash = plan.plan_hash
          return helperSuccess(op, requestId, plan)
        }
        case 'cleanup_apply': {
          const plan = cleanupPlanFor(provider, installationId)
          if (params.approved_cleanup_plan_hash !== plan.plan_hash) {
            return helperFailure(op, requestId, 'ERR_UNAPPROVED_PLAN', 'Approved cleanup plan hash mismatch')
          }
          const hasVolume = plan.resources_to_delete.some((resource) => resource.resource_type === 'volume')
          if (hasVolume && params.confirm_delete_persistent_storage !== true) {
            return helperFailure(op, requestId, 'ERR_VALIDATION_ERROR', 'Deleting the weights volume needs explicit confirmation')
          }
          const run = { op, provider, cancelled: false, timer: null, current: null, done: new Set() }
          state.provisionRun = run
          try {
            const outcome = await walkSteps(run, ['validate', 'cleanup'])
            if (outcome === 'cancelled') return helperFailure(op, requestId, 'ERR_CANCELLED', 'Cleanup was stopped.')
          } finally {
            if (state.provisionRun === run) state.provisionRun = null
          }
          state.installations.delete(key)
          return helperSuccess(op, requestId, {
            installation_id: installationId,
            deleted: plan.resources_to_delete,
            stage: 'cleaned_up',
          })
        }
        case 'forget_credential':
          return helperSuccess(op, requestId, { status: 'forgotten', setup_credential_cleared: true })
        default:
          return helperFailure(op, requestId, 'ERR_UNSUPPORTED_OPERATION', `Unsupported operation '${op}'`)
      }
    },

    /** Stops the running setup; the journal keeps what finished, so Resume picks it up. */
    async cancelCloudProvisioner() {
      const run = state.provisionRun
      if (!run) return { cancelled: false }
      run.cancelled = true
      return { cancelled: true }
    },

    async about() {
      await delay(timing.method)
      return aboutInfo()
    },

    /** `src-tauri/src/devlog.rs` reads its file; the mock has a few lines to show. */
    async readDevLog() {
      await delay(timing.method)
      return devLog
    },

    async clearDevLog() {
      await delay(timing.method)
      devLog = ''
    },

    /**
     * `src-tauri/src/diagnostics.rs` loads the runtime to answer; the mock has
     * nothing to load, so an installed runtime loads and a deleted one is
     * `missing`, as there. `?runtimeLoad=` names a load failure instead -
     * `unloadable`, `quarantined`, `refused` or `missingDependency` - because
     * a browser is the only place the readiness row that says so can be seen
     * before it ships, and this machine never fails that way.
     */
    async diagnostics() {
      await delay(timing.method)
      const failures = {
        unloadable: 'diagnostics.runtime.unloadable',
        quarantined: 'diagnostics.runtime.quarantined',
        refused: 'diagnostics.runtime.refused',
        missingDependency: 'diagnostics.runtime.missingDependency',
      }
      const knob = new URLSearchParams(globalThis.location?.search ?? '').get('runtimeLoad') ?? ''
      const failure = Object.hasOwn(failures, knob) ? failures[/** @type {keyof typeof failures} */ (knob)] : null
      const installed = !state.missingModels.has(MOCK_RUNTIME_ID)
      return {
        appVersion: aboutInfo().appVersion,
        components: [{
          name: 'onnxruntime',
          available: installed && !failure,
          detail: null,
          reasonKey: installed ? failure : 'diagnostics.runtime.missing',
        }],
      }
    },
  }
}
