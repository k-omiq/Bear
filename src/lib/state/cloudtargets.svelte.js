/**
 * Where Text cleanup's two halves run, and why the cloud cannot be chosen when it
 * cannot: one owner for both surfaces that ask.
 *
 * The Text cleanup panel owns *Detect on* and *Clean on*. They persist in
 * the same two settings the native runner reads, `analysisTargets` and
 * `cleanTarget`, and the reason a Cloud GPU entry is disabled has to read the
 * same wherever it is shown. So the reasoning lives here and nowhere else:
 *
 * - **What the endpoint offers.** The cloud-capable detection models are
 *   offered by the endpoint or not, and the only way to know is to ask it for
 *   its model list (metadata, no page leaves the computer). The answer is kept
 *   per endpoint, so switching endpoints asks again. A component that shows a
 *   detection choice calls `syncCloudOffer()` from an `$effect` of its own.
 * - **Why not.** `stageReasonKey` for one model, `detectReasonKey` for the
 *   tool's single *Detect on* (both cloud-capable models at once: a cloud run
 *   always uses both, `pipelines.js#runDetection`), `cleanReasonKey` for
 *   *Clean on*. Each is an i18n key or null.
 * - **Saving.** Every choice is written where the native run reads it
 *   (`writeSettings`) and put back when the save fails, so a control never
 *   shows a place the next run would not use.
 *
 * Nothing here sends anything. Choosing the cloud only changes where the next
 * run *asks* to send (`editor/cloudrun.js`).
 */

import { untrack } from 'svelte'
import { getBackend } from '../api/backend.js'
import { CLOUD_STAGES, detectsOnCloud } from '../model/pipelines.js'
import { cloud, cloudCleanAvailable, cloudUsable, refreshCloudReadiness } from './cloud.svelte.js'
import { session, setAnalysisTarget, setCleanTarget } from './session.svelte.js'
import { savedSetting, writeSettingsSerialized } from './settingswrite.js'

export const targets = $state({
  /**
   * Which analysis models the default cloud endpoint offers, as it answered.
   * Keyed by endpoint, so choosing another endpoint asks again.
   *
   * @type {{key: string, state: 'checking'|'ready'|'failed', offered: Set<string>}|null}
   */
  offer: null,
  /** The last detection choice could not be saved, and was put back. */
  detectSaveFailed: false,
  /** The last Clean on choice could not be saved, and was put back. */
  cleanSaveFailed: false,
})

/** The endpoint the offer is about, or null while none is usable. */
export function cloudTargetKey() {
  const target = cloud.readiness?.target
  const profile = cloud.readiness?.profile
  return cloudUsable() && target ? JSON.stringify([target.type, target.profile_id, profile?.endpointUrl ?? '', profile?.updatedAtMs ?? '']) : null
}

/**
 * Ask the endpoint what it offers, when the cloud GPU could be chosen or is
 * chosen, and the answer for this endpoint is not in hand. A cloud run uses
 * both cloud-capable models whatever this computer's selection holds, so
 * there is always something to ask about. Call it from an `$effect`: it reads
 * what should make it ask again, and does the asking untracked.
 */
export function syncCloudOffer() {
  const key = cloudTargetKey()
  if (!session.cloudAllowed && detectTarget() !== 'cloud') return
  untrack(() => {
    if (!cloud.checked) void refreshCloudReadiness()
    else if (key && targets.offer?.key !== key) void readCloudOffer(key)
  })
}

/** @param {string} key */
async function readCloudOffer(key) {
  const target = cloud.readiness?.target
  if (!target) return
  targets.offer = { key, state: 'checking', offered: new Set() }
  try {
    const listed = await getBackend().listRemoteAnalysisCapabilities({ provider: target.type, profileId: target.profile_id })
    if (targets.offer?.key !== key) return
    targets.offer = { key, state: 'ready', offered: new Set((listed?.capabilities ?? []).map((entry) => entry?.capability)) }
  } catch {
    if (targets.offer?.key === key) targets.offer = { key, state: 'failed', offered: new Set() }
  }
}

/** Forget the endpoint's answer: the next `syncCloudOffer` asks again. */
export function resetCloudOffer() {
  targets.offer = null
}

/**
 * Why Cloud GPU cannot be chosen for one detection model, or null when it can.
 *
 * @param {string} stageId - `rtFull` or `samTs`
 * @returns {string|null} an i18n key
 */
export function stageReasonKey(stageId) {
  if (!session.cloudAllowed) return 'settings.detection.runOn.off'
  if (!cloudUsable()) return 'settings.detection.runOn.notReady'
  const offer = targets.offer
  if (!offer || offer.key !== cloudTargetKey() || offer.state === 'checking') return 'settings.detection.runOn.checking'
  if (offer.state === 'failed') return 'settings.detection.runOn.unread'
  const capability = CLOUD_STAGES.find((stage) => stage.id === stageId)?.capability
  return capability && offer.offered.has(capability) ? null : 'settings.detection.runOn.notOffered'
}

/**
 * Why the tool's *Detect on: Cloud GPU* cannot be chosen, or null when it can.
 * A cloud run uses both cloud-capable models, so it is the first reason
 * either of them has.
 *
 * @returns {string|null} an i18n key
 */
export function detectReasonKey() {
  for (const stage of CLOUD_STAGES) {
    const reason = stageReasonKey(stage.id)
    if (reason) return reason
  }
  return null
}

/**
 * Why *Clean on: Cloud GPU* cannot be chosen, or null when it can. Nothing is
 * said about readiness before it has been read once, so the note never claims
 * an endpoint is missing that is merely not asked about yet.
 *
 * @returns {string|null} an i18n key
 */
export function cleanReasonKey() {
  if (!session.cloudAllowed) return 'settings.detection.runOn.off'
  if (!cloudCleanAvailable()) return 'cloud.configuration.cleanMissing'
  if (!cloud.checked || cloudUsable()) return null
  return 'settings.detection.runOn.notReady'
}

/**
 * Where detection runs, as the tool's single choice reads it
 * (`pipelines.js#detectsOnCloud`). It no longer depends on which models this
 * computer's selection holds: a cloud run uses its own fixed combination.
 *
 * @returns {'local'|'cloud'}
 */
export function detectTarget() {
  return detectsOnCloud(session.analysisTargets) ? 'cloud' : 'local'
}

/**
 * Save `analysisTargets` as the session now holds it, or put `previous` back.
 *
 * @param {{rtFull: 'local'|'cloud', samTs: 'local'|'cloud'}} previous
 * @returns {Promise<boolean>} whether it was saved
 */
let analysisChoice = 0
let cleanChoice = 0
const analysisInitial = new WeakMap()
const cleanInitial = new WeakMap()

async function saveAnalysisTargets(previous) {
  const backend = getBackend()
  if (!analysisInitial.has(backend)) analysisInitial.set(backend, previous)
  const wanted = { ...session.analysisTargets }
  const choice = ++analysisChoice
  targets.detectSaveFailed = false
  try {
    await writeSettingsSerialized(backend, { analysisTargets: wanted })
    return true
  } catch {
    if (choice === analysisChoice && CLOUD_STAGES.every((stage) =>
      session.analysisTargets[/** @type {'rtFull'|'samTs'} */ (stage.id)] === wanted[/** @type {'rtFull'|'samTs'} */ (stage.id)])) {
      const restored = savedSetting(backend, 'analysisTargets', analysisInitial.get(backend))
      for (const stage of CLOUD_STAGES) setAnalysisTarget(stage.id, restored[/** @type {'rtFull'|'samTs'} */ (stage.id)])
      targets.detectSaveFailed = true
    }
    return false
  }
}

/**
 * Where one detection model runs, as chosen by Text cleanup.
 *
 * @param {string} stageId
 * @param {string} target
 * @returns {Promise<boolean>} whether anything was saved
 */
export async function chooseStageTarget(stageId, target) {
  const previous = { ...session.analysisTargets }
  if (previous[/** @type {'rtFull'|'samTs'} */ (stageId)] === target) return false
  setAnalysisTarget(stageId, target)
  return saveAnalysisTargets(previous)
}

/**
 * Where detection runs, from the tool: both cloud-capable models at once,
 * selected or not, so a model selected later does not quietly run elsewhere.
 *
 * @param {string} target - `local` or `cloud`
 * @returns {Promise<boolean>} whether anything was saved
 */
export async function chooseDetectTarget(target) {
  if (target !== 'local' && target !== 'cloud') return false
  const previous = { ...session.analysisTargets }
  if (CLOUD_STAGES.every((stage) => previous[/** @type {'rtFull'|'samTs'} */ (stage.id)] === target)) return false
  for (const stage of CLOUD_STAGES) setAnalysisTarget(stage.id, target)
  return saveAnalysisTargets(previous)
}

/**
 * Where stored detections are cleaned, from the tool or from Settings.
 *
 * @param {string} target - `local` or `cloud`
 * @returns {Promise<boolean>} whether anything was saved
 */
export async function chooseCleanTarget(target) {
  if (target !== 'local' && target !== 'cloud') return false
  const previous = session.cleanTarget
  if (previous === target) return false
  targets.cleanSaveFailed = false
  const backend = getBackend()
  if (!cleanInitial.has(backend)) cleanInitial.set(backend, previous)
  const choice = ++cleanChoice
  setCleanTarget(target)
  try {
    await writeSettingsSerialized(backend, { cleanTarget: target })
    return true
  } catch {
    if (choice === cleanChoice && session.cleanTarget === target) {
      setCleanTarget(savedSetting(backend, 'cleanTarget', cleanInitial.get(backend)))
      targets.cleanSaveFailed = true
    }
    return false
  }
}
