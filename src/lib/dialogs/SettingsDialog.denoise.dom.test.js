/**
 * Settings > Denoise: the target decides which presets are listed, every
 * preset shows its time per page and its credit, Cloud is refused with a
 * reason while no cloud GPU is set up, and Measure runs the local benchmark
 * and keeps its answer.
 *
 * The panel is rendered alone, the way Settings mounts it (with Settings'
 * own catalogue passed in), against a seam stubbed with the emptiest true
 * answers. That the tab exists in Settings is `SettingsDialog.tabs`'s.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import {
  session,
  setCloudAllowed,
  setDenoiseLocalSeconds,
  setDenoisePreset,
  setDenoiseTarget,
} from '../state/session.svelte.js'
import { resetDenoiseState } from '../state/denoise.svelte.js'
import { DENOISE_PRESETS, presetsForPages } from '../model/denoise.js'
import DenoiseSettings from './denoise/DenoiseSettings.svelte'
import PresetList from './denoise/PresetList.svelte'
import { presetTime } from './denoise/time.js'

/** The two local package rows, as `listModels` lists them. @param {boolean} installed */
function catalogue(installed) {
  return {
    models: [
      { id: 'pageDenoiseModel', kindKey: 'models.kind.pageDenoise', fileName: 'a.onnx', bytes: 46_210_304, installed, requiredBy: ['pageDenoise'] },
      { id: 'pageDenoiseSeams', kindKey: 'models.kind.pageDenoiseSeams', fileName: 'b.onnx', bytes: 12_288, installed, requiredBy: ['pageDenoise'] },
    ],
  }
}

/** @type {Record<string, ReturnType<typeof vi.fn>>} */
let seam

beforeEach(() => {
  resetDenoiseState()
  setCloudAllowed(false)
  setDenoiseTarget('local')
  setDenoisePreset('')
  setDenoiseLocalSeconds(null)
  seam = {
    writeSettings: vi.fn(async () => ({})),
    denoisePresets: vi.fn(async () => []),
    cloudDenoisePresets: vi.fn(async () => DENOISE_PRESETS.map((preset) => preset.id)),
    benchmarkDenoiseLocal: vi.fn(async () => ({ secondsPerPage: 38.4 })),
    listModels: vi.fn(async () => catalogue(true)),
    downloadModel: vi.fn(async () => {}),
    cancelDownload: vi.fn(async () => {}),
    subscribe: vi.fn(() => () => {}),
  }
  setBackend(/** @type {any} */ (seam))
})

afterEach(cleanup)

/** @param {HTMLElement} container */
const presetIds = (container) =>
  [...container.querySelectorAll('[data-preset]')].map((row) => row.getAttribute('data-preset'))

/** The cloud presets listed for a page type, in table order. @param {'bw'|'color'} pages */
const cloudIds = (pages) => presetsForPages(DENOISE_PRESETS, pages).map((preset) => preset.id)

/** @param {HTMLElement} container @param {string} id */
const presetRow = (container, id) => /** @type {HTMLElement} */ (container.querySelector(`[data-preset="${id}"]`))

describe('Settings > Denoise', () => {
  it('lists only the local preset on this computer, and says it has not been measured', () => {
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })

    expect(presetIds(container)).toEqual(['waifu2x-scan-4x-n2'])
    const row = presetRow(container, 'waifu2x-scan-4x-n2')
    expect(row.textContent).toContain(t('denoise.time.notMeasured'))
    expect(row.textContent).toContain(t('denoise.credit', { name: 'waifu2x (nunif)', author: 'nagadomi', license: 'MIT' }))

    const cloud = getByRole('radio', { name: t('denoise.target.cloud') })
    expect(cloud.hasAttribute('disabled') || cloud.getAttribute('aria-disabled') === 'true').toBe(true)
    expect(container.textContent).toContain(t('denoise.target.cloudUnavailable'))
  })

  it('lists the black and white presets on the cloud GPU with their measured cloud time, and saves the target', async () => {
    setCloudAllowed(true)
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })

    await fireEvent.click(getByRole('radio', { name: t('denoise.target.cloud') }))
    await waitFor(() => expect(presetIds(container)).toEqual(cloudIds('bw')))
    expect(session.denoiseTarget).toBe('cloud')
    expect(seam.writeSettings).toHaveBeenLastCalledWith(expect.objectContaining({ denoiseTarget: 'cloud', denoisePreset: 'mangajanai-2x' }))

    const waifu = presetRow(container, 'waifu2x-scan-4x-n2')
    expect(waifu.textContent).toContain(t('denoise.time.cloud', { duration: t('denoise.duration.seconds', { value: '9.8' }) }))
    expect(waifu.textContent).toContain(t('denoise.time.cloudWhere', { gpu: 'L4' }))

    await fireEvent.click(presetRow(container, 'realcugan-3x-denoise3'))
    await waitFor(() => expect(session.denoisePreset).toBe('realcugan-3x-denoise3'))
  })

  it('shows every cloud preset its measured time', async () => {
    setCloudAllowed(true)
    setDenoiseTarget('cloud')
    const { container } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })

    await waitFor(() => expect(presetIds(container)).toEqual(cloudIds('bw')))
    for (const id of cloudIds('bw')) {
      expect(presetRow(container, id).querySelector('[data-time]')?.getAttribute('data-time'), id).toBe('known')
    }
  })

  it('says a cloud preset with no measured time has not been measured', () => {
    // The table has a time for every preset; one added before its round has none.
    const untimed = { ...DENOISE_PRESETS[0], id: 'untimed', cloudSecondsPerPage: null, cloudSecondsPerMegapixel: null }
    const { container } = render(PresetList, { props: {
      presets: [DENOISE_PRESETS[0], untimed], value: DENOISE_PRESETS[0].id, onchange: vi.fn(), label: 'presets',
      timeOf: (/** @type {any} */ preset) => presetTime(preset, 'cloud'),
    } })

    const row = presetRow(container, 'untimed')
    expect(row.querySelector('[data-time]')?.getAttribute('data-time')).toBe('unknown')
    expect(row.querySelector('[data-time]')?.textContent?.trim()).toBe(t('denoise.time.notMeasured'))
    expect(presetRow(container, DENOISE_PRESETS[0].id).querySelector('[data-time]')?.getAttribute('data-time')).toBe('known')
  })

  it('filters the list by what the pages are, starting on the chosen preset\'s own type', async () => {
    setCloudAllowed(true)
    setDenoiseTarget('cloud')
    setDenoisePreset('waifu2x-art-2x-n3')
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })
    const pages = (/** @type {string} */ key) => getByRole('radio', { name: t(key) })

    // A colour preset is chosen: the list opens on Colour, with it selected.
    await waitFor(() => expect(presetIds(container)).toEqual(cloudIds('color')))
    expect(pages('denoise.pages.color').getAttribute('aria-checked')).toBe('true')
    expect(presetRow(container, 'waifu2x-art-2x-n3').getAttribute('aria-checked')).toBe('true')
    expect(presetIds(container)).not.toContain('mangajanai-2x')

    // Black and white hides it: the first preset still listed is saved in its place.
    await fireEvent.click(pages('denoise.pages.bw'))
    await waitFor(() => expect(presetIds(container)).toEqual(cloudIds('bw')))
    expect(session.denoisePreset).toBe('mangajanai-2x')
    expect(seam.writeSettings).toHaveBeenLastCalledWith(expect.objectContaining({ denoisePreset: 'mangajanai-2x' }))
    expect(presetIds(container)).not.toContain('waifu2x-art-2x-n3')

    // A preset made for both stays chosen when the type is switched.
    await fireEvent.click(presetRow(container, 'realcugan-2x-conservative'))
    await waitFor(() => expect(session.denoisePreset).toBe('realcugan-2x-conservative'))
    const writes = seam.writeSettings.mock.calls.length
    await fireEvent.click(pages('denoise.pages.color'))
    await waitFor(() => expect(presetIds(container)).toEqual(cloudIds('color')))
    expect(session.denoisePreset).toBe('realcugan-2x-conservative')
    expect(seam.writeSettings.mock.calls.length).toBe(writes)
  })

  it('moves and selects with the arrows over the listed presets only, wrapping at the ends', async () => {
    setCloudAllowed(true)
    setDenoiseTarget('cloud')
    setDenoisePreset('waifu2x-art-2x-n3')
    const { container } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })
    const listed = cloudIds('color')
    await waitFor(() => expect(presetIds(container)).toEqual(listed))
    expect(listed.at(-1)).toBe('waifu2x-art-2x-n3')
    const stops = () => [...container.querySelectorAll('[data-preset][tabindex="0"]')].map((row) => row.getAttribute('data-preset'))
    expect(stops()).toEqual(['waifu2x-art-2x-n3'])

    // Down from the last listed row wraps to the first listed one, not to a hidden preset.
    await fireEvent.keyDown(presetRow(container, 'waifu2x-art-2x-n3'), { key: 'ArrowDown' })
    await waitFor(() => expect(session.denoisePreset).toBe(listed[0]))
    expect(document.activeElement).toBe(presetRow(container, listed[0]))
    expect(stops()).toEqual([listed[0]])
    // The first listed preset is made for both page types: the list stays on Colour.
    expect(presetIds(container)).toEqual(listed)

    await fireEvent.keyDown(presetRow(container, listed[0]), { key: 'ArrowUp' })
    await waitFor(() => expect(session.denoisePreset).toBe('waifu2x-art-2x-n3'))
    await fireEvent.keyDown(presetRow(container, 'waifu2x-art-2x-n3'), { key: 'Home' })
    await waitFor(() => expect(session.denoisePreset).toBe(listed[0]))
    await fireEvent.keyDown(presetRow(container, listed[0]), { key: 'End' })
    await waitFor(() => expect(session.denoisePreset).toBe('waifu2x-art-2x-n3'))
    expect(document.activeElement).toBe(presetRow(container, 'waifu2x-art-2x-n3'))
  })

  it('refuses Cloud, and says why, when the cloud GPU was set up without Page denoise', async () => {
    setCloudAllowed(true)
    seam.cloudDenoisePresets.mockRejectedValue(new Error('capability_unavailable: gateway denoise is not configured'))
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn(), active: true } })

    await waitFor(() => expect(container.textContent).toContain(t('denoise.target.cloudNotSetUp')))
    const cloud = getByRole('radio', { name: t('denoise.target.cloud') })
    expect(cloud.hasAttribute('disabled') || cloud.getAttribute('aria-disabled') === 'true').toBe(true)
  })

  it('lists only the presets the cloud GPU says it can run', async () => {
    setCloudAllowed(true)
    setDenoiseTarget('cloud')
    seam.cloudDenoisePresets.mockResolvedValue(['waifu2x-scan-4x-n2', 'realcugan-2x-conservative'])
    const { container } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn(), active: true } })

    await waitFor(() => expect(presetIds(container)).toEqual(['waifu2x-scan-4x-n2', 'realcugan-2x-conservative']))
  })

  it('asks the cloud GPU again each time the tab opens', async () => {
    setCloudAllowed(true)
    const view = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn(), active: false } })
    await Promise.resolve()
    expect(seam.cloudDenoisePresets).not.toHaveBeenCalled()
    await view.rerender({ view: catalogue(true), refresh: vi.fn(), active: true })
    await waitFor(() => expect(seam.cloudDenoisePresets).toHaveBeenCalledTimes(1))
    await view.rerender({ view: catalogue(true), refresh: vi.fn(), active: false })
    await view.rerender({ view: catalogue(true), refresh: vi.fn(), active: true })
    await waitFor(() => expect(seam.cloudDenoisePresets).toHaveBeenCalledTimes(2))
  })

  it('lists no presets when denoise is off', async () => {
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })
    await fireEvent.click(getByRole('radio', { name: t('denoise.target.off') }))
    await waitFor(() => expect(presetIds(container)).toEqual([]))
    expect(container.textContent).toContain(t('settings.denoise.offNote'))
  })

  it('measures this computer, shows the answer and keeps it', async () => {
    /** @type {(value: unknown) => void} */
    let answer = () => {}
    seam.benchmarkDenoiseLocal.mockImplementation(() => new Promise((resolve) => { answer = resolve }))
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(true), refresh: vi.fn() } })

    const measure = getByRole('button', { name: t('denoise.time.measure') })
    await fireEvent.click(measure)
    await waitFor(() => expect(container.textContent).toContain(t('denoise.time.measuring')))
    expect(seam.benchmarkDenoiseLocal).toHaveBeenCalledWith({ presetId: 'waifu2x-scan-4x-n2' })

    answer({ secondsPerPage: 38.4 })
    const measured = t('denoise.time.local', { duration: t('denoise.duration.seconds', { value: '38.4' }) })
    await waitFor(() => expect(presetRow(container, 'waifu2x-scan-4x-n2').textContent).toContain(measured))
    expect(session.denoiseLocalSecondsPerPage).toBe(38.4)
    expect(seam.writeSettings).toHaveBeenLastCalledWith(expect.objectContaining({ denoiseLocalSecondsPerPage: 38.4 }))
    getByRole('button', { name: t('denoise.time.remeasure') })
  })

  it('will not measure before the local model is here, and offers its download', async () => {
    const { container, getByRole } = render(DenoiseSettings, { props: { view: catalogue(false), refresh: vi.fn() } })
    expect(getByRole('button', { name: t('denoise.time.measure') }).hasAttribute('disabled')).toBe(true)
    expect(container.textContent).toContain(t('denoise.time.needsModel'))
    expect(container.querySelectorAll('[data-denoise-model] button').length).toBeGreaterThan(0)
  })
})
