import { describe, expect, it } from 'vitest'

import TABLE from './denoise-presets.json'
import {
  DENOISE_PRESETS,
  PRESET_TEXT,
  benchmarkSeconds,
  chapterEstimate,
  defaultOutDir,
  denoiseRows,
  durationText,
  pageMegapixels,
  presetsFor,
  validPreset,
} from './denoise.js'

describe('the denoise presets', () => {
  it('are the shared table, all six, each named and credited', () => {
    expect(DENOISE_PRESETS.map((preset) => preset.id)).toEqual(TABLE.presets.map((preset) => preset.id))
    expect(DENOISE_PRESETS).toHaveLength(6)
    for (const preset of DENOISE_PRESETS) {
      expect(PRESET_TEXT[preset.id]).toBeTruthy()
      expect(preset.credit.name && preset.credit.author && preset.credit.license).toBeTruthy()
      expect(preset.cloudSecondsPerPage).toBeGreaterThan(0)
    }
  })

  it('are filtered by target: all six on the cloud, one on this computer, none when off', () => {
    expect(presetsFor('cloud')).toHaveLength(6)
    expect(presetsFor('local').map((preset) => preset.id)).toEqual(['waifu2x-scan-4x-n2'])
    expect(presetsFor('off')).toEqual([])
  })

  it('hide a preset the backend does not know, and keep the table when it says nothing', () => {
    expect(presetsFor('cloud', [{ id: 'realcugan-2x-conservative' }]).map((preset) => preset.id))
      .toEqual(['realcugan-2x-conservative'])
    expect(presetsFor('cloud', [])).toHaveLength(6)
    expect(presetsFor('cloud', null)).toHaveLength(6)
  })

  it('resolve a stored choice to one the target offers, else the first', () => {
    expect(validPreset('cloud', 'mangajanai-4x')).toBe('mangajanai-4x')
    expect(validPreset('local', 'mangajanai-4x')).toBe('waifu2x-scan-4x-n2')
    expect(validPreset('cloud', 'nonsense')).toBe('mangajanai-2x')
    expect(validPreset('off', 'mangajanai-2x')).toBeNull()
  })
})

describe('time per page and per chapter', () => {
  const preset = DENOISE_PRESETS.find((entry) => entry.id === 'mangajanai-2x')
  const page = { width: 1284, height: 1809 }

  it('scales cloud time by the chapter pages\' area when every page has a size', () => {
    const estimate = chapterEstimate({ target: 'cloud', preset, pages: [page, page] })
    expect(estimate.basis).toBe('pages')
    expect(estimate.seconds).toBeCloseTo(2 * 1.284 * 1.809 * 1.73, 5)
  })

  it('falls back to the reference page when a page has no size', () => {
    const estimate = chapterEstimate({ target: 'cloud', preset, pages: [page, {}] })
    expect(estimate).toEqual({ seconds: 8, perPage: 4, basis: 'reference' })
    expect(pageMegapixels([page, {}])).toBeNull()
  })

  it('uses only what was measured on this computer, and says when nothing was', () => {
    const local = DENOISE_PRESETS.find((entry) => entry.id === 'waifu2x-scan-4x-n2')
    expect(chapterEstimate({ target: 'local', preset: local, pages: [page, page, page], localPerPage: 12.5 }))
      .toEqual({ seconds: 37.5, perPage: 12.5, basis: 'measured' })
    expect(chapterEstimate({ target: 'local', preset: local, pages: [page], localPerPage: null }).basis).toBe('unmeasured')
  })

  it('reads the benchmark bare or wrapped, and refuses anything else', () => {
    expect(benchmarkSeconds(9.5)).toBe(9.5)
    expect(benchmarkSeconds({ secondsPerPage: 38.4 })).toBe(38.4)
    expect(benchmarkSeconds({ secondsPerPage: -1 })).toBeNull()
    expect(benchmarkSeconds('fast')).toBeNull()
  })

  it('says a duration in seconds, minutes, then hours', () => {
    expect(durationText(9.8)).toEqual({ key: 'denoise.duration.seconds', params: { value: '9.8' } })
    expect(durationText(15.8).params.value).toBe('15.8')
    expect(durationText(150)).toEqual({ key: 'denoise.duration.minutes', params: { value: 3 } })
    expect(durationText(3 * 3600 + 20 * 60)).toEqual({ key: 'denoise.duration.hours', params: { hours: 3, minutes: 20 } })
  })
})

describe('the local package and the output folder', () => {
  it('are the catalogue rows required by pageDenoise', () => {
    const rows = [
      { id: 'inpainter', requiredBy: ['lama'] },
      { id: 'pageDenoiseModel', requiredBy: ['pageDenoise'] },
      { id: 'pageDenoiseSeams', requiredBy: ['pageDenoise'] },
    ]
    expect(denoiseRows(rows).map((row) => row.id)).toEqual(['pageDenoiseModel', 'pageDenoiseSeams'])
    expect(denoiseRows(null)).toEqual([])
  })

  it('default to a denoised folder inside the chapter, with its own separator', () => {
    expect(defaultOutDir('/scans/ch1/')).toBe('/scans/ch1/denoised')
    expect(defaultOutDir('C:\\scans\\ch1')).toBe('C:\\scans\\ch1\\denoised')
    expect(defaultOutDir('')).toBe('')
    expect(defaultOutDir('/scans/ch1/', 'denoised-cleaned')).toBe('/scans/ch1/denoised-cleaned')
  })
})
