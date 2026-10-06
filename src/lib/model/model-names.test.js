import { describe, expect, it } from 'vitest'
import { t } from '../i18n/index.js'
import {
  DETECTOR_MODEL_NAMES,
  analysisCapabilityName,
  currentCloudModelId,
  detectorModelName,
  engineModelLabel,
  knownModelName,
} from './model-names.js'

const FOUR = 'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic'
const NINE = 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32'

describe('engine model names', () => {
  it('uses exact pinned cloud IDs and marks remote execution', () => {
    expect(knownModelName(FOUR)).toBe('FLUX.2 Klein 4B')
    expect(engineModelLabel('cloud', NINE, 'Cloud')).toBe('☁ FLUX.2 Klein 9B · Cloud')
    expect(engineModelLabel('cloud', 'Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32', 'Cloud'))
      .toBe('☁ Qwen-Image-Edit-2511 · Cloud')
    expect(engineModelLabel('cloud', 'unrecognized', 'Cloud')).toBe('☁ Cloud')
    expect(engineModelLabel('flux', 'flux2-klein-4b', 'FLUX')).toBe('FLUX.2 Klein 4B · Local')
  })

  it('does not use metadata from a different endpoint', () => {
    const state = {
      readiness: { target: { type: 'modal', profile_id: 'new' }, profile: null },
      model: { id: FOUR, provider: 'modal', profileId: 'old' },
    }
    expect(currentCloudModelId(state)).toBeNull()
    state.model.profileId = 'new'
    expect(currentCloudModelId(state)).toBe(FOUR)
    state.readiness.profile = { modelId: NINE }
    expect(currentCloudModelId(state)).toBe(NINE)
  })
})

describe('detection model names', () => {
  it('names the four choices by their persisted ids, with no dash before a profile', () => {
    expect(DETECTOR_MODEL_NAMES).toEqual({
      ctd: 'Comic Text Detector (CTD)',
      rtFull: 'Ogkalu comic text & bubble detector (Full)',
      rtSmall: 'Ogkalu comic text & bubble detector (Small)',
      samTs: 'SAM-TS-L lettering mask',
    })
    expect(detectorModelName('rtSmall')).toBe('Ogkalu comic text & bubble detector (Small)')
    expect(detectorModelName('coo')).toBeNull()
  })

  it('names a cloud analysis capability as the model it runs, never by a helper’s own label', () => {
    expect(analysisCapabilityName('text_regions_rt@1')).toBe(DETECTOR_MODEL_NAMES.rtFull)
    expect(analysisCapabilityName('text_mask_sam_ts@1')).toBe(DETECTOR_MODEL_NAMES.samTs)
    expect(analysisCapabilityName('text_regions_ctd@1')).toBeNull()
    expect(knownModelName('ogkalu/comic-text-and-bubble-detector')).toBe(DETECTOR_MODEL_NAMES.rtFull)
    expect(knownModelName('sam-ts-l')).toBe(DETECTOR_MODEL_NAMES.samTs)
  })

  it('keeps the cloud review and model review copy on the same product names', () => {
    const { rtFull, samTs, ctd } = DETECTOR_MODEL_NAMES
    expect(t('cloud.analysis.capability.rt')).toBe(`☁ ${rtFull}`)
    expect(t('cloud.analysis.capability.sam')).toBe(`☁ ${samTs}`)
    expect(t('cloud.analysis.capability.both')).toBe(`☁ ${samTs} with ${rtFull}`)
    expect(t('workflow.model.sam')).toBe(samTs)
    expect(t('workflow.detail.rtIdentity')).toContain(rtFull)
    expect(t('workflow.ready.ctd')).toContain(ctd)
    // The review's family row covers both profiles, so it names the model without one.
    expect(`${t('workflow.model.rt')} (Full)`).toBe(rtFull)
  })
})
