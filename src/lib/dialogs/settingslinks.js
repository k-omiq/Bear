/**
 * Where Settings opens, and the ids older callers still ask for.
 *
 * Settings has seven sections: General, Models, Cloud, Denoise, Performance,
 * Shortcuts and About. Detection and Cleaning became two groups inside Models, and the
 * Cloud section's id is `cloud` now. A caller written before either change
 * still lands on the rows it meant: `detection` and `cleaning` open Models at
 * their group, `inference` opens Cloud, `acceleration` opens Performance.
 *
 * An **anchor** names one place inside a section: a group (`detection`,
 * `cleaning`, `filtering`, `access`), a model by its logical id (`samTs`,
 * `lama`), or `runtime` in Performance. The screen scrolls it into view,
 * opens the collapsed group that holds it, and moves focus to it, so a
 * missing-model notice or a cloud error can send the reader to the one row
 * that fixes it.
 *
 * Pure apart from `openSettings`, which pushes the modal.
 */

import { pushModal } from '../state/app.svelte.js'
import { CAPABILITIES, groupOfModel, model, modelOfFile } from '../model/pipelines.js'
import { DETECTOR_MODEL_NAMES } from '../model/model-names.js'

/** The sections, in the order the sidebar offers them. */
export const SETTINGS_SECTIONS = Object.freeze(['general', 'models', 'cloud', 'denoise', 'performance', 'shortcuts', 'about'])

/** Ids from before the restructure, and where their rows went. */
const ALIASES = Object.freeze({
  detection: Object.freeze({ section: 'models', anchor: 'detection' }),
  cleaning: Object.freeze({ section: 'models', anchor: 'cleaning' }),
  inference: Object.freeze({ section: 'cloud', anchor: null }),
  acceleration: Object.freeze({ section: 'performance', anchor: null }),
})

/** Anchors Models holds besides its groups and model ids. */
const MODEL_ANCHORS = new Set(['access', 'other', 'flux'])

/**
 * The section and anchor a request resolves to. Anything unknown is General
 * with no anchor; an anchor the section does not hold is dropped rather than
 * guessed at, so a stale caller still opens the right section.
 *
 * @param {unknown} tab
 * @param {unknown} [anchor]
 * @returns {{section: string, anchor: string|null}}
 */
export function resolveSettingsLink(tab, anchor) {
  const id = typeof tab === 'string' ? tab : ''
  const aliases = /** @type {Record<string, {section: string, anchor: string|null}>} */ (ALIASES)
  const alias = Object.hasOwn(aliases, id) ? aliases[id] : null
  const section = alias ? alias.section : SETTINGS_SECTIONS.includes(id) ? id : 'general'
  const wanted = typeof anchor === 'string' && anchor ? anchor : alias?.anchor ?? null
  return { section, anchor: wanted && anchorBelongs(section, wanted) ? wanted : null }
}

/**
 * @param {string} section
 * @param {string} anchor
 */
function anchorBelongs(section, anchor) {
  if (section === 'models') {
    return MODEL_ANCHORS.has(anchor) || CAPABILITIES.some((entry) => entry.group === anchor) || groupOfModel(anchor) !== null
  }
  if (section === 'performance') return anchor === 'runtime'
  if (section === 'cloud') return anchor === 'endpoints'
  return false
}

/**
 * The link that fixes a missing model: its row in Models, found by logical
 * id, catalogue file id or file group; the engine runtime's row in
 * Performance. A file no model claims goes to Models' list of other files.
 *
 * @param {string|null|undefined} id
 * @returns {{section: string, anchor: string|null}}
 */
export function settingsLinkForModel(id) {
  if (typeof id !== 'string' || !id) return { section: 'models', anchor: null }
  if (id === 'runtime') return { section: 'performance', anchor: 'runtime' }
  const named = Object.entries(DETECTOR_MODEL_NAMES).find(([, name]) => name === id)?.[0] ?? id
  const logical = model(named) ?? modelOfFile(named)
  if (logical && groupOfModel(logical.id)) return { section: 'models', anchor: logical.id }
  return { section: 'models', anchor: 'other' }
}

/**
 * Open Settings on a section, and optionally at one place inside it.
 *
 * @param {string} tab - a section id, or an older id (`detection`, `inference`)
 * @param {string|null} [anchor]
 */
export function openSettings(tab, anchor = null) {
  const link = resolveSettingsLink(tab, anchor)
  pushModal({ kind: 'settings', props: { tab: link.section, ...(link.anchor ? { anchor: link.anchor } : {}) } })
}

/**
 * Open Models at the row that installs a model (or Performance at the
 * runtime). What a missing-model notice or refusal offers as its way out.
 *
 * @param {string} id - a logical model id, catalogue file id, file group, or `runtime`
 */
export function openModelSettings(id) {
  const link = settingsLinkForModel(id)
  openSettings(link.section, link.anchor)
}
