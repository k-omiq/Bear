/**
 * The cloud clean run's own notices, made readable.
 *
 * The native batch (`src-tauri/src/inference/cloud_clean.rs`) reports a region
 * it could not clean, a region it left out and a batch it stopped, each with a
 * machine code or reason. A code is a word to the reader only where the
 * catalogue has one for it; otherwise the sentence is said without it rather
 * than with `gateway_protocol` in the middle. So each notice has two forms,
 * with and without the cause, and this picks one.
 *
 * Every other notice passes through unchanged.
 */

/** Codes the batch can report, as the words a sentence says them in. */
const CODE_WORDS = Object.freeze({
  cloud_disabled: 'notice.cloudClean.code.cloudDisabled',
  gateway_unauthorized: 'notice.cloudClean.code.gatewayUnauthorized',
  credential_missing: 'notice.cloudClean.code.credentialMissing',
  cloud_run_profile_changed: 'notice.cloudClean.code.profileChanged',
  gateway_unreachable: 'notice.cloudClean.code.gatewayUnreachable',
  gateway_error: 'notice.cloudClean.code.gatewayError',
  gateway_protocol: 'notice.cloudClean.code.gatewayProtocol',
  consent_invalid: 'notice.cloudClean.code.consentInvalid',
  remote_failed: 'notice.cloudClean.code.remoteFailed',
  // A stop between batches (`cloud_clean.rs#revalidate`, `run_chunk`).
  cloud_clean_recipe_changed: 'notice.cloudClean.code.recipeChanged',
  cloud_clean_gpu_changed: 'notice.cloudClean.code.gpuChanged',
  cloud_clean_chunk_outside_plan: 'notice.cloudClean.code.chunkRefused',
  cloud_clean_chunk_mismatch: 'notice.cloudClean.code.chunkRefused',
  poll_timeout: 'notice.cloudClean.code.pollTimeout',
  submission_unknown: 'notice.cloudClean.code.submissionUnknown',
  recovery_required: 'cloud.recovery.unresolved',
})

/** Why a region was left out, as its own sentence. */
const SKIPPED = Object.freeze({
  changed: 'notice.cloudClean.regionSkippedChanged',
  gone: 'notice.cloudClean.regionSkippedGone',
  unresolved: 'notice.cloudClean.regionSkippedUnresolved',
})

/** @param {unknown} code */
function codeKey(code) {
  const bare = typeof code === 'string' ? code.split(':')[0].trim() : ''
  return Object.hasOwn(CODE_WORDS, bare) ? CODE_WORDS[/** @type {keyof typeof CODE_WORDS} */ (bare)] : null
}

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
      const words = codeKey(code)
      if (!words) return { key, params: rest }
      return {
        key: key === 'notice.cloudClean.stopped' ? 'notice.cloudClean.stoppedBecause' : 'notice.cloudClean.regionFailedBecause',
        params: { ...rest, codeKey: words },
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
