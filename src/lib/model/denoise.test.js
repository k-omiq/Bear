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
  outputScalesFor,
  pageKindFor,
  pageMegapixels,
  presetById,
  presetsFor,
  presetsForPages,
  recipeAt,
  runScale,
  validPreset,
} from './denoise.js'
import { t } from '../i18n/index.js'

describe('the denoise presets', () => {
  it('are the shared table, all fourteen, each named, credited, and given a page type and a largest size', () => {
    expect(DENOISE_PRESETS.map((preset) => preset.id)).toEqual(TABLE.presets.map((preset) => preset.id))
    expect(DENOISE_PRESETS).toHaveLength(14)
    expect(Object.keys(PRESET_TEXT).sort()).toEqual(DENOISE_PRESETS.map((preset) => preset.id).sort())
    const names = new Set()
    for (const preset of DENOISE_PRESETS) {
      const text = PRESET_TEXT[preset.id]
      expect(t(text.nameKey), preset.id).not.toMatch(/^\[/)
      expect(t(text.noteKey), preset.id).not.toMatch(/^\[/)
      names.add(t(text.nameKey))
      expect(preset.credit.name && preset.credit.author && preset.credit.license).toBeTruthy()
      expect(['bw', 'color', 'any']).toContain(preset.kind)
      expect([1, 2, 3, 4]).toContain(preset.maxScale)
    }
    expect(names.size, 'no two presets share a name').toBe(14)
  })

  it('carry a cloud time that is measured or null, never a guess', () => {
    for (const preset of DENOISE_PRESETS) {
      const { cloudSecondsPerPage: perPage, cloudSecondsPerMegapixel: perMegapixel } = preset
      expect(perPage === null, preset.id).toBe(perMegapixel === null)
      if (perPage !== null) {
        expect(perPage).toBeGreaterThan(0)
        expect(perMegapixel).toBeGreaterThan(0)
      }
    }
  })

  it('are filtered by target: all fourteen on the cloud, one on this computer, none when off', () => {
    expect(presetsFor('cloud')).toHaveLength(14)
    expect(presetsFor('local').map((preset) => preset.id)).toEqual(['waifu2x-scan-4x-n2'])
    expect(presetsFor('off')).toEqual([])
  })

  it('hide a preset the backend does not know, and keep the table when it says nothing', () => {
    expect(presetsFor('cloud', [{ id: 'realcugan-2x-conservative' }]).map((preset) => preset.id))
      .toEqual(['realcugan-2x-conservative'])
    expect(presetsFor('cloud', [])).toHaveLength(14)
    expect(presetsFor('cloud', null)).toHaveLength(14)
  })

  it('resolve a stored choice to one the target offers, else the first', () => {
    expect(validPreset('cloud', 'mangajanai-4x')).toBe('mangajanai-4x')
    expect(validPreset('local', 'mangajanai-4x')).toBe('waifu2x-scan-4x-n2')
    expect(validPreset('cloud', 'nonsense')).toBe('mangajanai-2x')
    expect(validPreset('off', 'mangajanai-2x')).toBeNull()
  })
})

describe('the presets a page type shows', () => {
  const ids = (/** @type {Array<{id: string}>} */ list) => list.map((preset) => preset.id)

  it('are those made for it and those made for both, in table order', () => {
    const bw = presetsForPages(DENOISE_PRESETS, 'bw')
    const color = presetsForPages(DENOISE_PRESETS, 'color')
    expect(bw.every((preset) => preset.kind === 'bw' || preset.kind === 'any')).toBe(true)
    expect(color.every((preset) => preset.kind === 'color' || preset.kind === 'any')).toBe(true)
    expect(ids(bw)).toEqual(ids(DENOISE_PRESETS.filter((preset) => preset.kind !== 'color')))
    expect(ids(bw)).toContain('mangajanai-2x')
    expect(ids(bw)).not.toContain('waifu2x-art-2x-n1')
    expect(ids(color)).toEqual(expect.arrayContaining(['waifu2x-art-2x-n1', 'waifu2x-art-2x-n3', 'realcugan-2x-conservative']))
    expect(ids(color)).not.toContain('mangajanai-2x')
    expect(ids(color)).not.toContain('mangajpeg-hq')
  })

  it('keep a preset that names no type on both lists', () => {
    const list = [{ id: 'old' }, { id: 'ink', kind: 'bw' }]
    expect(ids(presetsForPages(list, 'color'))).toEqual(['old'])
    expect(ids(presetsForPages(list, 'bw'))).toEqual(['old', 'ink'])
  })

  it('start on the chosen preset\'s own type: colour for a colour preset, black and white for the rest', () => {
    expect(pageKindFor(DENOISE_PRESETS, 'waifu2x-art-2x-n3')).toBe('color')
    expect(pageKindFor(DENOISE_PRESETS, 'mangajanai-4x')).toBe('bw')
    expect(pageKindFor(DENOISE_PRESETS, 'realcugan-2x-conservative')).toBe('bw')
    expect(pageKindFor(DENOISE_PRESETS, null)).toBe('bw')
    expect(pageKindFor([], 'mangajanai-4x')).toBe('bw')
  })

  it('keep the picked type while it shows the chosen preset, and follow the preset when it does not', () => {
    expect(pageKindFor(DENOISE_PRESETS, 'realcugan-2x-conservative', 'color')).toBe('color')
    expect(pageKindFor(DENOISE_PRESETS, 'waifu2x-art-2x-n1', 'color')).toBe('color')
    expect(pageKindFor(DENOISE_PRESETS, 'mangajanai-2x', 'color')).toBe('bw')
    expect(pageKindFor(DENOISE_PRESETS, 'waifu2x-art-2x-n1', 'bw')).toBe('color')
    expect(pageKindFor(DENOISE_PRESETS, null, 'color')).toBe('color')
  })
})

describe('the output size of a run', () => {
  it('offers same size up to the preset\'s largest, and only same size when it says nothing usable', () => {
    expect(outputScalesFor(presetById('mangajanai-4x'))).toEqual([1, 2, 3, 4])
    expect(outputScalesFor(presetById('realcugan-3x-conservative'))).toEqual([1, 2, 3])
    expect(outputScalesFor(presetById('waifu2x-art-2x-n1'))).toEqual([1, 2])
    expect(outputScalesFor(presetById('mangajpeg-hq'))).toEqual([1])
    for (const maxScale of [undefined, null, 0, -2, 2.5, 'four', NaN]) expect(outputScalesFor({ maxScale })).toEqual([1])
    expect(outputScalesFor({ maxScale: 9 })).toEqual([1, 2, 3, 4])
    expect(outputScalesFor(null)).toEqual([1])
  })

  it('sends the preset\'s own recipe at same size, with no output_scale key at all', () => {
    const preset = presetById('mangajanai-4x')
    expect(recipeAt(preset, 1)).toBe(preset.recipe)
    expect('output_scale' in recipeAt(preset, 1)).toBe(false)
    expect(JSON.stringify(recipeAt(preset, 1))).toBe(JSON.stringify(preset.recipe))
  })

  it('adds a top-level output_scale above same size, and leaves the preset\'s recipe as it was', () => {
    const preset = presetById('mangajanai-4x')
    const before = JSON.stringify(preset.recipe)
    expect(recipeAt(preset, 2)).toEqual({ ...preset.recipe, output_scale: 2 })
    expect(recipeAt(preset, 4).output_scale).toBe(4)
    expect(recipeAt(preset, 4).steps).toEqual(preset.recipe.steps)
    expect(JSON.stringify(preset.recipe)).toBe(before)
  })

  it('never asks for a size the preset cannot give', () => {
    expect(recipeAt(presetById('mangajanai-2x'), 3)).toBe(presetById('mangajanai-2x').recipe)
    expect(recipeAt(presetById('mangajpeg-lq'), 2)).toBe(presetById('mangajpeg-lq').recipe)
    for (const scale of [0, 1.5, '2x', null, undefined]) {
      expect('output_scale' in recipeAt(presetById('mangajanai-4x'), scale)).toBe(false)
    }
  })

  it('reads a run\'s recorded scale, and same size from an older backend that records none', () => {
    expect(runScale(2)).toBe(2)
    expect(runScale(4)).toBe(4)
    for (const value of [undefined, null, 1, 0, 5, 2.5, '3']) expect(runScale(value)).toBe(1)
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

  it('says a cloud preset has not been measured rather than answering zero or NaN', () => {
    // Every listed preset has been measured; one added before its round has not.
    const untimed = { ...DENOISE_PRESETS[0], cloudSecondsPerPage: null, cloudSecondsPerMegapixel: null }
    for (const pages of [[page, page], [page, {}]]) {
      expect(chapterEstimate({ target: 'cloud', preset: untimed, pages }))
        .toEqual({ seconds: null, perPage: null, basis: 'unmeasured' })
    }
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
