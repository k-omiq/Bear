import { describe, expect, it } from 'vitest'
import { CATALOGUES, flatten, t } from '../i18n/index.js'
import { CLOUD_FAILURES, UNKNOWN_FAILURE, cloudErrorKey, presentNotice, recoveryActions } from './cloudnotices.js'

/** The sentence a notice renders as. */
const said = (key, params) => {
  const shown = presentNotice(key, params)
  return t(shown.key, shown.params)
}

describe('a cloud clean notice', () => {
  it('says a known code in words', () => {
    expect(presentNotice('notice.cloudClean.regionFailed', { regionId: 'r', page: 3, code: 'gateway_unreachable' })).toEqual({
      key: 'notice.cloudClean.regionFailedBecause',
      params: { regionId: 'r', page: 3, codeKey: 'notice.cloudClean.code.gatewayUnreachable' },
    })
    expect(said('notice.cloudClean.stopped', { page: 2, code: 'gateway_unauthorized' }))
      .toBe(t('notice.cloudClean.stoppedBecause', { page: 2, codeKey: 'notice.cloudClean.code.gatewayUnauthorized' }))
  })

  it('reads every code the batch stops on as words', () => {
    for (const code of ['cloud_disabled', 'gateway_unauthorized', 'credential_missing', 'cloud_run_profile_changed',
      'cloud_clean_recipe_changed', 'cloud_clean_gpu_changed', 'cloud_clean_chunk_outside_plan',
      'cloud_clean_chunk_mismatch: plan', 'capability_unavailable: gateway render is not configured']) {
      const shown = presentNotice('notice.cloudClean.stopped', { page: 1, code })
      expect(shown.key).toBe('notice.cloudClean.stoppedBecause')
      expect(shown.params.codeKey, code).not.toBe(UNKNOWN_FAILURE.words)
    }
  })

  it('gives a reason for a code it does not know, and for none, without printing the code', () => {
    const text = said('notice.cloudClean.regionFailed', { regionId: 'r', page: 4, code: 'inference_oom' })
    expect(text).toBe(t('notice.cloudClean.regionFailedBecause', { page: 4, codeKey: 'notice.cloudClean.code.generic' }))
    expect(text).not.toContain('inference_oom')
    expect(said('notice.cloudClean.stopped', { page: 1 }))
      .toBe(t('notice.cloudClean.stoppedBecause', { page: 1, codeKey: 'notice.cloudClean.code.generic' }))
  })

  it('says why page 26 was not cleaned, for an out of date cloud setup and for an unknown code', () => {
    for (const code of ['edit_not_applied', 'weights_unavailable', 'never_heard_of_it']) {
      const shown = presentNotice('notice.cloudClean.regionFailed', { regionId: 'r', code, page: 26 })
      expect(shown.key, code).toBe('notice.cloudClean.regionFailedBecause')
      expect(shown.params.page).toBe(26)
      const reason = t(/** @type {string} */ (shown.params.codeKey))
      expect(reason, code).not.toBe('')
      expect(t(shown.key, shown.params), code).toContain(reason)
    }
    expect(presentNotice('notice.cloudClean.regionFailed', { code: 'edit_not_applied', page: 26 }).params.codeKey)
      .toBe('notice.cloudClean.code.editNotApplied')
  })

  it('names why a region was left out, and falls back for a reason it does not know', () => {
    expect(presentNotice('notice.cloudClean.regionSkipped', { regionId: 'r', page: 1, reason: 'changed' }).key)
      .toBe('notice.cloudClean.regionSkippedChanged')
    expect(presentNotice('notice.cloudClean.regionSkipped', { regionId: 'r', page: 1, reason: 'gone' }).key)
      .toBe('notice.cloudClean.regionSkippedGone')
    // Left out of its batch: an earlier request for it is unresolved.
    expect(presentNotice('notice.cloudClean.regionSkipped', { regionId: 'r', page: 1, reason: 'unresolved' }).key)
      .toBe('notice.cloudClean.regionSkippedUnresolved')
    expect(presentNotice('notice.cloudClean.regionSkipped', { regionId: 'r', page: 1, reason: 'odd' }))
      .toEqual({ key: 'notice.cloudClean.regionSkipped', params: { regionId: 'r', page: 1 } })
  })

  it('explains why an unresolved attempt prevents a replacement render', () => {
    expect(said('notice.cloudClean.regionFailed', { page: 1, code: 'recovery_required' }))
      .toContain(t('notice.cloudClean.code.recoveryRequired'))
    expect(cloudErrorKey('recovery_required')).toBe('cloud.recovery.unresolved')
  })

  it('passes every other notice through', () => {
    expect(presentNotice('notice.run.detected', { pages: 1, regions: 2 })).toEqual({ key: 'notice.run.detected', params: { pages: 1, regions: 2 } })
  })
})

/**
 * Every code the native side can end a render with
 * (`src-tauri/src/inference/service.rs`: `InferenceServiceError::code`,
 * `consent_code`, `journal_code`, `http_code`; `region.rs#render_in_cloud`).
 */
const RENDER_CODES = ['cloud_disabled', 'cloud_run_profile_changed', 'target_invalid', 'target_changed',
  'profile_missing', 'endpoint_invalid', 'credential_missing', 'consent_invalid', 'region_not_found',
  'region_changed', 'region_unsupported', 'attempt_busy', 'attempt_exists', 'page_busy', 'journal_error',
  'journal_unavailable', 'recovery_required', 'provider_paused', 'project_error', 'config_unreadable',
  'invalid_request', 'result_invalid', 'gateway_protocol', 'gateway_unauthorized', 'gateway_error',
  'gateway_unreachable', 'submission_unknown', 'edit_not_applied', 'marker_remained', 'worker_outdated', 'qwen_update_required', 'flux3_update_required', 'remote_failed', 'weights_unavailable',
  'remote_cancelled', 'cancelled', 'poll_timeout']

/** A stop between two batches of a cloud clean: never the end of one render. */
const BATCH_ONLY_CODES = ['cloud_clean_recipe_changed', 'cloud_clean_gpu_changed',
  'cloud_clean_chunk_outside_plan', 'cloud_clean_chunk_mismatch', 'capability_unavailable']

describe('the table of cloud failures', () => {
  it('has both forms for every code a render can end with', () => {
    for (const code of RENDER_CODES) {
      expect(CLOUD_FAILURES[code]?.words, code).toEqual(expect.any(String))
      expect(CLOUD_FAILURES[code]?.sentence, code).toEqual(expect.any(String))
      expect(cloudErrorKey(code), code).toBe(CLOUD_FAILURES[code].sentence)
      expect(cloudErrorKey(code), code).not.toBe(UNKNOWN_FAILURE.sentence)
    }
  })

  it('has words for every code that only stops a batch', () => {
    for (const code of BATCH_ONLY_CODES) expect(CLOUD_FAILURES[code]?.words, code).toEqual(expect.any(String))
    expect(Object.keys(CLOUD_FAILURES).sort()).toEqual([...RENDER_CODES, ...BATCH_ONLY_CODES].sort())
  })

  it('names only strings every catalogue has', () => {
    const rows = [...Object.values(CLOUD_FAILURES), UNKNOWN_FAILURE]
    const keys = new Set(rows.flatMap((row) => [row.words, row.sentence]).filter(Boolean))
    for (const [tag, tree] of Object.entries(CATALOGUES)) {
      const flat = flatten(tree)
      for (const key of keys) expect(flat[key], `${tag}: ${key}`).toEqual(expect.stringMatching(/\S/))
    }
  })

  it('reads a code it does not know, or none, as the generic sentence', () => {
    expect(cloudErrorKey('something_new')).toBe('notice.cloud.error.generic')
    expect(cloudErrorKey(null)).toBe('notice.cloud.error.generic')
    // A code that only stops a batch has no sentence of its own.
    expect(cloudErrorKey('cloud_clean_gpu_changed')).toBe('notice.cloud.error.generic')
  })

  it('tells an out of date cloud setup to update, in both notices', () => {
    expect(cloudErrorKey('edit_not_applied')).toBe('notice.cloud.error.editNotApplied')
    expect(t(cloudErrorKey('edit_not_applied'))).toContain('Settings, Cloud')
    expect(said('notice.cloudClean.regionFailed', { page: 26, code: 'edit_not_applied' })).toContain('Settings, Cloud')
  })
})

it('offers recovery and abandonment for uncertainty but only acknowledgement for repair', () => {
  expect(recoveryActions('ambiguous')).toEqual({ retry: true, abandon: true, acknowledge: true })
  expect(recoveryActions('failed').abandon).toBe(true)
  expect(recoveryActions('repair_needed').abandon).toBe(false)
  expect(recoveryActions('repair_needed', true).abandon).toBe(true)
  expect(t('cloud.recovery.duplicateRisk')).toContain('duplicate')
})
