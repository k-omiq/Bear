import { describe, expect, it } from 'vitest'

import { t } from '../i18n/index.js'
import { imageType, presetName, runFacts } from './denoisehistory.js'

/** @param {Partial<import('../api/backend.js').DenoiseRun>} [over] */
const aRun = (over = {}) => ({
  created: 1790000000,
  preset: 'waifu2x-scan-4x-n2',
  target: 'local',
  fromCleaned: false,
  folder: '/scans/denoised',
  pages: [
    { pageIndex: 0, exists: true, taken: false, current: true, fromCleaned: false },
    { pageIndex: 1, exists: true, taken: false, current: true, fromCleaned: false },
  ],
  ...over,
})

describe('the words for a denoise run', () => {
  it('names a known preset, keeps an unknown id, and says when none was recorded', () => {
    expect(presetName('waifu2x-scan-4x-n2')).toBe(t('denoise.preset.waifu2xScan.name'))
    expect(presetName('a-later-preset')).toBe('a-later-preset')
    expect(presetName(null)).toBe(t('home.denoised.presetUnknown'))
  })

  it('states where it ran, what from, how many pages, and what is taken or gone', () => {
    expect(runFacts(aRun())).toEqual([t('denoise.target.local'), t('home.denoised.fromRaw'), t('home.denoised.pages', { count: 2 })])
    const used = aRun({
      target: 'cloud',
      fromCleaned: true,
      pages: [
        { pageIndex: 0, exists: true, taken: true, current: false, fromCleaned: true },
        { pageIndex: 1, exists: false, taken: false, current: true, fromCleaned: true },
      ],
    })
    expect(runFacts(used)).toEqual([
      t('denoise.target.cloud'), t('home.denoised.fromCleaned'), t('home.denoised.pages', { count: 2 }),
      t('home.denoised.taken', { count: 1 }), t('home.denoised.missing', { count: 1 }),
    ])
  })

  it('leaves out what an old run did not record', () => {
    expect(runFacts(aRun({ target: null, fromCleaned: null, preset: null }))).toEqual([t('home.denoised.pages', { count: 2 })])
  })
})

describe('an image type from its bytes', () => {
  it('knows a PNG, a JPEG and an SVG, and names nothing else', () => {
    expect(imageType(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d]))).toBe('image/png')
    expect(imageType(new Uint8Array([0xff, 0xd8, 0xff, 0xe0]).buffer)).toBe('image/jpeg')
    expect(imageType(new TextEncoder().encode('  <svg xmlns="http://www.w3.org/2000/svg"/>'))).toBe('image/svg+xml')
    expect(imageType(new Uint8Array([1, 2, 3]))).toBe('')
  })
})
