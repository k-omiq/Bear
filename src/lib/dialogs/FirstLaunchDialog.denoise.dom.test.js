/**
 * Setup's denoise step: the three targets it offers, what it says the cloud
 * step will do for Cloud, that each target lists its own presets with a time per page, and
 * what each choice puts in the Downloads step's queue. This computer queues
 * the local package (the rows required by `pageDenoise`) and the runtime it
 * runs on; the cloud GPU and "Don't use denoise" queue nothing.
 *
 * The step is rendered alone against the browser mock, whose catalogue
 * carries the two local package rows. Walking the whole setup is
 * `FirstLaunchDialog.dom.test.js`'s.
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { session, setCloudAllowed, setDenoisePreset, setDenoiseTarget } from '../state/session.svelte.js'
import { resetDenoiseState } from '../state/denoise.svelte.js'
import { RUNTIME_ID } from './firstlaunch.js'
import { chosenFiles, firstLaunch, offerFirstLaunch, resetFirstLaunch } from './firstlaunch.svelte.js'
import DenoiseStep from './onboarding/DenoiseStep.svelte'

const LOCAL_PACKAGE = ['pageDenoiseModel', 'pageDenoiseSeams']

/** @type {ReturnType<typeof createMockBackend>} */
let mock

beforeEach(async () => {
  resetDenoiseState()
  resetFirstLaunch()
  setCloudAllowed(false)
  setDenoiseTarget('off')
  setDenoisePreset('')
  mock = createMockBackend({ timing: { method: 0, cloud: 0, analysis: 0, region: 0, pageTail: 0 } })
  setBackend(mock)
  offerFirstLaunch(await mock.listModels(), { force: true })
})

afterEach(() => {
  cleanup()
  resetFirstLaunch()
})

/** @param {HTMLElement} container */
const targets = (container) =>
  [...container.querySelectorAll('[data-target]')].map((card) => card.getAttribute('data-target'))

/** @param {HTMLElement} container */
const presetIds = (container) =>
  [...container.querySelectorAll('[data-preset]')].map((row) => row.getAttribute('data-preset'))

/** @param {HTMLElement} container @param {string} target */
const card = (container, target) => /** @type {HTMLElement} */ (container.querySelector(`[data-target="${target}"]`))

describe('the denoise step', () => {
  it('knows the local package from the catalogue', () => {
    expect(firstLaunch.plan?.denoise).toEqual(LOCAL_PACKAGE)
  })

  it('offers the cloud GPU before one is set up, and says the next step sets it up', async () => {
    const { container } = render(DenoiseStep)
    expect(targets(container)).toEqual(['local', 'cloud', 'off'])
    expect(card(container, 'off').getAttribute('aria-checked')).toBe('true')
    expect(presetIds(container)).toEqual([])
    expect(container.querySelector('[data-cloud-next]')).toBeNull()

    await fireEvent.click(card(container, 'cloud'))
    await waitFor(() => expect(presetIds(container)).toHaveLength(6))
    expect(container.querySelector('[data-cloud-next]')?.textContent).toBe(t('onboarding.denoise.cloudNext'))
  })

  it('says the next step adds page denoise to a cloud GPU set up without it', async () => {
    setCloudAllowed(true)
    setDenoiseTarget('cloud')
    mock.cloudDenoisePresets = async () => { throw new Error('capability_unavailable: gateway denoise is not configured') }
    const { container } = render(DenoiseStep)
    await waitFor(() => expect(container.querySelector('[data-cloud-next]')?.textContent).toBe(t('onboarding.denoise.cloudAdd')))
  })

  it('offers the cloud GPU too once one is set up, with all six presets and their cloud times', async () => {
    setCloudAllowed(true)
    const { container } = render(DenoiseStep)
    expect(targets(container)).toEqual(['local', 'cloud', 'off'])

    await fireEvent.click(card(container, 'cloud'))
    await waitFor(() => expect(presetIds(container)).toHaveLength(6))
    expect(session.denoiseTarget).toBe('cloud')
    const first = /** @type {HTMLElement} */ (container.querySelector('[data-preset="mangajanai-2x"]'))
    expect(first.textContent).toContain(t('denoise.time.cloud', { duration: t('denoise.duration.seconds', { value: '4' }) }))
    expect(chosenFiles().filter((id) => LOCAL_PACKAGE.includes(id))).toEqual([])
  })

  it('queues the local package and its runtime for this computer, and says what it costs', async () => {
    const { container } = render(DenoiseStep)
    await fireEvent.click(card(container, 'local'))

    await waitFor(() => expect(presetIds(container)).toEqual(['waifu2x-scan-4x-n2']))
    expect(container.querySelector('[data-preset] [data-time]')?.getAttribute('data-time')).toBe('unknown')
    expect(container.querySelector('[data-denoise-download]')).not.toBeNull()
    const files = chosenFiles()
    expect(files).toEqual(expect.arrayContaining(LOCAL_PACKAGE))
    expect(files[0]).toBe(RUNTIME_ID)
    await waitFor(async () => expect(await mock.readSettings()).toEqual(expect.objectContaining({ denoiseTarget: 'local', denoisePreset: 'waifu2x-scan-4x-n2' })))
  })

  it('downloads nothing and saves off for "Don\'t use denoise"', async () => {
    const { container } = render(DenoiseStep)
    await fireEvent.click(card(container, 'local'))
    await fireEvent.click(card(container, 'off'))

    await waitFor(() => expect(session.denoiseTarget).toBe('off'))
    expect(chosenFiles().filter((id) => LOCAL_PACKAGE.includes(id))).toEqual([])
    expect(presetIds(container)).toEqual([])
    await waitFor(async () => expect(await mock.readSettings()).toEqual(expect.objectContaining({ denoiseTarget: 'off' })))
  })
})
