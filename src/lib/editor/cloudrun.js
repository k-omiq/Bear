/**
 * Text cleanup with either half on the cloud GPU: the consents, in order.
 *
 * Text cleanup has three modes (docs/detect-clean.md): Detect finds the text
 * regions and stores them, Clean cleans the stored ones, and Detect and clean
 * does both. Each half runs where its own setting says - detection where
 * the tool's Detect on routes the full region finder
 * and the lettering mask, cleaning where Clean on (`cleanTarget`) says - and
 * a half on the cloud does not start on a button press alone:
 *
 * 1. **Refuse in the interface** what the native side would refuse anyway, so
 *    the reason is a sentence and not an error code: a project-wide run (one
 *    consent cannot cover chapters nobody looked at), or a cloud GPU that is
 *    off or not set up. A long strip goes to the cloud like a paginated
 *    chapter: each page is sent on its own, and a window that crosses a join
 *    reads the next page's answer (`run.rs#remote_for_crop`).
 * 2. **Detection asks once, for the run's pages.** Readiness is read fresh,
 *    the native side plans the pages exactly as the run will and proposes
 *    what it would send (no pixels leave yet), and the run consent dialog
 *    shows it. Confirm mints a single-use grant the run spends and throws
 *    away; the run carries that consent to its last page, and no other run
 *    can.
 * 3. **Cleaning asks once, for the exact regions.** With Clean on the cloud,
 *    the run only detects. When it has finished, `prepareCloudClean`
 *    describes the plan and cleans nothing: every region in order, the
 *    batches it is sent in, the pages, the GPU time and cost range from each
 *    region's own crop, and any region left out because an earlier cloud
 *    request for it is unresolved. The cloud clean consent dialog shows that
 *    once for the whole plan, however many batches it takes, and only its
 *    answer mints the grant `startCloudClean` spends. Clean alone skips the
 *    detection half and goes straight to the proposal.
 * 4. **The cloud means the cloud.** With Clean on set to Cloud GPU every
 *    region is cleaned on the cloud GPU, a LaMa pick included: no region is
 *    cleaned by LaMa on this computer (`localFirst: false`), unless
 *    the Text cleanup panel's "Clean flat colours on this computer first"
 *    checkbox is ticked (Run options, shown while Clean on is Cloud GPU; it
 *    sets `session.cleanLocalFirst`, read by `cleanLocalFirst`, off by
 *    default). Then, once the run starts and chunk by chunk, Fill and Solid
 *    colour picks are tried here first, and the consent says so, and that its
 *    cost range still counts every such region as sent. One detected region
 *    cleaned from a Layers row or the region menu does not come through here:
 *    the user chose the cloud for it, so it goes there whatever its pick
 *    (`maskactions.svelte.js#cleanDetectedOnCloud`).
 *
 * The clean consent answers with the plan digest it showed, and the grant is
 * minted only for that plan.
 *
 * Each question is asked once per project: the first consent confirmed in a
 * project, of any of the three kinds, stands for later ones to the same
 * endpoint, and their proposals come back `standing`. That skips the dialog,
 * never the proposal or its single-use grant.
 *
 * There is no quiet fallback: a decline or a failure at any step leaves the
 * regions detected (nothing is lost, they wait in Layers) and says why. A
 * declined proposal is cancelled so it cannot be confirmed later. Running a
 * half on this computer instead is a choice the user makes, never one made
 * for them.
 */

import { askQwenEdit, isQwen, qwenSupported } from './qwenflow.js'
import { getBackend } from '../api/backend.js'
import { outcomeCopy, outcomeOf } from '../dialogs/workflowoutcome.js'
import { cloudCapabilitiesFor } from '../model/pipelines.js'
import { notify, pushModal } from '../state/app.svelte.js'
import { cloudUsable, cloudCleanAvailable, refreshCloudReadiness } from '../state/cloud.svelte.js'
import { configurationKey, configurationFailed } from '../state/cloudconfig.svelte.js'
import { adoptRun, claimRunStart, editor, releaseRunStart, runFinished, startRun } from '../state/editor.svelte.js'
import { finishCloudRun, trackCloudRun } from '../state/cloudgpu.svelte.js'
import { session } from '../state/session.svelte.js'

/**
 * The capabilities the current settings send to the cloud GPU, sorted. Empty
 * while Text cleanup detects on this computer; both cloud stages while it
 * detects on the cloud GPU, whose run uses its own fixed combination
 * (`pipelines.js#runDetection`).
 *
 * @returns {string[]}
 */
export function cloudRunCapabilities() {
  return cloudCapabilitiesFor(session)
}

/**
 * The step the Auto clean tool holds. Anything unreadable is Detect and clean,
 * which is what a run did before there were steps.
 *
 * @param {Record<string, unknown>} [params]
 * @returns {import('../api/backend.js').RunMode}
 */
export function runStep(params = editor.toolParams.autoClean ?? {}) {
  const step = params?.step
  return step === 'detect' || step === 'clean' ? step : 'auto'
}

/**
 * Whether the Text cleanup tool's cloud clean tries flat colours here first:
 * the explicit mixed choice (`toolParams.autoClean.localFirst`). Off unless it
 * is `true`, so a missing or unreadable value keeps the clean strictly on the
 * cloud.
 *
 * @param {Record<string, unknown>} [params]
 * @returns {boolean}
 */
export function cleanLocalFirst(params = editor.toolParams.autoClean ?? {}) {
  return typeof params?.localFirst === 'boolean' ? params.localFirst : session.cleanLocalFirst
}

/**
 * Which halves of a run of this step go to the cloud GPU.
 *
 * @param {import('../api/backend.js').RunMode} [step]
 * @returns {{detect: boolean, clean: boolean}}
 */
export function cloudParts(step = runStep()) {
  return {
    detect: step !== 'clean' && cloudRunCapabilities().length > 0,
    clean: step !== 'detect' && session.cleanTarget === 'cloud',
  }
}

/**
 * Why a run with these cloud halves cannot cover this scope, or null when it
 * can. Detection's reason leads where both halves are refused, because it is
 * the half that would have run first.
 *
 * @param {string} scope
 * @param {{detect: boolean, clean: boolean}} parts
 * @returns {string|null} an i18n key
 */
export function cloudScopeRefusal(scope, parts) {
  if (!parts.detect && !parts.clean) return null
  if (scope !== 'page' && scope !== 'chapter') {
    return parts.detect ? 'cloud.analysis.run.scopeProject' : 'cloud.clean.scopeProject'
  }
  return null
}

/**
 * Why a refusal or failure reads as it does: the error text the adapter
 * threw, matched on its stable code.
 *
 * @param {unknown} cause
 * @returns {string} an i18n key
 */
export function cloudRunErrorKey(cause) {
  const text = errorText(cause)
  if (text.startsWith('cloud_detect_small_unsupported')) return 'tools.target.cloudSmallRefused'
  if (text.startsWith('cloud_run_scope_unsupported')) return 'cloud.analysis.run.scopeProject'
  if (text.startsWith('cloud_run_grant_expired')) return 'cloud.analysis.run.expired'
  if (text.startsWith('cloud_run_too_many_pages')) return 'cloud.analysis.run.tooMany'
  if (text.startsWith('cloud_run_too_large')) return 'cloud.analysis.run.tooLargeChapter'
  if (text.startsWith('cloud_run_no_pages')) return 'cloud.analysis.run.nothing'
  if (text.startsWith('cloud_run_grant') || text.startsWith('cloud_run_page_not_granted')) return 'cloud.analysis.run.consentLost'
  if (text.startsWith('cloud_run_profile_changed')) return 'cloud.analysis.run.profileChanged'
  if (text.startsWith('cloud_disabled')) return 'cloud.analysis.explain.cloudDisabled'
  if (text.includes('gateway tile limits') || text.includes('megapixel') || text.includes('review-preview limit')) {
    return 'cloud.analysis.run.tooLarge'
  }
  if (text.startsWith('capability_unavailable')) return 'cloud.analysis.capabilityMissing'
  if (text.startsWith('analysis_proposal_expired')) return 'cloud.analysis.explain.proposalExpired'
  if (text.startsWith('analysis_proposal_limit')) return 'cloud.analysis.explain.proposalLimit'
  // Everything else as the review dialog says it (a missing key, an inactive
  // endpoint, a gateway without the model), and only what nobody named as
  // the failure to prepare the review.
  return outcomeCopy(outcomeOf(cause, 'propose')).body
}

/**
 * The cloud clean's refusals, by the stable code the native commands answer
 * with (`src-tauri/src/inference/cloud_clean.rs`), each to a plain sentence.
 * Codes that mean the same thing to the reader share one: a missing key, an
 * inactive profile and a bad address are all "not set up". The code is the
 * part before any `: detail`, so `cloud_clean_grant_mismatch: recipe` reads as
 * the mismatch it is.
 */
const CLEAN_ERROR_KEYS = Object.freeze({
  // prepare
  cloud_disabled: 'cloud.analysis.explain.cloudDisabled',
  cloud_clean_profile_not_active: 'cloud.clean.error.notReady',
  credential_missing: 'cloud.clean.error.notReady',
  endpoint_invalid: 'cloud.clean.error.notReady',
  cloud_clean_run_active: 'cloud.clean.busy',
  cloud_clean_scope_unsupported: 'cloud.clean.error.scope',
  cloud_clean_no_pages: 'cloud.clean.nothing',
  cloud_clean_no_regions: 'cloud.clean.nothing',
  cloud_clean_page_not_found: 'cloud.clean.error.gone',
  cloud_clean_region_not_found: 'cloud.clean.error.gone',
  cloud_clean_source_missing: 'cloud.clean.error.sourceMissing',
  cloud_clean_too_many_regions: 'cloud.clean.error.tooMany',
  job_busy: 'notice.job.busy',
  cloud_clean_proposal_limit: 'cloud.clean.error.proposalLimit',
  gateway_unauthorized: 'cloud.clean.error.unauthorized',
  gateway_unreachable: 'cloud.clean.error.unreachable',
  gateway_error: 'cloud.clean.error.gateway',
  gateway_protocol: 'cloud.clean.error.gateway',
  // confirm
  rights_attestation_required: 'cloud.clean.error.statements',
  retention_acknowledgement_required: 'cloud.clean.error.statements',
  cloud_clean_proposal_missing: 'cloud.clean.error.consentGone',
  cloud_clean_proposal_expired: 'cloud.clean.expired',
  cloud_clean_profile_changed: 'cloud.clean.profileChanged',
  cloud_clean_grant_limit: 'cloud.clean.error.proposalLimit',
  cloud_clean_plan_mismatch: 'cloud.clean.error.planChanged',
  // start
  cloud_clean_grant_missing: 'cloud.clean.error.consentGone',
  cloud_clean_grant_expired: 'cloud.clean.expired',
  cloud_clean_grant_mismatch: 'cloud.clean.profileChanged',
  cloud_run_profile_changed: 'cloud.clean.profileChanged',
})

/**
 * @param {unknown} cause
 * @param {string} fallback - the i18n key for the step that failed, for a
 *   code this does not know: never read as a success
 * @returns {string} an i18n key
 */
export function cloudCleanErrorKey(cause, fallback) {
  const code = errorText(cause).split(':')[0].trim()
  return Object.hasOwn(CLEAN_ERROR_KEYS, code) ? CLEAN_ERROR_KEYS[/** @type {keyof typeof CLEAN_ERROR_KEYS} */ (code)] : fallback
}

/** @param {unknown} cause */
function errorText(cause) {
  return typeof cause === 'string' ? cause : String(/** @type {any} */ (cause)?.message ?? '')
}

/**
 * @param {any} proposal
 * @param {'page'|'chapter'} scope
 * @param {boolean} cleanFollows - Clean is on the cloud too: its consent comes later
 * @returns {Promise<{rightsAttested: boolean, retentionAcknowledged: boolean}|string|null>}
 *   the two answers on Send, or the id of the button that dismissed it
 */
function askRunConsent(proposal, scope, cleanFollows) {
  return new Promise((resolve) => {
    pushModal({
      kind: 'cloudRunConsent',
      titleKey: 'cloud.analysis.run.title',
      // As with every cloud consent: a stray backdrop click is not a decision
      // to send a page. Escape still means Cancel.
      blocking: true,
      props: { proposal, scope, cleanFollows },
      actions: [
        { id: 'cancel', labelKey: 'cloud.analysis.consent.cancel' },
        { id: 'confirm', labelKey: 'cloud.analysis.run.confirm', variant: 'primary' },
      ],
      onresolve: resolve,
    })
  })
}

/**
 * @param {import('../api/backend.js').CloudCleanProposal} proposal
 * @param {string|null} endpoint - the endpoint address, as readiness read it
 * @returns {Promise<{rightsAttested: boolean, retentionAcknowledged: boolean, planDigest?: string}|string|null>}
 *   the answers and the plan digest shown, on Confirm
 */
function askCleanConsent(proposal, endpoint) {
  return new Promise((resolve) => {
    pushModal({
      kind: 'cloudCleanConsent',
      titleKey: 'cloud.clean.title',
      blocking: true,
      props: { proposal, endpoint },
      actions: [
        { id: 'cancel', labelKey: 'cloud.analysis.consent.cancel' },
        { id: 'confirm', labelKey: 'cloud.clean.confirm', variant: 'primary' },
      ],
      onresolve: resolve,
    })
  })
}

/**
 * The endpoint address shown in consent, without credentials or query data.
 *
 * @param {any} readiness
 * @returns {string|null}
 */
function endpointHost(readiness) {
  const url = readiness?.profile?.endpointUrl
  if (typeof url !== 'string' || !url) return null
  try {
    const parsed = new URL(url)
    return ['https:', 'http:'].includes(parsed.protocol) ? `${parsed.protocol}//${parsed.host}${parsed.pathname}` : null
  } catch {
    return null
  }
}

/**
 * Readiness, read fresh, when the cloud can be used now; null otherwise.
 *
 * @param {import('../api/backend.js').Backend} backend
 */
async function usableReadiness(backend) {
  const readiness = await refreshCloudReadiness(backend)
  return cloudUsable() && readiness.ready && readiness.target ? readiness : null
}

/**
 * Start Auto clean with the tool's step, asking for consent first for each
 * half that goes to the cloud GPU. With every half local this is `startRun`,
 * with the step as its mode.
 *
 * With Clean on the cloud and a step that detects, what this answers is the
 * detection run's id; the cloud clean follows it once it has finished, on its
 * own consent.
 *
 * @param {'page'|'chapter'|'project'} scope - `page` and `chapter` can go to
 *   the cloud; a project cannot, since each chapter needs its own consent
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<string|null>} the run id, or null when nothing started
 */
export async function startCleanRun(scope = 'page', backend = getBackend()) {
  const token = claimRunStart()
  if (!token) return null
  try {
    const step = runStep()
    const parts = cloudParts(step)
    if (!parts.detect && !parts.clean) return await startRun(scope, { mode: step, pendingToken: token })
    const chapter = editor.chapter
    if (!chapter || editor.run.active) return null
    const refusal = cloudScopeRefusal(scope, parts)
    if (refusal) {
      notify({ key: refusal, tone: 'warn' })
      return null
    }
    const pageIndex = editor.pageIndex
    const readiness = await usableReadiness(backend)
    if (!readiness) {
      notify({ key: parts.detect ? 'cloud.analysis.run.unavailable' : 'cloud.clean.unavailable', tone: 'warn' })
      return null
    }
    if (editor.chapter !== chapter || (scope === 'page' && editor.pageIndex !== pageIndex)) return null
    const params = editor.toolParams.autoClean ?? {}
    const where = {
      chapterId: chapter.id,
      chapter,
      scope: /** @type {'page'|'chapter'} */ (scope),
      pageIndices: scope === 'page' ? [pageIndex] : null,
      localFirst: cleanLocalFirst(),
      // The panel's picks are this clean's: under the mixed choice, a region
      // they set to Fill or Solid colour is tried on this computer first.
      picks: { bubbleEngine: String(params.bubbleEngine ?? 'fill'), outsideEngine: String(params.outsideEngine ?? 'lama') },
    }
    if (step === 'clean') return await cleanOnCloud(where, backend)

    /** @type {string|null} */
    let cloudGrant = null
    if (parts.detect) {
      cloudGrant = await detectionGrant(where.scope, chapter, pageIndex, readiness, backend, parts.clean)
      if (!cloudGrant) return null
    }
    /** @type {string|null} */
    let runId
    try {
      // With the Clean half on the cloud this run only detects: nothing is
      // cleaned here that the user chose to clean there.
      runId = await startRun(scope, { ...(cloudGrant ? { cloudGrant } : {}), mode: parts.clean ? 'detect' : step, pendingToken: token })
    } catch (cause) {
      // A failed page already said so in its own notice.
      const text = errorText(cause)
      if (!text.startsWith('cloud_analysis_failed')) notify({ key: cloudRunErrorKey(cause), tone: 'warn' })
      return null
    }
    if (runId && parts.detect) trackCloudRun(runId, 'analysis', backend, { provider: readiness.target.type, profileId: readiness.target.profile_id })
    if (runId && parts.clean) void cleanAfter(runId, where, backend)
    return runId
  } finally {
    releaseRunStart(token)
  }
}

/**
 * Detect one area of a page again: the selection tool's "Detect text here",
 * for text the page's Detect missed or whose mask was deleted. What is found
 * there is added to the page's detections; nothing the page holds is replaced
 * (`run.rs#DetectArea`).
 *
 * It runs where Text cleanup detects. On the cloud GPU the page is sent as a
 * page Detect sends it, on the same consent.
 *
 * @param {number} pageIndex
 * @param {{x: number, y: number, w: number, h: number}} area - page percent
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<string|null>} the run id, or null when nothing started
 */
export async function detectArea(pageIndex, area, backend = getBackend()) {
  const token = claimRunStart()
  if (!token) return null
  try {
    const chapter = editor.chapter
    if (!chapter || editor.run.active) return null
    const onCloud = cloudRunCapabilities().length > 0
    /** @type {any} */
    let readiness = null
    /** @type {string|null} */
    let cloudGrant = null
    if (onCloud) {
      readiness = await usableReadiness(backend)
      if (!readiness) {
        notify({ key: 'cloud.analysis.run.unavailable', tone: 'warn' })
        return null
      }
      if (editor.chapter !== chapter) return null
      cloudGrant = await detectionGrant('page', chapter, pageIndex, readiness, backend, false)
      if (!cloudGrant) return null
    }
    /** @type {string|null} */
    let runId
    try {
      runId = await startRun('page', { ...(cloudGrant ? { cloudGrant } : {}), mode: 'detect', pendingToken: token, area, pageIndex })
    } catch (cause) {
      // A failed page already said so in its own notice.
      if (!errorText(cause).startsWith('cloud_analysis_failed')) notify({ key: cloudRunErrorKey(cause), tone: 'warn' })
      return null
    }
    if (runId && onCloud) trackCloudRun(runId, 'analysis', backend, { provider: readiness.target.type, profileId: readiness.target.profile_id })
    return runId
  } finally {
    releaseRunStart(token)
  }
}

/**
 * The detection half's consent: propose, ask, confirm. The grant id, or null
 * when anything stopped it - with its notice said, and a declined proposal
 * cancelled.
 *
 * @param {'page'|'chapter'} scope
 * @param {any} chapter
 * @param {number} pageIndex
 * @param {any} readiness
 * @param {import('../api/backend.js').Backend} backend
 * @param {boolean} cleanFollows
 * @returns {Promise<string|null>}
 */
async function detectionGrant(scope, chapter, pageIndex, readiness, backend, cleanFollows) {
  const chapterId = chapter.id
  /** @type {any} */
  let proposal
  try {
    // The native side plans the pages exactly as the run will: the one page,
    // or every page of the chapter the run would clean.
    proposal = await backend.proposeRunAnalysis({
      chapterId,
      scope,
      pageIndices: scope === 'page' ? [pageIndex] : null,
      capabilities: cloudRunCapabilities(),
      provider: readiness.target.type,
      profileId: readiness.target.profile_id,
    })
  } catch (cause) {
    notify({ key: cloudRunErrorKey(cause), tone: 'warn' })
    return null
  }
  if (editor.chapter !== chapter || (scope === 'page' && editor.pageIndex !== pageIndex)) {
    await backend.cancelRunAnalysis({ proposalId: proposal.proposalId }).catch(() => {})
    notify({ key: 'cloud.analysis.run.consentLost', tone: 'warn' })
    return null
  }
  // The project's first consent stands for the rest of it. A standing
  // proposal is confirmed without the statements: the native side holds the
  // ones that consent was given with, and none is answered here for the user.
  const answer = proposal.standing === true
    ? { rightsAttested: false, retentionAcknowledged: false }
    : await askRunConsent(proposal, scope, cleanFollows)
  if (!answer || typeof answer !== 'object') {
    Promise.resolve()
      .then(() => backend.cancelRunAnalysis({ proposalId: proposal.proposalId }))
      .catch(() => {})
    return null
  }
  /** @type {any} */
  let grant
  try {
    grant = await backend.confirmRunAnalysis({
      proposalId: proposal.proposalId,
      rightsAttested: answer.rightsAttested === true,
      retentionAcknowledged: answer.retentionAcknowledged === true,
    })
  } catch (cause) {
    const key = cloudRunErrorKey(cause)
    notify({ key: key === 'cloud.analysis.failure.propose' ? 'cloud.analysis.run.consentFailed' : key, tone: 'warn' })
    return null
  }
  // The chapter, or for a page run the page, may have changed while the
  // dialog was open. The grant names what it was given for, and the native
  // side refuses anything else.
  if (editor.chapter !== chapter || (scope === 'page' && editor.pageIndex !== pageIndex)) {
    notify({ key: 'cloud.analysis.run.consentLost', tone: 'warn' })
    return null
  }
  return grant.grantId
}

/**
 * The second half of Detect and clean with Clean on the cloud: once the
 * detection run has ended, the cloud clean of the same pages. A cancelled run
 * stops here - the user stopped the whole thing, not only its first half -
 * and so does a chapter that is no longer open, whose regions wait detected.
 *
 * @param {string} runId
 * @param {{chapterId: string, chapter: any, scope: 'page'|'chapter', pageIndices: number[]|null, localFirst?: boolean, picks?: {bubbleEngine: string, outsideEngine: string}}} where
 * @param {import('../api/backend.js').Backend} backend
 */
async function cleanAfter(runId, where, backend) {
  const end = await runFinished(runId)
  // Left in the editor, the detection goes on in the background, and the GPU
  // tracker hears its real end on its own subscription (`trackCloudRun`).
  if (end.reason !== 'editor-closed') finishCloudRun(runId, backend)
  if (end.reason === 'editor-closed') {
    notify({ key: 'cloud.clean.leftDetected', tone: 'warn' })
    return null
  }
  if (end.reason !== 'completed') return null
  if (editor.chapter !== where.chapter) {
    notify({ key: 'cloud.clean.leftDetected', tone: 'warn' })
    return null
  }
  const token = claimRunStart()
  if (!token) return null
  try {
    return await cleanOnCloud(where, backend)
  } finally {
    releaseRunStart(token)
  }
}

/**
 * Clean stored detections on the cloud GPU: prepare, ask, confirm, start.
 * One consent covers the whole plan; the native run sends it in bounded
 * batches and stops them all on a cancel or a changed cloud setup.
 *
 * @param {{chapterId: string, chapter: any, scope: 'page'|'chapter', pageIndices?: number[]|null, localFirst?: boolean, picks?: {bubbleEngine?: string, outsideEngine?: string}|null}} where
 *   `localFirst` is the explicit mixed choice; anything but `true` is strictly the cloud.
 *   `picks` are Text cleanup's, saved onto the regions before they are planned.
 * @param {import('../api/backend.js').Backend} backend
 * @returns {Promise<string|null>} the render run's id, or null when nothing started
 */
async function cleanOnCloud(where, backend) {
  const { chapterId, chapter, scope, pageIndices = null, picks = null } = where
  const localFirst = where.localFirst === true
  if (editor.run.active) {
    notify({ key: 'cloud.clean.busy', tone: 'warn' })
    return null
  }
  const readiness = await usableReadiness(backend)
  if (!readiness) {
    notify({ key: 'cloud.clean.unavailable', tone: 'warn' })
    return null
  }
  if (!cloudCleanAvailable()) {
    notify({ key: 'cloud.configuration.cleanMissing', tone: 'warn' })
    return null
  }
  const configKey = configurationKey(readiness)
  if (editor.chapter !== chapter || (scope === 'page' && editor.pageIndex !== pageIndices?.[0])) return null
  /** @type {import('../api/backend.js').CloudCleanProposal} */
  let proposal
  try {
    const model = await backend.getCloudModelInfo({ provider: readiness.target.type, profileId: readiness.target.profile_id })
    let qwenEdit
    if (isQwen(model?.pinnedModelId)) {
      if (!qwenSupported(model)) return null
      qwenEdit = await askQwenEdit({ batch: true })
      if (!qwenEdit) return null
    }
    proposal = await backend.prepareCloudClean({ chapterId, scope, pageIndices, regionIds: null, localFirst,
      ...(picks ?? {}), ...(qwenEdit ? { qwenEdit } : {}) })
  } catch (cause) {
    await configurationFailed(backend, configKey, 'clean', cause)
    notify({ key: cloudCleanErrorKey(cause, 'cloud.clean.failure.prepare'), tone: 'warn' })
    return null
  }
  if (editor.chapter !== chapter || (scope === 'page' && editor.pageIndex !== pageIndices?.[0])) {
    if (proposal?.proposalId) await backend.cancelCloudClean({ proposalId: proposal.proposalId }).catch(() => {})
    notify({ key: 'cloud.clean.consentLost', tone: 'warn' })
    return null
  }
  if (!proposal?.proposalId || !(Number(proposal.regions) > 0)) {
    // Preparing cleaned nothing: what is in scope waits, detected.
    const held = Array.isArray(proposal?.unresolvedIds) ? proposal.unresolvedIds.length : 0
    const tooLarge = Array.isArray(proposal?.tooLargeIds) ? proposal.tooLargeIds.length : 0
    if (held > 0) notify({ key: 'cloud.clean.unresolvedOnly', params: { count: held }, tone: 'warn' })
    if (tooLarge > 0) notify({ key: 'cloud.clean.tooLargeOnly', params: { count: tooLarge }, tone: 'warn' })
    if (held === 0 && tooLarge === 0) notify({ key: 'cloud.clean.nothing', tone: 'warn' })
    return null
  }
  // The project's first consent stands for the rest of it; the plan digest is
  // still the one prepared, so the grant covers exactly this plan. A standing
  // plan is confirmed without the statements: the native side holds the ones
  // that consent was given with, and none is answered here for the user.
  const answer = proposal.standing === true
    ? { rightsAttested: false, retentionAcknowledged: false, planDigest: proposal.planDigest ?? '' }
    : await askCleanConsent(proposal, endpointHost(readiness))
  if (!answer || typeof answer !== 'object') {
    Promise.resolve()
      .then(() => backend.cancelCloudClean({ proposalId: /** @type {string} */ (proposal.proposalId) }))
      .catch(() => {})
    notify({ key: 'cloud.clean.declined' })
    return null
  }
  /** @type {import('../api/backend.js').CloudCleanGrant} */
  let grant
  try {
    grant = await backend.confirmCloudClean({
      proposalId: proposal.proposalId,
      // The digest the dialog showed; anything else is refused natively.
      planDigest: typeof answer.planDigest === 'string' ? answer.planDigest : '',
      rightsAttested: answer.rightsAttested === true,
      retentionAcknowledged: answer.retentionAcknowledged === true,
    })
  } catch (cause) {
    notify({ key: cloudCleanErrorKey(cause, 'cloud.clean.failure.confirm'), tone: 'warn' })
    return null
  }
  // The grant names this chapter's regions; the run's events are only read
  // for the open chapter, so a chapter changed meanwhile starts nothing.
  if (editor.chapter !== chapter || (scope === 'page' && editor.pageIndex !== pageIndices?.[0])) {
    notify({ key: 'cloud.clean.consentLost', tone: 'warn' })
    return null
  }
  if (editor.run.active) {
    notify({ key: 'cloud.clean.busy', tone: 'warn' })
    return null
  }
  try {
    const handle = await backend.startCloudClean({ grantId: grant.grantId })
    // The chapter got a run meanwhile, or as many runs as the backend allows
    // are going: the native side leaves the grant unspent. It is not ours to
    // adopt.
    if (handle?.atCapacity) {
      notify({ key: 'notice.run.atCapacity', tone: 'warn' })
      return null
    }
    if (handle?.alreadyRunning) {
      notify({ key: 'cloud.clean.busy', tone: 'warn' })
      return null
    }
    const runId = adoptRun(handle, scope === 'chapter' ? 'chapter' : 'page', 'clean', 'cloudClean')
    if (runId) trackCloudRun(runId, 'render', backend, { provider: proposal.provider, profileId: proposal.profileId || readiness.target.profile_id })
    return runId
  } catch (cause) {
    await configurationFailed(backend, configKey, 'clean', cause)
    notify({ key: cloudCleanErrorKey(cause, 'cloud.clean.failure.start'), tone: 'warn' })
    return null
  }
}
