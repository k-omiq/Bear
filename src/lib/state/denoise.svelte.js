/**
 * Page denoise's shared state: what the backend says it can run, the local
 * measurement in flight, and the one way the three choices are saved.
 *
 * Onboarding, Settings > Denoise and the chapter dialog all read and write
 * through here, so a preset chosen in one is the preset the others show, and
 * a measurement started in Settings is still "measuring" if the dialog opens.
 */

import { getBackend } from '../api/backend.js'
import { writeSettingsSerialized } from './settingswrite.js'
import {
  backendSettingsPatch,
  session,
  setDenoiseLocalSeconds,
  setDenoisePreset,
  setDenoiseTarget,
} from './session.svelte.js'
import { benchmarkSeconds, normalizeDenoiseTarget, presetsFor } from '../model/denoise.js'
import { cloud, refreshCloudReadiness } from './cloud.svelte.js'
import { checkConfiguration, configurationBelongsTo, configurationKey, rememberedConfiguration } from './cloudconfig.svelte.js'

export const denoise = $state({
  /**
   * The backend's own preset list (`denoise_presets`), or null until it
   * answers or when it cannot. Null means the shared table stands.
   *
   * @type {Array<{id: string, targets: string[], recipe: Object}>|null}
   */
  known: null,
  /** Whether `benchmark_denoise_local` is running. */
  measuring: false,
  /** Whether the last measurement failed. */
  measureFailed: false,
  /**
   * What the selected cloud deployment says about page denoise
   * (`cloud_denoise_presets`):
   *
   * - `unknown`: not asked yet, or cloud use is off.
   * - `checking`: asking.
   * - `ready`: it runs the presets in `cloudPresets`.
   * - `missing`: it was set up without page denoise (or on Beam).
   * - `noProfile`: no cloud GPU is selected.
   * - `unreachable`: it could not be asked. Cloud stays offered; a run says why it fails.
   *
   * @type {'unknown'|'checking'|'ready'|'missing'|'noProfile'|'unreachable'}
   */
  cloud: 'unknown',
  /** @type {string[]} */
  cloudPresets: [],
  cloudKey: null,
})

let asked = false
let cloudSeq = 0

/** Ask the backend which presets it knows, once per session. */
export async function loadDenoisePresets() {
  if (asked) return
  asked = true
  try {
    const list = await getBackend().denoisePresets()
    denoise.known = Array.isArray(list) && list.length ? list : null
  } catch {
    // An older backend without the command: the shared table stands.
    denoise.known = null
  }
}

/**
 * Ask the selected cloud deployment which presets it can run. Sends no page
 * and starts no GPU. Only the newest answer is kept.
 */
export async function checkCloudDenoise(force = false) {
  const mine = ++cloudSeq
  if (!session.cloudAllowed) {
    denoise.cloud = 'unknown'
    denoise.cloudPresets = []
    return
  }
  const backend = getBackend()
  if ((!cloud.checked || !configurationBelongsTo(backend)) && backend.readInferenceConfig) await refreshCloudReadiness(backend)
  const key = configurationKey(cloud.readiness)
  denoise.cloudKey = key
  denoise.cloud = 'checking'
  let next = /** @type {typeof denoise.cloud} */ ('ready')
  let presets = /** @type {string[]} */ ([])
  try {
    const target = cloud.readiness.target
    const known = key ? await checkConfiguration(backend, key, 'denoise', async () => ({ state: 'ready', modelId: null,
      presets: await backend.cloudDenoisePresets({ provider: target.type, profileId: target.profile_id }) }), force) : null
    if (key && !known) throw new Error('gateway_unreachable')
    if (known?.state === 'missing') next = 'missing'
    const listed = key ? known?.presets : await backend.cloudDenoisePresets()
    presets = Array.isArray(listed) ? listed.filter((id) => typeof id === 'string') : []
  } catch (error) {
    const code = String(error instanceof Error ? error.message : error ?? '').replace(/^Error:\s*/, '')
    next = code.startsWith('capability_unavailable: gateway denoise is not configured') ? 'missing'
      : code.startsWith('cloud_denoise_profile') ? 'noProfile'
      : 'unreachable'
  }
  if (mine !== cloudSeq || key !== configurationKey(cloud.readiness)) return
  denoise.cloud = next
  denoise.cloudPresets = presets
}

/** Whether Cloud can be chosen: cloud use is on and the deployment did not say no. */
export function cloudOffered() {
  const state = selectedCloudState()
  return session.cloudAllowed && state !== 'missing' && state !== 'noProfile'
}

function selectedCloudState() {
  if (!configurationBelongsTo(getBackend()) && configurationKey(cloud.readiness)) return 'unknown'
  const key = configurationKey(cloud.readiness)
  return rememberedConfiguration(key)?.denoise?.state ?? (key === denoise.cloudKey ? denoise.cloud : 'unknown')
}

/**
 * Why Cloud cannot be chosen, or may not work, as a whole key; null when
 * nothing needs saying.
 *
 * @returns {string|null}
 */
export function cloudNote() {
  const state = selectedCloudState()
  if (!session.cloudAllowed || state === 'noProfile') return 'denoise.target.cloudUnavailable'
  if (state === 'missing') return 'denoise.target.cloudNotSetUp'
  if (state === 'unreachable') return 'denoise.target.cloudUnchecked'
  return null
}

/**
 * The presets a target offers here. For Cloud, once the deployment has
 * answered, only the presets it says it can run. An empty answer is an older
 * deployment that does not list them, and the table stands.
 *
 * @param {string} target
 */
export function offeredPresets(target) {
  const offered = presetsFor(target, denoise.known)
  const remembered = configurationBelongsTo(getBackend()) ? rememberedConfiguration(configurationKey(cloud.readiness))?.denoise : null
  const presets = remembered?.presets ?? denoise.cloudPresets
  if (target !== 'cloud' || selectedCloudState() !== 'ready' || !presets.length) return offered
  return offered.filter((preset) => presets.includes(preset.id))
}

/** The stored preset, resolved for a target. @param {string} target */
export function currentPreset(target) {
  const offered = offeredPresets(target)
  return offered.find((preset) => preset.id === session.denoisePreset)?.id ?? offered[0]?.id ?? null
}

/**
 * Save where denoise runs and which preset, together, the way the rest of
 * setup saves: the session first, then the backend; a refused write puts
 * both back. The preset is resolved for the target, so a stored choice is
 * always one that target offers.
 *
 * @param {{target?: string, preset?: string}} choice
 * @returns {Promise<boolean>} whether the backend kept it
 */
export async function saveDenoiseChoice({ target, preset } = {}) {
  const previous = { target: session.denoiseTarget, preset: session.denoisePreset }
  const nextTarget = normalizeDenoiseTarget(target ?? session.denoiseTarget)
  const offered = offeredPresets(nextTarget)
  const wanted = preset ?? session.denoisePreset
  const nextPreset = offered.find((entry) => entry.id === wanted)?.id ?? offered[0]?.id ?? session.denoisePreset
  setDenoiseTarget(nextTarget)
  setDenoisePreset(nextPreset)
  try {
    await writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
    return true
  } catch {
    setDenoiseTarget(previous.target)
    setDenoisePreset(previous.preset)
    return false
  }
}

/**
 * Measure how long one page takes on this computer and keep the answer.
 * A second press while one runs does nothing.
 *
 * @returns {Promise<number|null>} seconds per page, or null when it failed
 */
export async function measureLocalDenoise() {
  if (denoise.measuring) return null
  denoise.measuring = true
  denoise.measureFailed = false
  try {
    const answer = await getBackend().benchmarkDenoiseLocal({ presetId: currentPreset('local') ?? undefined })
    const seconds = benchmarkSeconds(answer)
    if (seconds === null) throw new Error('denoise_benchmark_unreadable')
    setDenoiseLocalSeconds(seconds)
    // A refused write keeps the figure in the session: it is still what this
    // computer measured, and the next save of any setting carries it.
    await writeSettingsSerialized(getBackend(), () => backendSettingsPatch()).catch(() => {})
    return seconds
  } catch {
    denoise.measureFailed = true
    return null
  } finally {
    denoise.measuring = false
  }
}

/** Put the module back as it was. **Tests only.** */
export function resetDenoiseState() {
  asked = false
  denoise.known = null
  denoise.measuring = false
  denoise.measureFailed = false
  denoise.cloud = 'unknown'
  denoise.cloudPresets = []
  cloudSeq = 0
  denoise.cloudKey = null
}
