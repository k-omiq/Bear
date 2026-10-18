/**
 * Why a cloud render did not finish, in words, and the cloud clean run's own
 * notices made readable.
 *
 * The native side names every failure by a stable code
 * (`src-tauri/src/inference/service.rs#InferenceServiceError::code`), and two
 * notices say it: the batch's (`src-tauri/src/inference/cloud_clean.rs`: a
 * region it could not clean, a batch it stopped), which puts a clause in the
 * middle of its sentence, and a single render's (`state/cloud.svelte.js`),
 * which ends with a sentence of its own. Both read `CLOUD_FAILURES`, so a code
 * has words in one exactly when it has them in the other, and a code the table
 * does not know still gets a reason: that the app has no words for it. The
 * code itself is never printed.
 *
 * Every other notice passes through unchanged.
 */

/**
 * @typedef {Object} CloudFailure
 * @property {string} words - the clause the batch notice says it in
 * @property {string} [sentence] - the sentence a single render's notice ends
 *   with. Absent only where no single render can end with the code.
 */

const TARGET = { words: 'notice.cloudClean.code.profileChanged', sentence: 'notice.cloud.error.target' }
const REGION = { words: 'notice.cloudClean.code.regionChanged', sentence: 'notice.cloud.error.region' }
const BUSY = { words: 'notice.cloudClean.code.busy', sentence: 'notice.cloud.error.busy' }
const LOCAL = { words: 'notice.cloudClean.code.local', sentence: 'notice.cloud.error.local' }
const RESULT = { words: 'notice.cloudClean.code.gatewayProtocol', sentence: 'notice.cloud.error.result' }

/**
 * Every code a cloud render or a cloud clean batch can fail with. Codes that
 * mean the same thing to the person reading the notice share a row.
 *
 * @type {Readonly<Record<string, Readonly<CloudFailure>>>}
 */
export const CLOUD_FAILURES = Object.freeze({
  cloud_disabled: { words: 'notice.cloudClean.code.cloudDisabled', sentence: 'notice.cloud.error.disabled' },
  cloud_run_profile_changed: TARGET,
  target_invalid: TARGET,
  target_changed: TARGET,
  profile_missing: TARGET,
  endpoint_invalid: TARGET,
  provider_paused: { words: 'notice.cloudClean.code.providerPaused', sentence: 'notice.cloud.error.providerPaused' },
  credential_missing: { words: 'notice.cloudClean.code.credentialMissing', sentence: 'notice.cloud.error.credential' },
  consent_invalid: { words: 'notice.cloudClean.code.consentInvalid', sentence: 'notice.cloud.error.consent' },
  region_not_found: REGION,
  region_changed: REGION,
  region_unsupported: { words: 'notice.cloudClean.code.regionUnsupported', sentence: 'notice.cloud.error.regionUnsupported' },
  attempt_busy: BUSY,
  attempt_exists: BUSY,
  // A run is on the region's page (`run::lock_page`): the edit's own words.
  page_busy: { words: 'notice.cloudClean.code.pageBusy', sentence: 'notice.run.pageBusy' },
  recovery_required: { words: 'notice.cloudClean.code.recoveryRequired', sentence: 'cloud.recovery.unresolved' },
  journal_error: LOCAL,
  journal_unavailable: LOCAL,
  project_error: LOCAL,
  config_unreadable: LOCAL,
  invalid_request: LOCAL,
  result_invalid: RESULT,
  gateway_protocol: RESULT,
  gateway_unauthorized: { words: 'notice.cloudClean.code.gatewayUnauthorized', sentence: 'notice.cloud.error.unauthorized' },
  gateway_error: { words: 'notice.cloudClean.code.gatewayError', sentence: 'notice.cloud.error.gateway' },
  gateway_unreachable: { words: 'notice.cloudClean.code.gatewayUnreachable', sentence: 'notice.cloud.error.unreachable' },
  submission_unknown: { words: 'notice.cloudClean.code.submissionUnknown', sentence: 'notice.cloud.error.submission' },
  // Only a cloud setup deployed before the worker stopped rejecting results
  // sends this: it returned no image when its own edit check failed.
  edit_not_applied: { words: 'notice.cloudClean.code.editNotApplied', sentence: 'notice.cloud.error.editNotApplied' },
  remote_failed: { words: 'notice.cloudClean.code.remoteFailed', sentence: 'notice.cloud.error.remote' },
  weights_unavailable: { words: 'notice.cloudClean.code.weights', sentence: 'notice.cloud.error.weights' },
  remote_cancelled: { words: 'notice.cloudClean.code.remoteCancelled', sentence: 'notice.cloud.error.remoteCancelled' },
  cancelled: { words: 'notice.cloudClean.code.cancelled', sentence: 'notice.cloud.error.cancelled' },
  poll_timeout: { words: 'notice.cloudClean.code.pollTimeout', sentence: 'notice.cloud.error.timeout' },
  // The rows below have no sentence. Each is a stop between two batches of a
  // cloud clean (`cloud_clean.rs#revalidate`, `run_chunk`), before any render
  // of the next batch starts, so no single render can end with one.
  cloud_clean_recipe_changed: { words: 'notice.cloudClean.code.recipeChanged' },
  cloud_clean_gpu_changed: { words: 'notice.cloudClean.code.gpuChanged' },
  cloud_clean_chunk_outside_plan: { words: 'notice.cloudClean.code.chunkRefused' },
  cloud_clean_chunk_mismatch: { words: 'notice.cloudClean.code.chunkRefused' },
  // The deployment's model read found no cleaning model (`gpu::transport_code`).
  capability_unavailable: { words: 'notice.cloudClean.code.weights' },
})

/** A code the table does not have, or no code at all: still a reason. */
export const UNKNOWN_FAILURE = Object.freeze({
  words: 'notice.cloudClean.code.generic',
  sentence: 'notice.cloud.error.generic',
})

/**
 * The code is the part before any `: detail`, so
 * `cloud_clean_chunk_mismatch: plan` reads as the mismatch it is.
 *
 * @param {unknown} code
 * @returns {Readonly<CloudFailure>}
 */
function failureOf(code) {
  const bare = typeof code === 'string' ? code.split(':')[0].trim() : ''
  return Object.hasOwn(CLOUD_FAILURES, bare) ? CLOUD_FAILURES[bare] : UNKNOWN_FAILURE
}

/**
 * @param {unknown} code
 * @returns {string} the i18n key for why a single render did not finish
 */
export function cloudErrorKey(code) {
  return failureOf(code).sentence ?? UNKNOWN_FAILURE.sentence
}

/** Why a region was left out, as its own sentence. */
const SKIPPED = Object.freeze({
  changed: 'notice.cloudClean.regionSkippedChanged',
  gone: 'notice.cloudClean.regionSkippedGone',
  unresolved: 'notice.cloudClean.regionSkippedUnresolved',
})

/**
 * @param {string} key - the notice's key, as the backend sent it
 * @param {Record<string, unknown>} [params]
 * @returns {{key: string, params: Record<string, unknown>}}
 */
export function presentNotice(key, params = {}) {
  switch (key) {
    case 'notice.cloudClean.regionFailed':
    case 'notice.cloudClean.stopped': {
      const { code, ...rest } = params
      return {
        key: key === 'notice.cloudClean.stopped' ? 'notice.cloudClean.stoppedBecause' : 'notice.cloudClean.regionFailedBecause',
        params: { ...rest, codeKey: failureOf(code).words },
      }
    }
    case 'notice.cloudClean.regionSkipped': {
      const { reason, ...rest } = params
      const known = typeof reason === 'string' && Object.hasOwn(SKIPPED, reason)
      return { key: known ? SKIPPED[/** @type {keyof typeof SKIPPED} */ (reason)] : key, params: rest }
    }
    default:
      return { key, params }
  }
}

/** @param {string|undefined} reason @param {boolean|undefined} [canAbandon] */
export function recoveryActions(reason, canAbandon) {
  return { retry: true, abandon: canAbandon ?? (reason !== 'repair_needed' && reason !== 'load_error'), acknowledge: true }
}
