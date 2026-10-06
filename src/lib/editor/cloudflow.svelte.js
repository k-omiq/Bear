/**
 * The cloud gate, and the one consent question in front of every cloud render.
 *
 * Three steps, in this order, for every action that would send a region:
 *
 * 1. **Refuse while cloud is off** (`cloudRefused`), in the interface, before
 *    any adapter call. `notice.cloud.blocked` says nothing was sent. The
 *    backend's own refusal stays in place as the second line of defence.
 * 2. **Ask once** (`requestCloudConsent`). Readiness is read fresh, the native
 *    side prepares a proposal for exactly this region, and one dialog says what
 *    is sent (a crop, not the page), where (the endpoint the proposal names:
 *    its saved name, or its provider and host) and what it costs (the
 *    estimate, or that there is none). The grant is minted only after
 *    Confirm, and covers one attempt at this one action.
 * 3. **Run it where it can be watched** (`runCloudJob`). The render is shown
 *    in the status element from before its first event, so Cancel works from
 *    the first moment, and it ends in exactly one notice.
 */

import { askQwenEdit, isQwen, qwenSupported, takeQwenRetry } from './qwenflow.js'
import { getBackend } from '../api/backend.js'
import { cloudAttemptId } from '../api/attempt.js'
import { notify, pushModal } from '../state/app.svelte.js'
import { session } from '../state/session.svelte.js'
import { cloud, cloudCleanAvailable, refreshCloudReadiness, settleCloudJob, trackCloudJob } from '../state/cloud.svelte.js'
import { configurationKey, configurationFailed } from '../state/cloudconfig.svelte.js'
import { toolSpendsCloud } from './tools.js'

/**
 * Would this call spend money, and is that switched off? Refuses in the
 * interface, before anything is sent.
 *
 * @param {string} tool
 * @param {Record<string, unknown>} params
 * @returns {boolean} true when the caller must not proceed
 */
export function cloudRefused(tool, params) {
  if (!toolSpendsCloud(tool, params)) return false
  if (session.cloudAllowed) return false
  notify({ key: 'notice.cloud.blocked', tone: 'warn' })
  return true
}

/**
 * What a confirmed consent hands the command that renders: the four fields
 * every cloud-capable command reads, and the attempt id the render will carry.
 *
 * @typedef {Object} CloudGrant
 * @property {import('../api/backend.js').CloudRunParams} params
 * @property {string} attemptId
 */

/**
 * The host an endpoint URL names, or '' when it does not parse.
 *
 * @param {unknown} url
 * @returns {string}
 */
function hostOf(url) {
  try {
    return new URL(String(url)).host
  } catch {
    return ''
  }
}

/**
 * Where a proposal sends the crop, as the dialog names it. The proposal says
 * which endpoint (provider and profile id) and its address; the name is the
 * one saved with that endpoint on this computer, read with readiness. With no
 * saved name the dialog says the provider and the host instead.
 *
 * @param {any} proposal
 * @param {import('../api/backend.js').CloudReadiness} readiness
 * @returns {{provider: 'modal'|'beam', name: string, host: string}}
 */
function destinationOf(proposal, readiness) {
  const target = /** @type {import('../api/backend.js').ExecutionTarget} */ (readiness.target)
  const provider = proposal.provider === 'modal' || proposal.provider === 'beam' ? proposal.provider : target.type
  const profileId = typeof proposal.profileId === 'string' && proposal.profileId ? proposal.profileId : target.profile_id
  const saved = readiness.endpoints.find((endpoint) => endpoint.provider === provider && endpoint.id === profileId)
  const url =
    typeof proposal.endpointUrl === 'string' && proposal.endpointUrl
      ? proposal.endpointUrl
      : (saved?.endpointUrl ?? readiness.profile?.endpointUrl)
  return { provider, name: saved?.name ?? '', host: hostOf(url) }
}

/**
 * Put the consent dialog up and wait for its answer.
 *
 * @param {Record<string, unknown>} props
 * @returns {Promise<{rightsAttested: boolean, retentionAcknowledged: boolean}|null>} the two statements
 *   'confirm' answered with, or null for any other answer
 */
function askConsent(props) {
  return new Promise((resolve) => {
    pushModal({
      kind: 'cloudConsent',
      titleKey: 'modal.title.cloudConsent',
      // A stray backdrop click is not a decision to send a scan to a third
      // party. Escape still means Cancel.
      blocking: true,
      props,
      actions: [
        { id: 'cancel', labelKey: 'shell.action.cancel' },
        { id: 'confirm', labelKey: 'shell.action.confirmSpend', variant: 'primary' },
      ],
      onresolve: (/** @type {any} */ answer) => resolve(answer && typeof answer === 'object'
        ? { rightsAttested: answer.rightsAttested === true, retentionAcknowledged: answer.retentionAcknowledged === true }
        : null),
    })
  })
}

/**
 * The notice for a proposal the native side refused, by its stable code: a
 * region too large for the render service says so, anything else that the
 * request could not be prepared.
 *
 * @param {unknown} cause
 * @returns {string} an i18n key
 */
function consentFailureKey(cause) {
  const text = typeof cause === 'string' ? cause : String(/** @type {any} */ (cause)?.message ?? '')
  return text.startsWith('cloud_consent_crop_too_large') ? 'cloud.clean.regionTooLarge' : 'notice.cloud.consentFailed'
}

/**
 * Ask for consent to render one region in the cloud.
 *
 * Nothing is sent before the answer: the model information is metadata read
 * from the endpoint, and the proposal is prepared on this machine. A refusal,
 * a dismissal or any failure answers `null` and leaves nothing behind but a
 * notice; the caller then does nothing.
 *
 * The question is asked once per project: confirming it with both statements
 * records a standing consent for that endpoint, and a later proposal in the
 * same project comes back `standing`. That skips the dialog only; each region
 * still gets its own proposal and single-use grant, confirmed without the
 * statements, which the native side holds from the first.
 *
 * @param {{qwenEdit?: any, regionId: string, chapterId: string, pageIndex: number, intent: import('../model/types.js').OperationIntent}} request
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<CloudGrant|null>}
 */
export async function requestCloudConsent(request, backend = getBackend()) {
  const readiness = await refreshCloudReadiness(backend)
  if (!readiness.ready || !readiness.target || !readiness.profile) {
    notify({ key: readiness.allowed ? 'notice.cloud.notReady' : 'notice.cloud.blocked', tone: 'warn' })
    return null
  }
  const target = readiness.target
  if (!cloudCleanAvailable()) {
    notify({ key: 'cloud.configuration.cleanMissing', tone: 'warn' })
    return null
  }
  const configKey = configurationKey(readiness)

  /** @type {import('../model/types.js').RenderRecipe} */
  let recipe
  /** @type {any} */
  let proposal
  try {
    const model = await backend.getCloudModelInfo({ provider: target.type, profileId: target.profile_id })
    cloud.model = { id: model.pinnedModelId, provider: target.type, profileId: target.profile_id,
      updatedAtMs: readiness.profile.updatedAtMs ?? null }
    recipe = {
      recipe_id: model.pinnedRecipeId,
      preprocessing_version: model.preprocessingVersion,
      model_id: model.pinnedModelId,
      model_revision: model.pinnedModelRevision,
      native_mask_conditioning: model.nativeMaskConditioning,
    }
    if (isQwen(recipe.model_id)) {
      if (!qwenSupported(model)) return null
      const edit = await askQwenEdit({ regionId: request.regionId, initial: request.qwenEdit })
      if (!edit) return null
      recipe.qwen_edit = edit
    }
    proposal = await backend.prepareCloudConsent({
      target,
      recipe,
      intent: request.intent,
      regionId: request.regionId,
      chapterId: request.chapterId,
      pageIndex: request.pageIndex,
    })
  } catch (cause) {
    await configurationFailed(backend, configKey, 'clean', cause)
    notify({ key: consentFailureKey(cause), tone: 'warn' })
    return null
  }
  if (!proposal || typeof proposal.proposalId !== 'string') {
    notify({ key: 'notice.cloud.consentFailed', tone: 'warn' })
    return null
  }

  /** @type {{rightsAttested: boolean, retentionAcknowledged: boolean}} */
  let statements = { rightsAttested: false, retentionAcknowledged: false }
  if (proposal.standing !== true) {
    const where = destinationOf(proposal, readiness)
    const answer = await askConsent({
      provider: where.provider,
      profileName: where.name,
      host: where.host,
      width: proposal.rect?.w ?? 0,
      height: proposal.rect?.h ?? 0,
      estimatedCostUsd: proposal.estimatedCostUsd ?? null,
    })
    if (!answer) return null
    statements = answer
  }

  /** @type {any} */
  let grant
  try {
    grant = await backend.confirmCloudConsent({ proposalId: proposal.proposalId, intent: request.intent, ...statements })
  } catch {
    notify({ key: 'notice.cloud.consentFailed', tone: 'warn' })
    return null
  }
  if (typeof grant?.nonce !== 'string' || !grant.nonce) {
    notify({ key: 'notice.cloud.consentFailed', tone: 'warn' })
    return null
  }
  return {
    params: { grantNonce: grant.nonce, executionTarget: target, recipe, intent: request.intent },
    attemptId: cloudAttemptId(grant.nonce),
  }
}

/**
 * How an `applyTool` answer to a cloud render ended, for the status element.
 *
 * @param {any} result
 * @returns {import('../state/cloud.svelte.js').CloudOutcome|null}
 */
export function cloudOutcomeOf(result) {
  switch (result?.status) {
    case 'applied':
      return { phase: 'committed' }
    case 'failed':
    case 'cancelled':
    case 'unknown':
      return { phase: result.status, errorCode: result.errorCode ?? null }
    // The backend refused because cloud went off, and said so itself.
    case 'blocked':
      return { phase: 'failed', errorCode: 'cloud_disabled', quiet: true }
    case 'not-found':
      return { phase: 'failed', errorCode: 'region_not_found' }
    default:
      return null
  }
}

const ERROR_CODE = /^[a-z][a-z_]{0,47}$/

/**
 * The stable code a rejected call carries, when it carries one. The browser
 * mock rejects with the code as the message; the native side answers with it,
 * and its rejections are prose, which reads as no code at all.
 *
 * @param {unknown} error
 * @returns {string|null}
 */
function codeOf(error) {
  const text = error instanceof Error ? error.message : typeof error === 'string' ? error : ''
  return ERROR_CODE.test(text) ? text : null
}

/**
 * Run the command a grant was minted for, shown in the status element from
 * before its first event and ended with one notice.
 *
 * @template T
 * @param {CloudGrant} grant
 * @param {{regionId: string, chapterId: string, pageIndex: number}} where
 * @param {(params: import('../api/backend.js').CloudRunParams) => Promise<T>} call
 * @param {(answer: T) => import('../state/cloud.svelte.js').CloudOutcome|null} outcomeOf -
 *   how the answer ended, or null when only the event can say
 * @returns {Promise<T|null>} the command's answer, or null when it threw
 */
export async function runCloudJob(grant, where, call, outcomeOf) {
  while (true) {
    trackCloudJob({ attemptId: grant.attemptId, ...where, target: {
      provider: grant.params.executionTarget.type, profileId: grant.params.executionTarget.profile_id,
    } })
    /** @type {T|null} */
    let answer = null
    let rejected = false
    try {
      answer = await call(grant.params)
    } catch (error) {
      rejected = true
      settleCloudJob(grant.attemptId, { phase: 'failed', errorCode: codeOf(error) })
    }
    if (!rejected) settleCloudJob(grant.attemptId, outcomeOf(/** @type {T} */ (answer)))
    const retry = takeQwenRetry(grant.attemptId)
    if (!retry) return answer
    const next = await requestCloudConsent({ ...where, intent: grant.params.intent, qwenEdit: retry })
    if (!next) return answer
    grant = next
  }
}
