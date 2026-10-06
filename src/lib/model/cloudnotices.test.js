import { describe, expect, it } from 'vitest'
import { t } from '../i18n/index.js'
import { presentNotice, recoveryActions } from './cloudnotices.js'

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
      'cloud_clean_chunk_mismatch: plan']) {
      expect(presentNotice('notice.cloudClean.stopped', { page: 1, code }).key).toBe('notice.cloudClean.stoppedBecause')
    }
  })

  it('leaves an unknown code out of the sentence rather than printing it', () => {
    const text = said('notice.cloudClean.regionFailed', { regionId: 'r', page: 4, code: 'inference_oom' })
    expect(text).toBe(t('notice.cloudClean.regionFailed', { page: 4 }))
    expect(text).not.toContain('inference_oom')
    expect(said('notice.cloudClean.stopped', { page: 1 })).toBe(t('notice.cloudClean.stopped', { page: 1 }))
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
      .toContain(t('cloud.recovery.unresolved'))
  })

  it('passes every other notice through', () => {
    expect(presentNotice('notice.run.detected', { pages: 1, regions: 2 })).toEqual({ key: 'notice.run.detected', params: { pages: 1, regions: 2 } })
  })
})

it('offers recovery and abandonment for uncertainty but only acknowledgement for repair', () => {
  expect(recoveryActions('ambiguous')).toEqual({ retry: true, abandon: true, acknowledge: true })
  expect(recoveryActions('failed').abandon).toBe(true)
  expect(recoveryActions('repair_needed').abandon).toBe(false)
  expect(recoveryActions('repair_needed', true).abandon).toBe(true)
  expect(t('cloud.recovery.duplicateRisk')).toContain('duplicate')
})
