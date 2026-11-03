/**
 * Settings > Cloud > Nano Banana, mounted: the key is saved with the button
 * or Enter, once however often it is pressed, and Nano Banana is offered
 * after; a key kept for this session only says so; each refusal of the key
 * commands has its own message; a key that changed but could not be read
 * back is still shown as saved; and a model that could not be saved goes
 * back to the one before it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { capabilities } from '../state/capabilities.svelte.js'
import { session } from '../state/session.svelte.js'
import NanoBananaSettings from './NanoBananaSettings.svelte'

/** @type {boolean} */
let cloudAllowed

beforeEach(() => {
  cloudAllowed = session.cloudAllowed
  session.cloudAllowed = true
  capabilities.nanoBanana = false
})

afterEach(() => {
  cleanup()
  session.cloudAllowed = cloudAllowed
  capabilities.nanoBanana = false
  vi.restoreAllMocks()
})

/** A backend holding no key, with each command a spy. @param {Record<string, any>} [overrides] */
function backendWith(overrides = {}) {
  let stored = false
  return {
    nanoBananaHasKey: vi.fn(async () => stored),
    storeNanoBananaKey: vi.fn(async () => { stored = true; return { sessionOnly: false } }),
    deleteNanoBananaKey: vi.fn(async () => { stored = false }),
    readSettings: vi.fn(async () => ({ nanoBananaModel: 'gemini-3.1-flash-image' })),
    writeSettings: vi.fn(async () => ({})),
    ...overrides,
  }
}

/** @param {any} backend */
async function mount(backend) {
  render(NanoBananaSettings, { props: { backend } })
  await vi.waitFor(() => expect(backend.readSettings).toHaveBeenCalled())
  const box = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.key')))
  return { box, save: () => screen.getByRole('button', { name: t('nanoBanana.settings.save') }) }
}

describe('the Nano Banana key', () => {
  it('is typed into a box that does not correct, capitalise or remember it', async () => {
    const { box } = await mount(backendWith())
    expect(box.type).toBe('password')
    expect(box.getAttribute('autocomplete')).toBe('off')
    expect(box.getAttribute('autocapitalize')).toBe('off')
    expect(box.getAttribute('spellcheck')).toBe('false')
    expect(Number(box.getAttribute('maxlength'))).toBeGreaterThan(39)
  })

  it('is saved with Enter, and Nano Banana is offered after', async () => {
    const backend = backendWith()
    const { box } = await mount(backend)
    await fireEvent.input(box, { target: { value: 'AIza-key' } })
    await fireEvent.submit(/** @type {HTMLFormElement} */ (box.form))
    await vi.waitFor(() => expect(capabilities.nanoBanana).toBe(true))
    expect(backend.storeNanoBananaKey).toHaveBeenCalledWith({ key: 'AIza-key' })
    expect(box.value).toBe('')
    expect(screen.getByText(t('nanoBanana.settings.keySaved'))).toBeTruthy()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('is saved once however often Save is pressed while the first is going', async () => {
    /** @type {() => void} */
    let finish = () => {}
    const backend = backendWith({
      storeNanoBananaKey: vi.fn(() => new Promise((resolve) => { finish = () => resolve({ sessionOnly: false }) })),
    })
    const { box, save } = await mount(backend)
    await fireEvent.input(box, { target: { value: 'AIza-key' } })
    await fireEvent.click(save())
    expect(/** @type {HTMLButtonElement} */ (save()).disabled).toBe(true)
    expect(box.disabled).toBe(true)
    await fireEvent.click(save())
    await fireEvent.submit(/** @type {HTMLFormElement} */ (box.form))
    finish()
    await vi.waitFor(() => expect(box.disabled).toBe(false))
    expect(backend.storeNanoBananaKey).toHaveBeenCalledTimes(1)
  })

  it('says when it is kept only until the app closes', async () => {
    const backend = backendWith()
    backend.storeNanoBananaKey.mockImplementation(async () => {
      backend.nanoBananaHasKey.mockResolvedValue(true)
      return { sessionOnly: true }
    })
    backend.deleteNanoBananaKey.mockImplementation(async () => {
      backend.nanoBananaHasKey.mockResolvedValue(false)
    })
    const { box, save } = await mount(backend)
    await fireEvent.input(box, { target: { value: 'AIza-key' } })
    await fireEvent.click(save())
    expect(await screen.findByText(t('nanoBanana.settings.sessionOnly'))).toBeTruthy()
    // Removed, the notice goes with it.
    const remove = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('nanoBanana.settings.remove') }))
    await vi.waitFor(() => expect(remove.disabled).toBe(false))
    await fireEvent.click(remove)
    expect(await screen.findByText(t('nanoBanana.settings.keyMissing'))).toBeTruthy()
  })

  for (const code of ['nanoBanana.key.noStore', 'nanoBanana.key.invalid', 'nanoBanana.key.failed']) {
    it(`says what ${code} means, not a generic failure`, async () => {
      const backend = backendWith({ storeNanoBananaKey: vi.fn(async () => { throw code }) })
      vi.spyOn(console, 'error').mockImplementation(() => {})
      const { box, save } = await mount(backend)
      await fireEvent.input(box, { target: { value: 'AIza-key' } })
      await fireEvent.click(save())
      expect((await screen.findByRole('alert')).textContent).toBe(t(code))
      expect(box.value).toBe('AIza-key')
      expect(capabilities.nanoBanana).toBe(false)
    })
  }

  it('falls back to the general message for a refusal it does not know', async () => {
    const backend = backendWith({ storeNanoBananaKey: vi.fn(async () => { throw new Error('keyring locked') }) })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const { box, save } = await mount(backend)
    await fireEvent.input(box, { target: { value: 'AIza-key' } })
    await fireEvent.click(save())
    expect((await screen.findByRole('alert')).textContent).toBe(t('nanoBanana.settings.failed'))
  })

  it('is shown as saved when it changed and could not be read back, and Nano Banana is still refreshed', async () => {
    const backend = backendWith()
    const { box, save } = await mount(backend)
    backend.nanoBananaHasKey.mockRejectedValueOnce(new Error('keyring locked'))
    backend.nanoBananaHasKey.mockResolvedValue(true)
    vi.spyOn(console, 'error').mockImplementation(() => {})
    await fireEvent.input(box, { target: { value: 'AIza-key' } })
    await fireEvent.click(save())
    await vi.waitFor(() => expect(capabilities.nanoBanana).toBe(true))
    expect(screen.queryByRole('alert')).toBeNull()
    expect(screen.getByText(t('nanoBanana.settings.keySaved'))).toBeTruthy()
  })
})

describe('the Nano Banana model', () => {
  it('is saved when picked', async () => {
    const backend = backendWith()
    await mount(backend)
    const select = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('nanoBanana.settings.model')))
    await vi.waitFor(() => expect(select.value).toBe('gemini-3.1-flash-image'))
    await fireEvent.change(select, { target: { value: 'gemini-3-pro-image' } })
    await vi.waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith({ nanoBananaModel: 'gemini-3-pro-image' }))
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('goes back to the one before when it could not be saved, and says so', async () => {
    const backend = backendWith({ writeSettings: vi.fn(async () => { throw new Error('disk') }) })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    await mount(backend)
    const select = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('nanoBanana.settings.model')))
    await vi.waitFor(() => expect(select.value).toBe('gemini-3.1-flash-image'))
    await fireEvent.change(select, { target: { value: 'gemini-3-pro-image' } })
    expect((await screen.findByRole('alert')).textContent).toBe(t('nanoBanana.settings.modelFailed'))
    expect(select.value).toBe('gemini-3.1-flash-image')
  })
})
