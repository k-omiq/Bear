/**
 * Session preferences - the things that survive a relaunch and are not part of
 * any one project: theme, the default reading direction for new projects,
 * whether cloud engines are allowed, how the original-view control behaves,
 * and where the editor's three floating windows sit.
 *
 * Shape of the module: a single rune-backed object plus free functions that
 * mutate it. Not a class with getters over hidden mutables - consumers read
 * `session.theme` directly and get reactivity for free.
 *
 * Persistence goes through `persist.js`: validated on read, defaults on
 * anything unrecognised, and written explicitly by the setters rather than by
 * an effect, so `systemDark` (derived from `matchMedia`) can never be written
 * to disk.
 */

import {
  readRecord,
  writeRecord,
  oneOf,
  boolOr,
  numberIn,
  plainObject,
} from './persist.js'
import {
  DEFAULT_POINTER_MODIFIER,
  isApplePlatform,
  modifierCap,
  normalizePointerModifier,
  rebindShortcut,
  resetAllShortcuts,
  resetShortcut,
  sanitizeShortcutOverrides,
  setShortcutOverrides,
  shortcutOverrides,
  unbindShortcut,
} from '../shortcuts.js'
import { CATALOGUES, matchLocale, provideContextParam, provideLocale } from '../i18n/index.js'
import {
  WINDOW_IDS,
  defaultGeometry,
  clampPosition,
  clampSize,
  raisedOrder,
  minWidthFor,
  contentSized,
  MAX_WIDTH,
  MIN_HEIGHT,
} from '../model/windows.js'
import { hexOnCommit } from '../ui/color.js'

import { localSeconds, normalizeDenoiseTarget } from '../model/denoise.js'
import { ANALYSIS_TARGETS, CLOUD_STAGES, DEFAULT_DETECTOR, DEFAULT_DETECTOR_MODELS, DETECTOR_MODEL_IDS, LANGUAGES, defaultAnalysisTargets, migrateDetectorChoice, normalizeAnalysisTargets, normalizeDetectorModels, unifyAnalysisTargets } from '../model/pipelines.js'

const STORAGE_KEY = 'session.v1'

/**
 * `system` follows the OS between `light` and `dark`; the rest are fixed.
 * Every fixed name is a token block in `app.css`, keyed off `data-theme`.
 */
export const THEMES = /** @type {const} */ (['system', 'light', 'dark', 'sakura', 'jade', 'ocean'])
/** @typedef {(typeof THEMES)[number]} Theme */
/** Each theme's name, by value. @type {Record<Theme, string>} */
export const THEME_LABEL_KEYS = Object.freeze({
  system: 'settings.theme.system',
  light: 'settings.theme.light',
  dark: 'settings.theme.dark',
  sakura: 'settings.theme.sakura',
  jade: 'settings.theme.jade',
  ocean: 'settings.theme.ocean',
})
export const READING_DIRECTIONS = /** @type {const} */ (['rtl', 'ltr'])
export const ORIGINAL_VIEW_MODES = /** @type {const} */ (['hold', 'pinned'])
export const TEXT_POLICIES = /** @type {const} */ (['legacy_gate', 'all_text'])

/**
 * The colour a detected region's mask is drawn in until somebody picks
 * another: `--page-mark`, the sky blue every mark on the sheet uses, because
 * it survives black ink and white paper alike.
 */
export const DEFAULT_MASK_COLOR = '#0284c7'

/**
 * The colour a mask over text **outside** a speech bubble is drawn in until
 * somebody picks another. `maskColor` above is the speech bubble one, and
 * every region whose place is unknown.
 *
 * A burnt orange: the far side of the wheel from the blue, so the two are
 * told apart by hue (blue against orange is the pair most colour-blind
 * readers still separate), and dark enough to survive black ink and white
 * paper as the blue does. Its outline clears 4:1 against both (5.2 on white,
 * 4.1 on black; the blue is 4.1 and 5.1).
 */
export const DEFAULT_OUTSIDE_MASK_COLOR = '#c2410c'

/**
 * How strong the mask's fill is, in percent. Enough to see which pixels Clean
 * will take, light enough to read the lettering under it; the outline is
 * always drawn at full strength.
 */
export const DEFAULT_MASK_OPACITY = 35

/**
 * A stored mask colour as `#rrggbb`, or `fallback`. Only a string is read: a
 * stored number such as `123456` is not a colour somebody picked.
 *
 * @param {unknown} value
 * @param {string} fallback
 * @returns {string}
 */
export function maskColorOr(value, fallback) {
  return (typeof value === 'string' && hexOnCommit(value)) || fallback
}

/**
 * A stored mask opacity as a whole percent, clamped into range like every
 * other stored number, or `fallback` when it is not a number at all.
 *
 * @param {unknown} value
 * @param {number} fallback
 * @returns {number}
 */
export function maskOpacityOr(value, fallback) {
  return Math.round(numberIn(value, { min: 0, max: 100, fallback }))
}

/**
 * The values the AI redraw engine row offers.
 *
 * `'auto'` first because it is the default and the answer for almost everybody:
 * the core resolves it per platform. The other two are the backends
 * `cleaner_core::sidecar::backend` has a contract for and can render through -
 * `sdcpp` is in that table and is deliberately **not** here, because it declines
 * every open and a control that fails is worse than a control that is absent.
 */
export const FLUX_BACKENDS = /** @type {const} */ (['auto', 'mflux', 'sdnq'])
export const MODEL_ACCELERATOR_IDS = Object.freeze([...DETECTOR_MODEL_IDS, 'inpainter'])

/** Preserve only known model overrides. A provider id is validated by the backend at use time. */
export function sanitizeModelAccelerators(raw) {
  const stored = plainObject(raw)
  /** @type {Record<string, string>} */
  const result = {}
  for (const id of MODEL_ACCELERATOR_IDS) {
    const value = stored[id]
    if (typeof value === 'string' && /^[a-z][a-z0-9]{0,31}$/.test(value)) result[id] = value
  }
  return result
}

export { WINDOW_IDS }

/**
 * The shortcut table names the three windows by what they used to be - panels
 * called `pages`, `masks` and `tools`. `src/lib/shortcuts.js` is the one place
 * a keybinding may be declared and is not this task's to edit, so the mapping
 * onto window ids lives here, in one line, rather than in the keyboard layer.
 */
const PANEL_TO_WINDOW = /** @type {Record<string, string>} */ ({
  pages: 'pages',
  masks: 'layers',
  tools: 'tool',
})

/** The viewport the default layout is computed against. */
function viewport() {
  return {
    vw: Math.round(globalThis.innerWidth) || 1400,
    vh: Math.round(globalThis.innerHeight) || 900,
  }
}

/**
 * @typedef {import('../model/windows.js').WindowGeometry & {open: boolean, fold: boolean}} WindowState
 */

/**
 * @typedef {Object} PersistedSession
 * @property {Theme} theme
 * @property {string} language - the interface language, a `CATALOGUES` tag; see `setLanguage`
 * @property {'rtl'|'ltr'} readingDirection - the default for new projects; a project overrides it
 * @property {boolean} cloudAllowed
 * @property {'hold'|'pinned'} originalView
 * @property {string} sidecarPath
 * @property {string} fluxModel
 * @property {'auto'|'mflux'|'sdnq'} fluxBackend
 * @property {string} accelerator - `auto`, `cpu`, or an execution provider id; see `setAccelerator`
 * @property {Record<string, string>} modelAccelerators - local per-model overrides; absent models inherit `accelerator`
 * @property {'alt'|'meta'|'control'|'shift'} cloneSourceModifier - held while clicking to set Clone / heal's source; see `setCloneSourceModifier`
 * @property {boolean} firstLaunchOffered - whether the first-launch download offer has been made on this machine
 * @property {boolean} closeToTray - close control hides the window and keeps downloads running
 * @property {Record<string, string|null>} detection - the detector each source language uses, by language id; null skips the language
 * @property {string[]} detectorModels - the detection models selected, by stored id: `ctd`, one Ogkalu profile (`rtFull` or `rtSmall`), `samTs`
 * @property {{rtFull: 'local'|'cloud', samTs: 'local'|'cloud'}} analysisTargets - where the two cloud-capable detection stages run; `cloud` needs consent per run
 * @property {'local'|'cloud'} cleanTarget - where Auto clean cleans stored detections; `cloud` needs consent per batch (docs/detect-clean.md)
 * @property {boolean} cleanLocalFirst - whether cloud clean tries saved flat-colour picks here first
 * @property {'local'|'cloud'|'off'} denoiseTarget - where page denoise runs; `cloud` still asks for consent per chapter
 * @property {string} denoisePreset - the preset id last chosen; `model/denoise.js#validPreset` resolves it against the target
 * @property {number|null} denoiseLocalSecondsPerPage - what `benchmark_denoise_local` measured on this computer, or null before it has run
 * @property {'legacy_gate'|'all_text'} textPolicy - whether to use the legacy script gate or open no-recognition text-shaped review
 * @property {boolean} ocrRescue - whether the optional Japanese OCR rescue may run after an uncertain script decision
 * @property {Record<string, import('../shortcuts.js').Chord|null>} shortcuts - rebindings, by shortcut id; only the differences from the defaults
 * @property {string} maskColor - the colour the canvas draws a mask over speech bubble text in, `#rrggbb`; also every region whose place is unknown. The key the single mask colour was stored under, so a saved pick carries over as this one
 * @property {string} outsideMaskColor - the colour the canvas draws a mask over text outside speech bubbles in, `#rrggbb`
 * @property {number} maskOpacity - how strong either mask's fill is, a whole percent from 0 to 100
 * @property {Record<string, WindowState>} windows
 */

/** @returns {PersistedSession} */
function defaults() {
  const { vw, vh } = viewport()
  const geometry = defaultGeometry(vw, vh)
  /** @type {Record<string, WindowState>} */
  const windows = {}
  for (const id of WINDOW_IDS) windows[id] = { ...geometry[id], open: true, fold: false }
  return {
    theme: 'system',
    language: matchLocale(globalThis.navigator?.languages),
    readingDirection: 'rtl',
    cloudAllowed: false,
    originalView: 'hold',
    sidecarPath: '',
    fluxModel: '',
    fluxBackend: 'auto',
    accelerator: 'auto',
    modelAccelerators: {},
    cloneSourceModifier: DEFAULT_POINTER_MODIFIER,
    firstLaunchOffered: false,
    closeToTray: false,
    detection: defaultDetection(),
    detectorModels: [...DEFAULT_DETECTOR_MODELS],
    analysisTargets: defaultAnalysisTargets(),
    cleanTarget: 'local',
    cleanLocalFirst: false,
    denoiseTarget: 'off',
    denoisePreset: '',
    denoiseLocalSecondsPerPage: null,
    textPolicy: 'legacy_gate',
    ocrRescue: false,
    shortcuts: {},
    maskColor: DEFAULT_MASK_COLOR,
    outsideMaskColor: DEFAULT_OUTSIDE_MASK_COLOR,
    maskOpacity: DEFAULT_MASK_OPACITY,
    windows,
  }
}

/**
 * Coerce anything at all into a valid session record. Exported so the
 * validation can be tested without a DOM.
 *
 * A stored geometry is validated field by field but **not** clamped to the
 * current viewport here: the viewport at read time may not be the one the
 * editor will mount into. `clampWindowsToViewport()` does that, from the
 * editor, and again on every resize.
 *
 * @param {unknown} raw
 * @returns {PersistedSession}
 */
export function sanitizeSession(raw) {
  const base = defaults()
  const record = plainObject(raw)
  const stored = plainObject(record.windows)

  /** @type {Record<string, WindowState>} */
  const windows = {}
  for (const id of WINDOW_IDS) {
    const fallback = base.windows[id]
    const win = plainObject(stored[id])
    // A null height is "size to content", and it is a value in its own right -
    // it must survive both a missing record and a stored `null`.
    const height = win.h === undefined ? fallback.h : win.h
    windows[id] = {
      // A window may hang off an edge, so x and y take a wide band here and
      // are brought onto the real viewport by `clampWindowsToViewport`.
      x: numberIn(win.x, { min: -4000, max: 8000, fallback: fallback.x }),
      y: numberIn(win.y, { min: -4000, max: 8000, fallback: fallback.y }),
      // A content-sized window's stored width is a stale measurement rather
      // than a preference: it is kept only so the first clamp has a truthful
      // number to keep a grabbable strip by, and the element overwrites it as
      // soon as it is on screen. So no ceiling for it either.
      w: numberIn(win.w, {
        min: minWidthFor(id),
        max: contentSized(id) ? 8000 : MAX_WIDTH,
        fallback: fallback.w,
      }),
      // A content-sized window has no height and cannot be folded: the tool
      // bar is one row of controls, and it has neither a resize corner nor a
      // body to collapse. A record written before it was a bar still carries
      // the tool window's `h` and may carry `fold: true`, and restoring either
      // would leave the store asserting a geometry the element does not have.
      // Nothing would be seen to go wrong - `ToolBar.svelte` reads neither
      // field - which is the reason to drop them here rather than trust that
      // no future reader looks. Dropped on read rather than migrated: there is
      // nothing to keep.
      h: contentSized(id)
        ? null
        : height === null
          ? null
          : numberIn(height, { min: MIN_HEIGHT, max: 8000, fallback: fallback.h ?? MIN_HEIGHT }),
      open: boolOr(win.open, fallback.open),
      fold: contentSized(id) ? false : boolOr(win.fold, fallback.fold),
    }
  }

  return {
    theme: /** @type {Theme} */ (oneOf(record.theme, THEMES, base.theme)),
    language: oneOf(record.language, Object.keys(CATALOGUES), base.language),
    readingDirection: /** @type {'rtl'|'ltr'} */ (
      oneOf(record.readingDirection, READING_DIRECTIONS, base.readingDirection)
    ),
    cloudAllowed: boolOr(record.cloudAllowed, base.cloudAllowed),
    originalView: /** @type {'hold'|'pinned'} */ (
      oneOf(record.originalView, ORIGINAL_VIEW_MODES, base.originalView)
    ),
    sidecarPath: typeof record.sidecarPath === 'string' ? record.sidecarPath : (typeof record.sidecarFolder === 'string' ? record.sidecarFolder : base.sidecarPath),
    fluxModel: typeof record.fluxModel === 'string' ? record.fluxModel : (typeof record.sidecarModel === 'string' ? record.sidecarModel : base.fluxModel),
    fluxBackend: /** @type {'auto'|'mflux'|'sdnq'} */ (
      oneOf(record.fluxBackend ?? record.sidecarBackend, FLUX_BACKENDS, base.fluxBackend)
    ),
    // Not validated against a list, and deliberately: the providers a machine
    // has come from the loaded ONNX Runtime (`listAccelerators`), so the only
    // side that could check this value is the backend - which refuses an
    // unusable one with a reason rather than silently substituting. A closed
    // list here would drop a provider a newer runtime added.
    accelerator: typeof record.accelerator === 'string' ? record.accelerator : base.accelerator,
    modelAccelerators: sanitizeModelAccelerators(record.modelAccelerators),
    // The shortcut module owns this vocabulary, the same way it owns the chord
    // one: anything that is not one of the four modifiers a pointer event can
    // carry falls back to the modifier this gesture has always used.
    cloneSourceModifier: normalizePointerModifier(record.cloneSourceModifier),
    // False is the safe default in both directions: a record written before
    // this flag existed offers the download once, which is the whole point of
    // the offer, and a machine that has already been asked carries a `true` it
    // wrote itself.
    firstLaunchOffered: boolOr(record.firstLaunchOffered, base.firstLaunchOffered),
    closeToTray: boolOr(record.closeToTray, base.closeToTray),
    detection: sanitizeDetection(record.detection),
    detectorModels: normalizeDetectorModels(record.detectorModels),
    // Anything unreadable is local: a damaged record may only keep pages here.
    // Two stages stored apart settle on this computer too: Detect on is one
    // place, and a migration never sends a model the user kept here.
    analysisTargets: unifyAnalysisTargets(record.analysisTargets),
    // As strict as the native setting: anything but `cloud` keeps the work here.
    cleanTarget: record.cleanTarget === 'cloud' ? 'cloud' : 'local',
    cleanLocalFirst: boolOr(record.cleanLocalFirst, base.cleanLocalFirst),
    // As strict as the native setting: anything unreadable is off, so a
    // damaged record never sends a page anywhere.
    denoiseTarget: normalizeDenoiseTarget(record.denoiseTarget),
    denoisePreset: typeof record.denoisePreset === 'string' ? record.denoisePreset : base.denoisePreset,
    denoiseLocalSecondsPerPage: localSeconds(record.denoiseLocalSecondsPerPage),
    textPolicy: /** @type {'legacy_gate'|'all_text'} */ (
      oneOf(record.textPolicy, TEXT_POLICIES, base.textPolicy)
    ),
    // A retired `ctd-rtdetr-ocr` row meant "rescue Japanese with OCR". It
    // reads back as the plain detector (above) with the rescue on, so the
    // intent survives the row. The first save then stores the migrated pair.
    ocrRescue: storedRetiredRescue(record.detection) || boolOr(record.ocrRescue, base.ocrRescue),
    // The shortcut table owns this vocabulary and validates it: an id the
    // table no longer has, a chord that will not parse, or a chord that is
    // only the default written out is dropped here rather than kept as a
    // binding nothing can run.
    shortcuts: sanitizeShortcutOverrides(record.shortcuts),
    // One colour used to cover every mask, under `maskColor`. It stays the
    // speech bubble colour, so a record saved before the split keeps its pick
    // there, and the outside colour starts at its default.
    maskColor: maskColorOr(record.maskColor, base.maskColor),
    outsideMaskColor: maskColorOr(record.outsideMaskColor, base.outsideMaskColor),
    maskOpacity: maskOpacityOr(record.maskOpacity, base.maskOpacity),
    windows,
  }
}

/** Initial stacking: the order they are declared in, lowest first. */
function initialStacking() {
  /** @type {Record<string, number>} */
  const ranks = {}
  WINDOW_IDS.forEach((id, index) => {
    ranks[id] = index + 1
  })
  return ranks
}

const storedRecord = readRecord(STORAGE_KEY, null)

/**
 * Whether this machine had a session on disk when the app started. Read once,
 * at module load, because the first `save()` makes it true forever after - and
 * "has this user ever set a preference in this app" is the question
 * `reconcileSettings()` needs answered.
 */
export const sessionWasStored = storedRecord !== null

/**
 * The live session. `systemDark` and `stacking` are runtime-only -
 * `systemDark` mirrors the OS, and which window was raised last is not worth
 * a disk write per click (see `persistable()`).
 */
export const session = $state({
  ...sanitizeSession(storedRecord),
  systemDark: false,
  /** @type {Record<string, number>} id → 1..n, highest on top */
  stacking: initialStacking(),
})

// `src/lib/shortcuts.js` holds the table actually in force and every consumer
// reads it from there. `session.shortcuts` is where that table's *differences*
// are stored and persisted, and the two are seeded here and kept in step by
// `syncShortcuts()` - never edited apart.
setShortcutOverrides(session.shortcuts)

/**
 * The subset that goes to disk. Hand-picked, so no derived value can leak in.
 * @returns {PersistedSession}
 */
function persistable() {
  /** @type {Record<string, WindowState>} */
  const windows = {}
  for (const id of WINDOW_IDS) {
    const win = session.windows[id]
    windows[id] = { x: win.x, y: win.y, w: win.w, h: win.h, open: win.open, fold: win.fold }
  }
  return {
    theme: session.theme,
    language: session.language,
    readingDirection: session.readingDirection,
    cloudAllowed: session.cloudAllowed,
    originalView: session.originalView,
    sidecarPath: session.sidecarPath,
    fluxModel: session.fluxModel,
    fluxBackend: session.fluxBackend,
    accelerator: session.accelerator,
    modelAccelerators: { ...session.modelAccelerators },
    cloneSourceModifier: session.cloneSourceModifier,
    firstLaunchOffered: session.firstLaunchOffered,
    closeToTray: session.closeToTray,
    detection: { ...session.detection },
    detectorModels: [...session.detectorModels],
    analysisTargets: { ...session.analysisTargets },
    cleanTarget: session.cleanTarget,
    cleanLocalFirst: session.cleanLocalFirst,
    denoiseTarget: session.denoiseTarget,
    denoisePreset: session.denoisePreset,
    denoiseLocalSecondsPerPage: session.denoiseLocalSecondsPerPage,
    textPolicy: session.textPolicy,
    ocrRescue: session.ocrRescue,
    shortcuts: shortcutOverrides(),
    maskColor: session.maskColor,
    outsideMaskColor: session.outsideMaskColor,
    maskOpacity: session.maskOpacity,
    windows,
  }
}

function save() {
  writeRecord(STORAGE_KEY, persistable())
}

/* ------------------------------------------------------------------ */
/* Theme                                                               */
/* ------------------------------------------------------------------ */

/**
 * The theme actually in force: `system` resolves against the OS.
 * Call it inside a template or an effect and it tracks both inputs.
 *
 * @returns {Exclude<Theme, 'system'>}
 */
export function resolvedTheme() {
  if (session.theme === 'system') return session.systemDark ? 'dark' : 'light'
  return session.theme
}

/**
 * Write the resolved theme onto `<html data-theme>`, which is what `app.css`
 * keys the dark token block off. Reactive: call it from an `$effect`.
 */
export function applyTheme() {
  const root = globalThis.document?.documentElement
  if (root) root.dataset.theme = resolvedTheme()
}

/**
 * Track `prefers-color-scheme` for the lifetime of the app, so `system` follows
 * the OS *live* and not only at load.
 *
 * @returns {() => void} teardown - return it straight from an `$effect`
 */
export function installThemeSync() {
  const query = globalThis.matchMedia?.('(prefers-color-scheme: dark)')
  if (!query) return () => {}
  session.systemDark = query.matches
  /** @param {MediaQueryListEvent} event */
  const onChange = (event) => {
    session.systemDark = event.matches
  }
  query.addEventListener('change', onChange)
  return () => query.removeEventListener('change', onChange)
}

/* ------------------------------------------------------------------ */
/* Language                                                            */
/* ------------------------------------------------------------------ */

/**
 * The interface language. A first run starts in the OS language when the app
 * speaks it, English otherwise, and the Welcome screen offers the choice
 * before anything else is read.
 *
 * **Not a backend setting**, like `cloneSourceModifier`: the core reports
 * keys, never sentences, so nothing outside this interface reads it.
 *
 * @param {string} tag - a `CATALOGUES` tag
 */
export function setLanguage(tag) {
  session.language = oneOf(tag, Object.keys(CATALOGUES), session.language)
  save()
}

/** `<html lang>`, for screen readers, line breaking and CJK font choice. Reactive: call it from an `$effect`. */
export function applyLanguage() {
  const root = globalThis.document?.documentElement
  if (root) root.lang = session.language
}

// Pushed into `i18n` rather than read by it, so the catalogue stays a
// catalogue. Reading `session` here is what redraws every string on a change.
provideLocale(() => session.language)

/** @param {Theme} theme */
export function setTheme(theme) {
  session.theme = oneOf(theme, THEMES, session.theme)
  save()
}

/* ------------------------------------------------------------------ */
/* Everything else                                                     */
/* ------------------------------------------------------------------ */

/** @param {boolean} enabled */
export function setCloseToTray(enabled) {
  session.closeToTray = boolOr(enabled, session.closeToTray)
  save()
}

/**
 * The colour masks over speech bubble text are drawn in, and masks whose
 * place is unknown (Settings › General). A value that is not a colour leaves
 * the one in force.
 *
 * @param {string} color
 */
export function setMaskColor(color) {
  session.maskColor = maskColorOr(color, session.maskColor)
  save()
}

/**
 * The colour masks over text outside speech bubbles are drawn in (Settings ›
 * General). A value that is not a colour leaves the one in force.
 *
 * @param {string} color
 */
export function setOutsideMaskColor(color) {
  session.outsideMaskColor = maskColorOr(color, session.outsideMaskColor)
  save()
}

/**
 * How strong a detected region's mask fill is, in whole percent.
 *
 * @param {number} opacity
 */
export function setMaskOpacity(opacity) {
  session.maskOpacity = maskOpacityOr(opacity, session.maskOpacity)
  save()
}

/**
 * Choose the detector for one source language, or `null` to skip it.
 *
 * Only a ready detector that serves the language is accepted; anything else
 * leaves the choice as it was.
 *
 * @param {string} language
 * @param {string|null} detectorId
 */
export function setDetection(language, detectorId) {
  if (!LANGUAGES.some((entry) => entry.id === language)) return
  // A retired id (`ctd-rtdetr-ocr`) is accepted as what it meant: the plain
  // detector, with the OCR rescue switched on.
  const { detector, ocrRescue } = migrateDetectorChoice(language, detectorId)
  if (detector === undefined) return
  session.detection = { ...session.detection, [language]: detector }
  if (ocrRescue) session.ocrRescue = true
  save()
}

/** @param {string[]} models */
export function setDetectorModels(models) {
  session.detectorModels = normalizeDetectorModels(models)
  save()
}

/**
 * Where one cloud-capable detection stage runs. Choosing `cloud` sends nothing
 * by itself: every run that would use it asks for consent first
 * (`editor/cloudrun.svelte.js`).
 *
 * @param {string} stage - `rtFull` or `samTs`
 * @param {string} target - `local` or `cloud`
 */
export function setAnalysisTarget(stage, target) {
  if (!CLOUD_STAGES.some((entry) => entry.id === stage) || !ANALYSIS_TARGETS.includes(/** @type {any} */ (target))) return
  session.analysisTargets = normalizeAnalysisTargets({ ...session.analysisTargets, [stage]: target })
  save()
}

/**
 * Where Auto clean cleans the regions it detected. Choosing `cloud` sends
 * nothing by itself: every batch asks for consent with the exact region count
 * and cost first (`editor/cloudrun.js`).
 *
 * @param {string} target - `local` or `cloud`
 */
export function setCleanTarget(target) {
  if (target !== 'local' && target !== 'cloud') return
  session.cleanTarget = target
  save()
}

/** @param {boolean} enabled */
export function setCleanLocalFirst(enabled) {
  if (typeof enabled !== 'boolean') return
  session.cleanLocalFirst = enabled
  save()
}

/**
 * Where page denoise runs. Choosing `cloud` sends nothing by itself: a
 * chapter's cloud denoise asks for consent with its page count and cost
 * first (`dialogs/DenoiseDialog.svelte`).
 *
 * @param {string} target - `local`, `cloud` or `off`
 */
export function setDenoiseTarget(target) {
  if (target !== 'local' && target !== 'cloud' && target !== 'off') return
  session.denoiseTarget = target
  save()
}

/** @param {string} id - a preset id; validity for the target is `model/denoise.js#validPreset`'s */
export function setDenoisePreset(id) {
  if (typeof id !== 'string') return
  session.denoisePreset = id
  save()
}

/** @param {unknown} seconds - seconds per page measured on this computer; anything else clears it */
export function setDenoiseLocalSeconds(seconds) {
  session.denoiseLocalSecondsPerPage = localSeconds(seconds)
  save()
}

/** @param {'legacy_gate'|'all_text'} policy */
export function setTextPolicy(policy) {
  session.textPolicy = /** @type {'legacy_gate'|'all_text'} */ (
    oneOf(policy, TEXT_POLICIES, session.textPolicy)
  )
  save()
}

/** @param {boolean} enabled */
export function setOcrRescue(enabled) {
  session.ocrRescue = boolOr(enabled, session.ocrRescue)
  save()
}

/** @returns {Record<string, string|null>} */
function defaultDetection() {
  return Object.fromEntries(LANGUAGES.map((language) => [language.id, DEFAULT_DETECTOR]))
}

/**
 * @param {unknown} raw
 * @returns {Record<string, string|null>}
 */
function sanitizeDetection(raw) {
  const record = plainObject(raw)
  const detection = defaultDetection()
  for (const language of LANGUAGES) {
    const { detector } = migrateDetectorChoice(language.id, record[language.id])
    if (detector !== undefined) detection[language.id] = detector
  }
  return detection
}

/**
 * Whether a stored detection map still holds a retired row that meant the OCR
 * rescue was wanted.
 *
 * @param {unknown} raw
 */
function storedRetiredRescue(raw) {
  const record = plainObject(raw)
  return LANGUAGES.some((language) => migrateDetectorChoice(language.id, record[language.id]).ocrRescue)
}

/** @param {'rtl'|'ltr'} direction */
export function setReadingDirection(direction) {
  session.readingDirection = oneOf(direction, READING_DIRECTIONS, session.readingDirection)
  save()
}

/** @param {boolean} allowed */
export function setCloudAllowed(allowed) {
  session.cloudAllowed = boolOr(allowed, session.cloudAllowed)
  save()
}

/**
 * Remember that the first-launch download offer has been made.
 *
 * Set by both answers the dialog takes - `Not now` and `Download` - because
 * what it records is that the user *was asked*, not what they said: a machine
 * that started the downloads and lost its connection halfway has been asked as
 * surely as one that declined, and a modal that came back on the next launch
 * to say the same thing again would be nagging rather than offering. Settings ›
 * Models is the way back for both of them.
 *
 * **Not a backend setting.** It is a fact about this interface on this machine
 * and nothing in the core reads it, so it stays out of `backendSettingsPatch()`
 * and out of `adoptBackendSettings()`; there is one copy of it and it is here.
 */
export function markFirstLaunchOffered() {
  session.firstLaunchOffered = true
  save()
}

/** @param {'hold'|'pinned'} mode */
export function setOriginalView(mode) {
  session.originalView = oneOf(mode, ORIGINAL_VIEW_MODES, session.originalView)
  save()
}

/** @param {string} path */
export function setSidecarPath(path) {
  session.sidecarPath = typeof path === 'string' ? path : ''
  save()
}

/** @param {string} model */
export function setFluxModel(model) {
  session.fluxModel = typeof model === 'string' ? model : ''
  save()
}

/**
 * Which sidecar backend the AI redraw rung asks for.
 *
 * `'auto'` is not a backend - it is a deferral to the core, which resolves it
 * per platform (`mflux` on Apple Silicon, `sdnq` elsewhere) beside the table it
 * resolves against. Nothing on this side knows which platform it is on, and
 * nothing here should: a value this store invented would be a second copy of
 * that decision.
 *
 * @param {string} backend
 */
export function setFluxBackend(backend) {
  session.fluxBackend = /** @type {'auto'|'mflux'|'sdnq'} */ (
    oneOf(backend, FLUX_BACKENDS, session.fluxBackend)
  )
  save()
}

/**
 * Which execution provider the next session is built on.
 *
 * `auto` - the default and the right answer for almost everybody - lets
 * `cleaner_core::accel` choose per model, which is not one answer: the
 * inpainter needs a `DFT` kernel that CUDA does not have, so a machine can
 * correctly run the detector on the GPU and LaMa on the CPU. `cpu` pins
 * everything to the CPU, and a provider id forces that one and is **refused
 * with a reason** where it cannot be had rather than silently ignored.
 *
 * **It takes effect on the next session, not on the running one.** `run.rs`
 * reads it before `Pipeline::open` and a session already built keeps the
 * provider it was built with, which is why the Settings row says so.
 *
 * @param {string} id
 */
export function setAccelerator(id) {
  session.accelerator = typeof id === 'string' && id ? id : 'auto'
  save()
}

/** An absent override follows the global choice; `inherit` clears it. */
export function setModelAccelerator(modelId, id) {
  if (!MODEL_ACCELERATOR_IDS.includes(modelId)) return
  const next = { ...session.modelAccelerators }
  if (id === 'inherit') delete next[modelId]
  else if (typeof id === 'string' && /^[a-z][a-z0-9]{0,31}$/.test(id)) next[modelId] = id
  else return
  session.modelAccelerators = next
  save()
}

/**
 * Which modifier is held while clicking the page to set where Clone / heal
 * reads from.
 *
 * It was `Alt` and nothing else, written into `DrawLayer`'s `onpointerdown` as
 * `event.altKey` and into the tool window's hint as the word *Alt* - a word
 * that is not printed on an Apple keyboard, where the key is `⌥` and is called
 * Option. So the gesture was documented in a name its user could not find.
 *
 * **Not a backend setting.** It is a fact about this interface's pointer
 * handling; nothing in the core reads it, and `run.rs` has no gestures. So it
 * stays out of `backendSettingsPatch()` and out of `adoptBackendSettings()`,
 * beside `firstLaunchOffered` - there is one copy of it and it is here.
 *
 * @param {string} modifier - one of `POINTER_MODIFIERS`
 */
export function setCloneSourceModifier(modifier) {
  session.cloneSourceModifier = normalizePointerModifier(modifier)
  save()
}

/**
 * And the same value as a keycap, for the one string whose call site cannot
 * pass it: `tools.hint.cloneHeal` is rendered by the tool bar as
 * `t(spec.hintKey)`, over a key the tool table chose.
 *
 * Registered here because this module owns the value. `i18n` imports nothing
 * to read it; the getter is pushed in, and reading `session` inside it is what
 * makes the tool bar's hint redraw when the row in Settings moves.
 */
provideContextParam('cloneSourceModifier', () =>
  modifierCap(session.cloneSourceModifier, { apple: isApplePlatform() }),
)

/* ------------------------------------------------------------------ */
/* Floating windows                                                    */
/* ------------------------------------------------------------------ */

/**
 * Position and size are written *live* while a pointer drags and persisted
 * once, on release: `save()` serialises the whole record to `localStorage`, and
 * a drag would otherwise do that a few hundred times. Open and fold are
 * discrete decisions and persist immediately.
 *
 * @param {string} id
 * @param {{x?: number, y?: number, w?: number, h?: number|null}} patch
 */
export function setWindowBox(id, patch) {
  const win = session.windows[id]
  if (!win) return
  const { vw, vh } = viewport()
  const size = clampSize(
    { w: patch.w ?? win.w, h: patch.h === undefined ? win.h : patch.h },
    vh,
    id,
  )
  const position = clampPosition(
    { x: patch.x ?? win.x, y: patch.y ?? win.y, w: size.w, h: size.h },
    vw,
    vh,
    id,
  )
  win.x = position.x
  win.y = position.y
  win.w = size.w
  win.h = size.h
}

/** Persist whatever the last drag or resize left behind. */
export function commitWindows() {
  save()
}

/**
 * Bring a window to the front of the other two. Not persisted - which window
 * was on top last is worth less than a disk write per click.
 *
 * @param {string} id
 */
export function raiseWindow(id) {
  if (session.stacking[id] === WINDOW_IDS.length) return
  session.stacking = raisedOrder(session.stacking, id)
}

/**
 * @param {string} id
 * @param {boolean} open
 */
export function setWindowOpen(id, open) {
  const win = session.windows[id]
  if (!win) return
  win.open = open
  if (open) raiseWindow(id)
  save()
}

/** Open a window if it is closed, and raise it either way. @param {string} id */
export function openWindow(id) {
  if (!session.windows[id]) return
  if (!session.windows[id].open) setWindowOpen(id, true)
  else raiseWindow(id)
}

/** @param {string} id */
export function toggleWindow(id) {
  if (!session.windows[id]) return
  setWindowOpen(id, !session.windows[id].open)
}

/** @param {string} id */
export function foldWindow(id) {
  const win = session.windows[id]
  if (!win) return
  win.fold = !win.fold
  save()
}

/**
 * Back to where this window started - the Escape of a keyboard move, and
 * **position only**, as the chrome spec says.
 *
 * Escape is bound on the grip, which is the move handle; the size belongs to
 * the corner, which has no Escape of its own. A user who resized a window and
 * then pressed Escape to re-place it asked for one of those two things, and
 * assigning the whole default geometry took the other away as well.
 *
 * The default position is computed from the current viewport and then clamped
 * through `setWindowBox` like any other move, so a window cannot be reset to
 * somewhere a short viewport does not have.
 *
 * @param {string} id
 */
export function resetWindowBox(id) {
  const win = session.windows[id]
  if (!win) return
  const { vw, vh } = viewport()
  const geometry = defaultGeometry(vw, vh)[id]
  if (!geometry) return
  const x = contentSized(id)
    ? Math.max(16, Math.round(vw / 2 - (win.w || geometry.w) / 2))
    : geometry.x
  setWindowBox(id, { x, y: geometry.y })
  save()
}

/**
 * Pull every window back onto the viewport. Called when the editor mounts and
 * on every viewport resize: a layout saved on a 27" display must not leave the
 * windows unreachable on a laptop.
 *
 * Deliberately does not persist - a resize is not a decision about where the
 * windows live, and the next drag will save the corrected geometry anyway.
 *
 * @param {number} [vw]
 * @param {number} [vh]
 */
export function clampWindowsToViewport(vw, vh) {
  const size = viewport()
  const width = vw ?? size.vw
  const height = vh ?? size.vh
  for (const id of WINDOW_IDS) {
    const win = session.windows[id]
    if (!win) continue
    const box = clampSize({ w: win.w, h: win.h }, height, id)
    const position = clampPosition({ x: win.x, y: win.y, w: box.w, h: box.h }, width, height, id)
    win.x = position.x
    win.y = position.y
    win.w = box.w
    win.h = box.h
  }
}

/**
 * The `F` / `L` / `T` shortcuts, which still speak the shortcut table's panel
 * vocabulary. See `PANEL_TO_WINDOW`.
 *
 * @param {'pages'|'masks'|'tools'} panel
 */
export function togglePanel(panel) {
  const id = PANEL_TO_WINDOW[panel]
  if (id) toggleWindow(id)
}

/* ------------------------------------------------------------------ */
/* Keyboard shortcuts                                                  */
/* ------------------------------------------------------------------ */

/**
 * Mirror the table's overrides back onto the session and write them.
 *
 * The reassignment is what makes the change visible: `session.shortcuts` is
 * the rune every consumer of the sheet reads to know the bindings moved, and
 * `src/lib/shortcuts.js` is where they actually live.
 */
function syncShortcuts() {
  session.shortcuts = shortcutOverrides()
  save()
}

/**
 * Bind a shortcut to a recorded chord. Refused, with a reason, when the chord
 * already belongs to another entry in an overlapping scope.
 *
 * @param {string} id
 * @param {import('../shortcuts.js').Chord} chord
 * @returns {import('../shortcuts.js').RebindResult}
 */
export function setShortcutBinding(id, chord) {
  const result = rebindShortcut(id, chord)
  if (result.ok) syncShortcuts()
  return result
}

/**
 * Clear a shortcut's chord. The row stays in the sheet, reading unbound.
 *
 * @param {string} id
 * @returns {import('../shortcuts.js').RebindResult}
 */
export function clearShortcutBinding(id) {
  const result = unbindShortcut(id)
  if (result.ok) syncShortcuts()
  return result
}

/**
 * Give one shortcut its default back.
 *
 * @param {string} id
 * @returns {import('../shortcuts.js').RebindResult}
 */
export function resetShortcutBinding(id) {
  const result = resetShortcut(id)
  if (result.ok) syncShortcuts()
  return result
}

/** Give every shortcut its default back. */
export function resetAllShortcutBindings() {
  resetAllShortcuts()
  syncShortcuts()
}

/* ------------------------------------------------------------------ */
/* Reconciling with the backend's own settings                         */
/* ------------------------------------------------------------------ */

/**
 * Adopt the backend's settings snapshot (`backend.readSettings()`) into the
 * session. The mapping between the two vocabularies - `cloudEngines:
 * 'allowed'|'blocked'` there, `cloudAllowed: boolean` here - lives only in this
 * function and its mirror, `backendSettingsPatch()`.
 *
 * Called by `reconcileSettings()`, never directly.
 *
 * @param {Record<string, unknown>} settings
 */
export function adoptBackendSettings(settings) {
  const record = plainObject(settings)
  if (record.closeToTray !== undefined) setCloseToTray(record.closeToTray === true)
  if (record.theme !== undefined) setTheme(/** @type {any} */ (record.theme))
  if (record.readingDirection !== undefined) {
    setReadingDirection(/** @type {any} */ (record.readingDirection))
  }
  if (record.cloudEngines !== undefined) setCloudAllowed(record.cloudEngines === 'allowed')
  if (record.originalView !== undefined) setOriginalView(/** @type {any} */ (record.originalView))
  if (record.sidecarPath !== undefined) {
    setSidecarPath(typeof record.sidecarPath === 'string' ? record.sidecarPath : '')
  } else if (record.sidecarFolder !== undefined) {
    setSidecarPath(typeof record.sidecarFolder === 'string' ? record.sidecarFolder : '')
  }
  if (record.fluxModel !== undefined) {
    setFluxModel(typeof record.fluxModel === 'string' ? record.fluxModel : '')
  } else if (record.sidecarModel !== undefined) {
    setFluxModel(typeof record.sidecarModel === 'string' ? record.sidecarModel : '')
  }
  if (record.fluxBackend !== undefined) {
    setFluxBackend(/** @type {any} */ (record.fluxBackend))
  } else if (record.sidecarBackend !== undefined) {
    setFluxBackend(/** @type {any} */ (record.sidecarBackend))
  }
  if (record.accelerator !== undefined) {
    setAccelerator(typeof record.accelerator === 'string' ? record.accelerator : 'auto')
  }
  if (record.modelAccelerators !== undefined) {
    session.modelAccelerators = sanitizeModelAccelerators(record.modelAccelerators)
    save()
  }
  if (record.analysisTargets !== undefined) {
    session.analysisTargets = unifyAnalysisTargets(record.analysisTargets)
    save()
  }
  if (record.cleanTarget !== undefined) setCleanTarget(record.cleanTarget === 'cloud' ? 'cloud' : 'local')
  if (record.denoiseTarget !== undefined) setDenoiseTarget(normalizeDenoiseTarget(record.denoiseTarget))
  if (typeof record.denoisePreset === 'string') setDenoisePreset(record.denoisePreset)
  if (localSeconds(record.denoiseLocalSecondsPerPage) !== null) setDenoiseLocalSeconds(record.denoiseLocalSecondsPerPage)
  if (record.shortcuts !== undefined) {
    setShortcutOverrides(record.shortcuts)
    syncShortcuts()
  }
}

/**
 * The session's shared preferences in the backend's vocabulary - what
 * `backend.writeSettings()` takes.
 *
 * `shortcuts` is the one nested value in the patch, and it survives the seam's
 * shallow one-level merge intact *because* it is nested: the whole override map
 * is written as one key, so clearing the last override is expressible (an empty
 * object) where a flattened `shortcuts.tool.brush` family would leave stale
 * entries behind forever.
 *
 * `analysisTargets` is read by `run_clean` when a caller does not name the
 * targets itself, so the saved choice is never silently ignored.
 *
 * @returns {{theme: string, readingDirection: string, cloudEngines: 'allowed'|'blocked', originalView: string, sidecarPath: string, fluxModel: string, fluxBackend: string, accelerator: string, modelAccelerators: Record<string,string>, analysisTargets: {rtFull: string, samTs: string}, cleanTarget: 'local'|'cloud', shortcuts: Record<string, import('../shortcuts.js').Chord|null>}}
 */
export function backendSettingsPatch() {
  return {
    closeToTray: session.closeToTray,
    theme: session.theme,
    readingDirection: session.readingDirection,
    cloudEngines: session.cloudAllowed ? 'allowed' : 'blocked',
    originalView: session.originalView,
    sidecarPath: session.sidecarPath,
    fluxModel: session.fluxModel,
    fluxBackend: session.fluxBackend,
    accelerator: session.accelerator,
    modelAccelerators: { ...session.modelAccelerators },
    analysisTargets: { ...session.analysisTargets },
    cleanTarget: session.cleanTarget,
    denoiseTarget: session.denoiseTarget,
    ...(session.denoisePreset ? { denoisePreset: session.denoisePreset } : {}),
    // Left out until measured: absent is "not measured", and a null would
    // be a value the backend has to decide about.
    ...(session.denoiseLocalSecondsPerPage !== null ? { denoiseLocalSecondsPerPage: session.denoiseLocalSecondsPerPage } : {}),
    shortcuts: shortcutOverrides(),
  }
}

/**
 * Reconcile the backend's settings with the session, and say what to write
 * back. **This is the only place the two are compared**, and the direction it
 * runs is:
 *
 * > **The backend seeds a session that has never existed. After that the
 * > session wins and the backend is made to agree with it.**
 *
 * Both halves matter, and neither is arbitrary.
 *
 * *The backend seeds a first run* because on a machine with no stored session
 * there is no user preference to defend - the backend's snapshot is the only
 * opinion in the system, and in the shipped app it is a real config file that a
 * user may well have written by hand.
 *
 * *The session wins thereafter* because the backend is not obliged to be
 * durable and today's is not: the mock rebuilds `defaultSettings()` on every
 * reload. Adopting unconditionally would reset the theme every time the page
 * refreshed and silently discard a preference the user had set in this app,
 * which is a worse failure than the two stores disagreeing was.
 *
 * The consequence that matters most: `session.cloudAllowed` - which
 * `cloudRefused()` gates every cloud request on - and the adapter's own
 * `settings.cloudEngines` can no longer drift apart, because opening Settings
 * pushes the session's value down, and every change pushes it again.
 *
 * @param {Record<string, unknown>} settings - `backend.readSettings()`
 * @returns {ReturnType<typeof backendSettingsPatch>} the patch to write back
 */
export function reconcileSettings(settings) {
  if (!sessionWasStored) adoptBackendSettings(settings)
  return backendSettingsPatch()
}
