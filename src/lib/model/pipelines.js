/**
 * The two pipelines a page goes through, the engines each one offers, and the
 * capability graph Settings and setup draw the models from.
 *
 * **Detection** finds the text to remove. **Cleaning** redraws what was under
 * it. Onboarding and Settings both draw from this one module, so the two can
 * never disagree about which choice needs which files.
 *
 * `files` are catalogue ids from `src-tauri/src/weights.rs`, written out
 * because an engine is a *combination* of files and the catalogue's
 * `requiredBy` names features, not combinations. An engine is
 * `ready` when the app can download and run it today; the rest are listed so
 * the choice reads as a roadmap, and are drawn disabled.
 *
 * **What a choice needs follows the workflow, not a universal list.** Legacy
 * script filtering needs its detector files, the script gate pair and, only
 * when the optional rescue is on and Japanese is cleaned, the three manga-ocr
 * files. The all-text policy never needs the gate or OCR. See `filesFor`.
 *
 * **Model names are data, not copy.** The four detection choices are named
 * once, in `model-names.js#DETECTOR_MODEL_NAMES`, by the ids sessions store.
 *
 * `rating` is two scores out of 5, higher is better on both:
 *  - `efficiency` - speed per page for the quality it gives
 *  - `light` - how little disk and memory it needs
 * These are provisional: derived from file sizes and the per-page timings in
 * `docs/findings.md`, not from a common benchmark. See "What has not been
 * measured" there. Every table that draws them says so beside the stars.
 */

import { DETECTOR_MODEL_NAMES } from './model-names.js'

/** The source languages the detection pipeline cleans. Latin text is left alone by design. */
export const LANGUAGES = Object.freeze([
  { id: 'ja', labelKey: 'pipelines.language.ja' },
  { id: 'zh', labelKey: 'pipelines.language.zh' },
  { id: 'ko', labelKey: 'pipelines.language.ko' },
])

/** The legacy script gate: the model and the labels it must match. One logical capability. */
export const SCRIPT_GATE_FILES = Object.freeze(['scriptGate', 'scriptGateLabels'])

/** The older Japanese-only OCR rescue reader: encoder, decoder, vocabulary. One logical capability. */
export const OCR_FILES = Object.freeze(['ocrEncoder', 'ocrDecoder', 'ocrVocab'])

/** The optional text reader (Hayai OCR): vision graph, decoder, tokenizer. One logical capability. */
export const HAYAI_FILES = Object.freeze(['hayaiVision', 'hayaiDecoder', 'hayaiTokenizer'])

/** The two text policies, as `session.textPolicy` stores them. */
export const LEGACY_POLICY = 'legacy_gate'
export const ALL_TEXT_POLICY = 'all_text'

/**
 * @typedef {Object} Engine
 * @property {string} id
 * @property {string} name - a product name, not translated
 * @property {string} [noteKey] - one short line on what it is
 * @property {string[]} files - catalogue ids it needs
 * @property {string[]} [languages] - detection only: which languages it serves
 * @property {string} [sidecar] - cleaning only: the sidecar model directory it runs from
 * @property {string} [cloudModel] - cleaning only: offered through cloud setup, never downloaded here
 * @property {boolean} ready
 * @property {{efficiency: number, light: number}} rating
 */

/** @type {readonly Engine[]} */
export const DETECTORS = Object.freeze([
  {
    id: 'ctd-rtdetr',
    name: 'CTD + Ogkalu comic text & bubble detector',
    noteKey: 'pipelines.detector.ctdRtdetr',
    // The detector pair only. The script gate belongs to the legacy policy
    // and OCR to its optional rescue, so neither rides along with a detector.
    files: ['textDetector', 'balloonDetector'],
    languages: ['ja', 'zh', 'ko'],
    ready: true,
    rating: { efficiency: 4, light: 4 },
  },
])

/**
 * Detector ids a stored session may still hold, and what each one meant.
 *
 * `ctd-rtdetr-ocr` was a per-language row that queued the 440 MB reader while
 * the run obeyed the separate `ocrRescue` switch. OCR rescue is one explicit
 * capability now, so the row reads back as the plain detector with the rescue
 * switched on: nobody who picked it loses what they asked for.
 */
export const RETIRED_DETECTORS = Object.freeze({
  'ctd-rtdetr-ocr': Object.freeze({ detector: 'ctd-rtdetr', ocrRescue: true, languages: Object.freeze(['ja']) }),
})

/**
 * Read a stored detector choice for a language, migrating a retired id.
 *
 * @param {string} language
 * @param {unknown} value - what the session stored
 * @returns {{detector: string|null|undefined, ocrRescue: boolean}} `undefined` when the value is not a choice this language can hold
 */
export function migrateDetectorChoice(language, value) {
  if (value === null) return { detector: null, ocrRescue: false }
  if (typeof value !== 'string') return { detector: undefined, ocrRescue: false }
  const retired = Object.hasOwn(RETIRED_DETECTORS, value) ? RETIRED_DETECTORS[value] : null
  if (retired) {
    return retired.languages.includes(language)
      ? { detector: retired.detector, ocrRescue: retired.ocrRescue }
      : { detector: undefined, ocrRescue: false }
  }
  return detectorsFor(language).some((engine) => engine.id === value)
    ? { detector: value, ocrRescue: false }
    : { detector: undefined, ocrRescue: false }
}

/** The review preset corresponding to a selected detection combination. */
export function workflowForDetectorModels(value) {
  const selected = normalizeDetectorModels(value)
  const ctd = selected.includes('ctd')
  const rt = selected.includes('rtSmall') || selected.includes('rtFull')
  const sam = selected.includes('samTs')
  if (ctd && rt && sam) return 'ctd_text_shape'
  if (ctd && rt) return 'ctd_regions'
  if (ctd && sam) return 'ctd_mask'
  if (rt && sam) return 'text_shape'
  if (ctd) return 'ctd'
  if (rt) return 'regions'
  return 'mask'
}

/** Independent model analysis presets; a selected combination chooses the initial one. */
export const WORKFLOW_PRESETS = Object.freeze([
  { id: 'ctd', name: 'Comic Text Detector (CTD)', needs: ['ctd'], description: 'CTD text boxes' },
  { id: 'ctd_regions', name: 'CTD + Ogkalu detector', needs: ['ctd', 'rt'], description: 'CTD text with Ogkalu comic text & bubble regions' },
  { id: 'ctd_mask', name: 'CTD + SAM-TS-L lettering mask', needs: ['ctd', 'sam'], description: 'CTD text with SAM-TS-L lettering pixels' },
  { id: 'ctd_text_shape', name: 'CTD + Ogkalu detector + SAM-TS-L lettering mask', needs: ['ctd', 'rt', 'sam'], description: 'All three detection models' },
  { id: 'regions', name: 'Regions only', needs: ['rt'], description: 'Ogkalu comic text & bubble boxes' },
  { id: 'mask', name: 'Mask only', needs: ['sam'], description: 'SAM-TS-L lettering pixels, without the Ogkalu detector or OCR' },
  { id: 'text_shape', name: 'Text-shaped review', needs: ['rt', 'sam'], description: 'Ogkalu detector context with the unchanged SAM-TS-L mask' },
])

export const OPTIONAL_SFX = Object.freeze({ id: 'coo-mtsv3', name: 'COO MTSv3 SFX assist', selectable: false })

/** @type {readonly Engine[]} */
export const CLEANERS = Object.freeze([
  { id: 'lama-manga', name: 'LaMa Manga', noteKey: 'pipelines.cleaner.lamaManga', files: ['inpainter'], ready: true, rating: { efficiency: 4, light: 4 } },
  { id: 'big-lama', name: 'Big LaMa', noteKey: 'pipelines.cleaner.bigLama', files: [], ready: false, rating: { efficiency: 3, light: 3 } },
  { id: 'flux2-klein-4b', name: 'FLUX.2 Klein 4B', noteKey: 'pipelines.cleaner.flux', files: [], sidecar: 'flux2-klein-4b', ready: false, rating: { efficiency: 3, light: 2 } },
  { id: 'flux2-klein-4b-cloud', name: '☁ FLUX.2 Klein 4B', noteKey: 'pipelines.cleaner.fluxCloud', files: [], cloudModel: 'flux2-klein-4b', ready: false, rating: { efficiency: 3, light: 2 } },
  { id: 'flux2-klein-9b', name: '☁ FLUX.2 Klein 9B', noteKey: 'pipelines.cleaner.fluxCloud', files: [], cloudModel: 'flux2-klein-9b', ready: false, rating: { efficiency: 2, light: 1 } },
  { id: 'flux1-schnell', name: 'FLUX.1 [schnell]', noteKey: 'pipelines.cleaner.flux', files: [], sidecar: 'flux1-schnell', ready: false, rating: { efficiency: 3, light: 1 } },
  { id: 'flux1-dev', name: 'FLUX.1 [dev]', noteKey: 'pipelines.cleaner.flux', files: [], sidecar: 'flux1-dev', ready: false, rating: { efficiency: 2, light: 1 } },
  { id: 'qwen-image-edit-2511', name: '☁ Qwen-Image-Edit-2511', noteKey: 'pipelines.cleaner.qwen', files: [], cloudModel: 'qwen-image-edit-2511', ready: false, rating: { efficiency: 2, light: 1 } },
])

/** The detector a language starts on. */
export const DEFAULT_DETECTOR = 'ctd-rtdetr'

/**
 * Independent detection capabilities, by the ids sessions and the native run
 * store. The Ogkalu detector has two mutually exclusive profiles, `rtFull`
 * and `rtSmall`; CTD and the SAM-TS-L lettering mask combine with either.
 * This order is the stored and wire order; `DETECTOR_CHOICES` is the order a
 * screen lists them in.
 */
export const DETECTOR_MODEL_IDS = Object.freeze(['ctd', 'rtSmall', 'rtFull', 'samTs'])
export const DEFAULT_DETECTOR_MODELS = Object.freeze(['ctd', 'rtSmall'])

/** The detection choices as Settings and setup list them: CTD, Full, Small, SAM-TS-L. */
export const DETECTOR_CHOICES = Object.freeze(['ctd', 'rtFull', 'rtSmall', 'samTs'])

/** The profile a selected one excludes. */
const EXCLUDES = Object.freeze({ rtFull: 'rtSmall', rtSmall: 'rtFull' })

/**
 * Validate a persisted or incoming combination without silently enabling models.
 *
 * Migration, for records written by earlier builds or by hand: ids this build
 * does not know are dropped rather than voiding the rest, and a record that
 * holds both Ogkalu profiles, which no run accepts, keeps the Small one (the
 * default, downloadable profile) with the rest of its choices. A record with
 * nothing usable left, or that is not a list, reads as the default.
 */
export function normalizeDetectorModels(value) {
  if (!Array.isArray(value)) return [...DEFAULT_DETECTOR_MODELS]
  const selected = new Set(value.filter((id) => DETECTOR_MODEL_IDS.includes(id)))
  if (selected.has('rtSmall') && selected.has('rtFull')) selected.delete('rtFull')
  if (!selected.size) return [...DEFAULT_DETECTOR_MODELS]
  return DETECTOR_MODEL_IDS.filter((id) => selected.has(id))
}

/**
 * Tick or untick one detection model. Choosing one Ogkalu profile clears the
 * other; a change that would leave nothing selected is refused, because a run
 * needs at least one model.
 *
 * @param {unknown} current - the selection now
 * @param {string} id
 * @param {boolean} checked
 * @returns {string[]} the new selection, in stored order
 */
export function toggleDetectorModel(current, id, checked) {
  const selected = normalizeDetectorModels(current)
  if (!DETECTOR_MODEL_IDS.includes(id)) return selected
  let next = selected.filter((entry) => entry !== id)
  if (checked) next = [...next.filter((entry) => entry !== EXCLUDES[id]), id]
  return next.length ? DETECTOR_MODEL_IDS.filter((entry) => next.includes(entry)) : selected
}

/**
 * The detection stages a cloud GPU can run instead of this computer, with the
 * wire capability each one sends. Only these two have a cloud twin: the
 * Ogkalu detector's Full profile and the SAM-TS-L lettering mask. CTD, the
 * Small profile and OCR always run here.
 */
export const CLOUD_STAGES = Object.freeze([
  Object.freeze({ id: 'rtFull', capability: 'text_regions_rt@1' }),
  Object.freeze({ id: 'samTs', capability: 'text_mask_sam_ts@1' }),
])
export const ANALYSIS_TARGETS = /** @type {const} */ (['local', 'cloud'])

/** Every stage local, which is the default and the answer to anything unreadable. */
export function defaultAnalysisTargets() {
  return { rtFull: 'local', samTs: 'local' }
}

/**
 * Coerce a stored `analysisTargets` into the one shape the backend accepts.
 * Unknown stages are dropped and any value but `cloud` reads as `local`, so a
 * damaged record can only ever keep pages on this computer.
 *
 * @param {unknown} raw
 * @returns {{rtFull: 'local'|'cloud', samTs: 'local'|'cloud'}}
 */
export function normalizeAnalysisTargets(raw) {
  const record = raw && typeof raw === 'object' && !Array.isArray(raw) ? /** @type {Record<string, unknown>} */ (raw) : {}
  const targets = defaultAnalysisTargets()
  for (const stage of CLOUD_STAGES) {
    if (record[stage.id] === 'cloud') targets[stage.id] = 'cloud'
  }
  return targets
}

/** Whether a detection model has a cloud twin today. @param {string} id */
export function runsOnCloud(id) {
  return CLOUD_STAGES.some((stage) => stage.id === id)
}

/**
 * One place for every cloud-capable stage, as the editor's single *Detect on*
 * sets it: both on this computer or both on the cloud GPU.
 *
 * A record with the two stages apart, which only Settings' retired per-model
 * *Run on* could write, settles on this computer: a model the user kept here
 * is never sent to the cloud by a migration, and choosing Cloud GPU again is
 * one press in Text cleanup. The native side refuses a split write and
 * settles a split it finds stored the same way
 * (`inference/run_analysis.rs#AnalysisTargets`), and the boot reconciliation
 * writes the settled value back (`App.svelte`, `reconcileSettings`).
 *
 * @param {unknown} targets
 * @returns {{rtFull: 'local'|'cloud', samTs: 'local'|'cloud'}}
 */
export function unifyAnalysisTargets(targets) {
  const routed = normalizeAnalysisTargets(targets)
  return routed.rtFull === routed.samTs ? routed : defaultAnalysisTargets()
}

/**
 * The detection models a Cloud GPU run uses, whatever this computer's own
 * selection holds: CTD, the Ogkalu detector's Full profile and the SAM-TS-L
 * lettering mask, with the text reader (`CLOUD_RUN_READS`). Full and SAM-TS-L
 * run on the cloud GPU; CTD and the reader are light, give the same results
 * anywhere, and run on this computer beside them. Small has no cloud twin and
 * Full replaces it, so a cloud run never holds it (`run.rs#cloud_detect_refusal`).
 */
export const CLOUD_DETECTOR_MODELS = Object.freeze(['ctd', 'rtFull', 'samTs'])
export const CLOUD_RUN_READS = true

/**
 * Whether Text cleanup detects on the cloud GPU: both cloud-capable stages
 * routed there, as the single *Detect on* sets them. A split record settles on
 * this computer (`unifyAnalysisTargets`).
 *
 * @param {unknown} targets
 */
export function detectsOnCloud(targets) {
  return unifyAnalysisTargets(targets).rtFull === 'cloud'
}

/**
 * The selection a run actually uses: the single source every run, download
 * plan, readiness line, cloud capability list and placement note reads.
 *
 * - **This computer:** the user's combination and text reader switch, as
 *   stored.
 * - **Cloud GPU:** `CLOUD_DETECTOR_MODELS` and the reader, fixed. The user's
 *   own combination and switch stay in the session untouched, so choosing
 *   This computer again gives them back.
 *
 * `ocrRescue` is the switch as sent to the run; a retired `ctd-rtdetr-ocr`
 * detector choice still turns the reader on through `rescueRuns`.
 *
 * @param {{detectorModels?: unknown, analysisTargets?: unknown, ocrRescue?: boolean}} [choices]
 * @returns {{target: 'local'|'cloud', detectorModels: string[], ocrRescue: boolean}}
 */
export function runDetection({ detectorModels = null, analysisTargets = null, ocrRescue = false } = {}) {
  if (detectsOnCloud(analysisTargets)) {
    return { target: 'cloud', detectorModels: [...CLOUD_DETECTOR_MODELS], ocrRescue: CLOUD_RUN_READS }
  }
  return { target: 'local', detectorModels: normalizeDetectorModels(detectorModels), ocrRescue: ocrRescue === true }
}

/**
 * Where each detection model a run uses runs, in the order screens list them.
 * On the cloud GPU, Full and SAM-TS-L go there and CTD stays here; on this
 * computer every selected model runs here.
 *
 * @param {{detectorModels?: unknown, analysisTargets?: unknown, ocrRescue?: boolean}|null|undefined} [choices]
 * @returns {{target: 'local'|'cloud', cloud: string[], local: string[]}} ids in choice order
 */
export function detectionPlacement(choices) {
  const run = runDetection(choices ?? {})
  const ordered = DETECTOR_CHOICES.filter((id) => run.detectorModels.includes(id))
  const cloud = run.target === 'cloud' ? ordered.filter((id) => runsOnCloud(id)) : []
  return { target: run.target, cloud, local: ordered.filter((id) => !cloud.includes(id)) }
}

/**
 * The wire capabilities a run sends to the cloud, sorted as the native side
 * sorts them: both cloud stages on the cloud GPU, none on this computer.
 *
 * @param {{detectorModels?: unknown, analysisTargets?: unknown}|null|undefined} [choices]
 * @returns {string[]}
 */
export function cloudCapabilitiesFor(choices) {
  const placement = detectionPlacement(choices)
  return CLOUD_STAGES.filter((stage) => placement.cloud.includes(stage.id)).map((stage) => stage.capability).sort()
}

/**
 * The detection models a run needs on this computer: a stage routed to the
 * cloud needs no local file.
 *
 * @param {{detectorModels?: unknown, analysisTargets?: unknown}|null|undefined} [choices]
 */
export function localDetectorModels(choices) {
  const placement = detectionPlacement(choices)
  return DETECTOR_MODEL_IDS.filter((id) => placement.local.includes(id))
}

/**
 * The ready detectors a language can use, in table order.
 * @param {string} language
 */
export function detectorsFor(language) {
  return DETECTORS.filter((engine) => engine.ready && engine.languages?.includes(language))
}

/**
 * Every detector that serves a language, ready or not, in table order.
 * @param {string} language
 */
export function allDetectorsFor(language) {
  return DETECTORS.filter((engine) => engine.languages?.includes(language))
}

/** @param {string} id */
export function detector(id) {
  return DETECTORS.find((engine) => engine.id === id) ?? null
}

/** @param {string} id */
export function cleaner(id) {
  return CLEANERS.find((engine) => engine.id === id) ?? null
}

/**
 * The detector a language resolves to, retired ids included, or null when the
 * language is skipped or holds nothing this table can run.
 *
 * @param {Record<string, string|null>|null|undefined} detection
 * @param {string} language
 */
function chosenDetector(detection, language) {
  const { detector: id } = migrateDetectorChoice(language, detection?.[language] ?? null)
  const engine = id ? detector(id) : null
  return engine?.ready && engine.languages?.includes(language) ? engine : null
}

/**
 * Whether a set of choices asks for the text reader: the switch is on (or a
 * retired `ctd-rtdetr-ocr` row still says so). It reads every language and
 * runs under both text policies (`RunSelection::reader`), so a run that
 * cleans anything can use it.
 *
 * @param {Record<string, string|null>|null|undefined} detection
 * @param {boolean|undefined} ocrRescue
 */
export function rescueRuns(detection, ocrRescue) {
  return ocrRescue === true || migrateDetectorChoice('ja', detection?.ja ?? null).ocrRescue
}

/**
 * The logical models a workflow needs before it can run, in download order.
 *
 * - **Legacy script filtering** with at least one language cleaned: the text
 *   finder, the speech bubble finder, the script gate pair, and the text
 *   reader only when `rescueRuns`.
 * - **All text**: the speech bubble finder for the review's small whole-page
 *   profile and the SAM-TS-L graphs, and the text reader only when
 *   `rescueRuns`. Never the gate. SAM is an import, so it is listed but never
 *   downloaded.
 *
 * The models are the run's (`runDetection`): on the cloud GPU that is CTD and
 * the text reader here, whatever this computer's own selection holds, and a
 * stage routed to the cloud is never a local need.
 *
 * @param {{textPolicy?: string, detection?: Record<string, string|null>|null, ocrRescue?: boolean, detectorModels?: string[]|null, analysisTargets?: Record<string, string>|null}} [choices]
 * @returns {string[]} logical model ids (`MODELS`)
 */
export function workflowNeeds({ textPolicy = LEGACY_POLICY, detection = null, ocrRescue = false, detectorModels = null, analysisTargets = null } = {}) {
  if (detectorModels || detectsOnCloud(analysisTargets)) {
    const run = runDetection({ detectorModels, analysisTargets, ocrRescue })
    const selected = localDetectorModels({ detectorModels, analysisTargets })
    const cleaned = LANGUAGES.some((language) => chosenDetector(detection, language.id))
    if (!cleaned && textPolicy !== ALL_TEXT_POLICY) return []
    return [...selected, ...(textPolicy === LEGACY_POLICY ? ['scriptGate'] : []),
      ...(rescueRuns(detection, run.ocrRescue) ? ['hayaiOcr'] : [])]
  }
  const reader = rescueRuns(detection, ocrRescue) ? ['hayaiOcr'] : []
  if (textPolicy === ALL_TEXT_POLICY) return ['rtSmall', 'samTs', ...reader]
  const cleaned = LANGUAGES.some((language) => chosenDetector(detection, language.id))
  if (!cleaned) return []
  return ['ctd', 'rtSmall', 'scriptGate', ...reader]
}

/**
 * Where the engine runtime leaves a workflow. Every native workflow loads its
 * models through ONNX Runtime, so a workflow with every file on disk still
 * cannot run while the runtime is missing: a readiness line that counted files
 * alone called it ready, and the run then refused to start.
 *
 * A workflow with no needs (legacy with every language skipped) runs nothing,
 * so the runtime does not decide anything for it.
 *
 * **Installed is not enough.** The catalogue's `installed` is a file found; a
 * native run also loads it and refuses to start when that fails - a CUDA build
 * on a machine without CUDA, a quarantined or damaged library. `load` is what
 * the `diagnostics` command said when it made that same load: `loaded`,
 * `failed`, `checking` while the question is out, `unchecked` when the
 * question itself was refused. Only `loaded` makes an installed runtime
 * `installed` here; anything else keeps the workflow from reading as ready.
 *
 * @param {{installed?: boolean, available?: boolean, downloading?: boolean}|null|undefined} runtime - the catalogue's runtime row
 * @param {readonly string[]} needs - what `workflowNeeds` answered
 * @param {'loaded'|'failed'|'checking'|'unchecked'} [load] - whether the installed runtime loads
 * @returns {'notNeeded'|'installed'|'unloadable'|'checking'|'unchecked'|'downloading'|'missing'|'unavailable'}
 */
export function runtimeState(runtime, needs, load) {
  if (needs.length === 0) return 'notNeeded'
  if (runtime?.installed === true) {
    if (load === 'loaded') return 'installed'
    if (load === 'failed') return 'unloadable'
    return load === 'unchecked' ? 'unchecked' : 'checking'
  }
  if (runtime?.downloading === true) return 'downloading'
  return runtime?.available === false ? 'unavailable' : 'missing'
}

/**
 * The catalogue ids a set of choices needs, deduplicated, in first-seen order.
 * Pure: the same answer for the same choices, whatever is installed.
 *
 * Downloads follow the selected workflow. Legacy filtering needs each cleaned
 * language's detector files, then the script gate pair, then the three text
 * reader files when its switch is on. All-text needs the speech bubble finder,
 * the reader when its switch is on, and nothing from the gate. Imported
 * graphs (SAM-TS-L, the Full detector) are never downloads. Every ready cleaner
 * that is wanted adds its own files after that.
 *
 * @param {Record<string, string|null>|null|undefined} detection - language id → detector id, or null to skip the language
 * @param {Record<string, boolean>|null|undefined} cleaners - cleaner id → wanted
 * @param {{textPolicy?: string, ocrRescue?: boolean, detectorModels?: string[]|null, analysisTargets?: Record<string, string>|null}} [options] - the policy (legacy by default), the rescue switch, and the selection and targets `runDetection` reads
 * @returns {string[]}
 */
export function filesFor(detection, cleaners, { textPolicy = LEGACY_POLICY, ocrRescue = false, detectorModels = null, analysisTargets = null } = {}) {
  const ids = new Set()
  if (detectorModels || detectsOnCloud(analysisTargets)) {
    for (const id of workflowNeeds({ textPolicy, detection, ocrRescue, detectorModels, analysisTargets })) {
      for (const file of model(id)?.files ?? []) ids.add(file)
    }
  } else if (textPolicy === ALL_TEXT_POLICY) {
    for (const file of model('rtSmall')?.files ?? []) ids.add(file)
    if (rescueRuns(detection, ocrRescue)) for (const file of HAYAI_FILES) ids.add(file)
  } else {
    for (const language of LANGUAGES) {
      for (const file of chosenDetector(detection, language.id)?.files ?? []) ids.add(file)
    }
    const needs = workflowNeeds({ textPolicy, detection, ocrRescue })
    if (needs.includes('scriptGate')) for (const file of SCRIPT_GATE_FILES) ids.add(file)
    if (needs.includes('hayaiOcr')) for (const file of HAYAI_FILES) ids.add(file)
  }
  for (const engine of CLEANERS) {
    if (engine.ready && cleaners?.[engine.id]) for (const file of engine.files) ids.add(file)
  }
  return [...ids]
}

/* ------------------------------------------------------------------ */
/* The capability graph                                                */
/* ------------------------------------------------------------------ */

/**
 * What a removal can stop, by workflow. Named so a Delete can say which of
 * them it disables before it happens.
 *
 * - `legacy` - legacy automatic cleaning (CTD + the Small detector + script gate)
 * - `ocrRescue` - the optional Japanese OCR rescue inside legacy filtering
 * - `reviewSmall` - the text-shaped review's small whole-page RT profile
 * - `reviewFull` - the text-shaped review's full two-tile RT profile
 * - `review` - the text-shaped review itself (its lettering mask)
 * - `lama` - LaMa redraw
 */
export const WORKFLOWS = Object.freeze(['legacy', 'ocrRescue', 'reviewSmall', 'reviewFull', 'review', 'lama'])

/**
 * @typedef {Object} LogicalModel
 * @property {string} id
 * @property {string} nameKey - what the row is called
 * @property {string|null} product - the product name, data rather than copy
 * @property {'download'|'import'|'excluded'} source - how it reaches the machine
 * @property {string[]} files - catalogue ids, for downloads; empty otherwise
 * @property {string|null} group - the native group id that installs and removes the files as one unit
 * @property {'samTs'|'fullRt'|null} importId - which import command owns it
 * @property {string[]} disables - the workflows its removal stops
 * @property {string} roleKey - one line on what it does and for whom
 * @property {string|null} removeKey - the confirmation a removal asks, naming what it disables
 */

/** @type {readonly LogicalModel[]} */
export const MODELS = Object.freeze([
  {
    id: 'ctd',
    nameKey: 'models.kind.textDetector',
    product: DETECTOR_MODEL_NAMES.ctd,
    source: 'download',
    files: ['textDetector'],
    group: null,
    importId: null,
    disables: ['legacy'],
    roleKey: 'settings.detection.role.ctd',
    removeKey: 'settings.models.remove.ctd',
  },
  {
    id: 'rtSmall',
    nameKey: 'models.kind.balloonDetector',
    product: DETECTOR_MODEL_NAMES.rtSmall,
    source: 'download',
    files: ['balloonDetector'],
    group: null,
    importId: null,
    disables: ['legacy', 'reviewSmall'],
    roleKey: 'settings.detection.role.rtSmall',
    removeKey: 'settings.models.remove.rtSmall',
  },
  {
    id: 'rtFull',
    nameKey: 'models.kind.fullRt',
    product: DETECTOR_MODEL_NAMES.rtFull,
    source: 'download',
    files: ['fullRt'],
    group: null,
    importId: null,
    disables: ['reviewFull'],
    roleKey: 'settings.detection.role.rtFull',
    removeKey: 'settings.models.remove.rtFull',
  },
  {
    id: 'samTs',
    nameKey: 'models.kind.samTs',
    product: DETECTOR_MODEL_NAMES.samTs,
    source: 'import',
    files: [],
    group: null,
    importId: 'samTs',
    disables: ['review'],
    roleKey: 'settings.detection.role.samTs',
    removeKey: 'settings.models.remove.samTs',
  },
  {
    id: 'scriptGate',
    nameKey: 'settings.models.groups.scriptGate',
    product: 'ogkalu Image Script Identification',
    source: 'download',
    files: [...SCRIPT_GATE_FILES],
    group: 'scriptGate',
    importId: null,
    disables: ['legacy'],
    roleKey: 'settings.detection.role.scriptGate',
    removeKey: 'settings.models.remove.scriptGate',
  },
  {
    id: 'mangaOcr',
    nameKey: 'settings.models.groups.mangaOcr',
    product: 'Manga OCR',
    source: 'download',
    files: [...OCR_FILES],
    group: 'mangaOcr',
    importId: null,
    disables: ['ocrRescue'],
    roleKey: 'settings.detection.role.mangaOcr',
    removeKey: 'settings.models.remove.mangaOcr',
  },
  {
    id: 'hayaiOcr',
    nameKey: 'settings.models.groups.hayaiOcr',
    product: 'Hayai OCR v2.5 Nova',
    source: 'download',
    files: [...HAYAI_FILES],
    group: 'hayaiOcr',
    importId: null,
    disables: ['ocrRescue'],
    roleKey: 'settings.detection.role.hayaiOcr',
    removeKey: 'settings.models.remove.hayaiOcr',
  },
  {
    id: 'lama',
    nameKey: 'models.kind.inpainter',
    product: 'LaMa Manga',
    source: 'download',
    files: ['inpainter'],
    group: null,
    importId: null,
    disables: ['lama'],
    roleKey: 'settings.cleaning.role.lama',
    removeKey: 'settings.models.remove.lama',
  },
])

/**
 * The groups Settings > Models draws, in order, each with the logical models
 * it holds. `group` is the anchor the group is reached by (`detection`,
 * `cleaning`, and the collapsed `filtering` inside Detection), which is also
 * where the old Detection and Cleaning tab ids land. `advanced` groups are
 * collapsed until opened. A model that cannot be used in this version is not
 * listed at all: an entry nobody can choose is not a setting.
 */
export const CAPABILITIES = Object.freeze([
  { id: 'detect', group: 'detection', headingKey: 'settings.detection.capability.detect', models: [...DETECTOR_CHOICES], advanced: false },
  { id: 'filtering', group: 'filtering', headingKey: 'settings.detection.capability.japanese', models: ['scriptGate', 'hayaiOcr', 'mangaOcr'], advanced: true },
  { id: 'rebuild', group: 'cleaning', headingKey: 'settings.cleaning.capability.rebuild', models: ['lama'], advanced: false },
])

/** The Settings group that lists a logical model, or null for one no group draws. @param {string} id */
export function groupOfModel(id) {
  return CAPABILITIES.find((entry) => entry.models.includes(id))?.group ?? null
}

/** @param {string} id @returns {LogicalModel|null} */
export function model(id) {
  return MODELS.find((entry) => entry.id === id) ?? null
}

/**
 * The logical model a catalogue file belongs to, or null for a file no model
 * claims (a weight the backend added before this table knew it).
 *
 * @param {string} fileId
 * @returns {LogicalModel|null}
 */
export function modelOfFile(fileId) {
  return MODELS.find((entry) => entry.files.includes(fileId)) ?? null
}

/**
 * What removing a logical model, or any one of its files, stops. A group
 * member answers for its whole group: the native delete removes the group as
 * one unit. Another model's files are never part of the answer, so removing
 * Japanese filtering or OCR never names the shared Small detector weights.
 *
 * @param {string} id - a logical model id or a catalogue file id
 * @returns {{model: LogicalModel, files: string[], disables: string[]}|null}
 */
export function removalImpact(id) {
  const entry = model(id) ?? modelOfFile(id)
  if (!entry || entry.source === 'excluded') return null
  return { model: entry, files: [...entry.files], disables: [...entry.disables] }
}

/**
 * Whether the workflow the choices select uses a logical model, so a removal
 * can add that it stops the cleaning in force now.
 *
 * @param {string} id
 * @param {{textPolicy?: string, detection?: Record<string, string|null>|null, ocrRescue?: boolean}} [choices]
 */
export function usedNow(id, choices = {}) {
  return workflowNeeds(choices).includes(id)
}

/**
 * Bytes an engine still needs, given what is on disk.
 *
 * @param {Engine} engine
 * @param {Record<string, {bytes: number, installed: boolean}>} files - by catalogue id
 * @param {Record<string, boolean>} [finished] - ids that arrived since `files` was read
 */
export function engineBytes(engine, files, finished = {}) {
  return engine.files.reduce((sum, id) => {
    const file = files[id]
    return file && !file.installed && !finished[id] ? sum + file.bytes : sum
  }, 0)
}
