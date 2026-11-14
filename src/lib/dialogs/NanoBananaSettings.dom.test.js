/**
 * Settings > Cloud > Nano Banana, mounted: profiles drawn like the endpoint
 * list. A profile is added with a name, a key and a model, and the key box
 * does not correct, capitalise or remember; the list shows each profile and
 * which is active; picking another makes it active; rename, a new key, a
 * model change and delete with a confirm each go through their own command;
 * each refusal says what it means; Nano Banana is refreshed after a change;
 * there is no finishing here any more; and the two sending choices still
 * save as before.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte'
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

const PRO = 'gemini-3-pro-image'
const FLASH = 'gemini-3.1-flash-image'

/**
 * A backend that keeps profiles as native does, with each command a spy.
 *
 * @param {Array<{id: string, name: string, model: string, hasKey?: boolean|null, sessionOnly?: boolean}>} [start]
 * @param {Record<string, any>} [overrides]
 */
function backendWith(start = [], overrides = {}) {
  const state = {
    active: /** @type {string|null} */ (start[0]?.id ?? null),
    profiles: start.map((profile) => ({ hasKey: true, sessionOnly: false, ...profile })),
  }
  let made = 0
  const view = () => structuredClone(state)
  const at = (/** @type {string} */ id) => {
    const found = state.profiles.find((profile) => profile.id === id)
    if (!found) throw new Error('nanoBanana.profile.missing')
    return found
  }
  return {
    state,
    listNanoBananaProfiles: vi.fn(async () => view()),
    createNanoBananaProfile: vi.fn(async ({ name, model }) => {
      made += 1
      const id = `p${made}`
      state.profiles.push({ id, name, model, hasKey: true, sessionOnly: false })
      state.active ??= id
      return view()
    }),
    updateNanoBananaProfile: vi.fn(async ({ profileId, name, model }) => {
      const profile = at(profileId)
      if (name) profile.name = name
      if (model) profile.model = model
      return view()
    }),
    selectNanoBananaProfile: vi.fn(async ({ profileId }) => { at(profileId); state.active = profileId; return view() }),
    storeNanoBananaKey: vi.fn(async ({ profileId }) => { at(profileId).hasKey = true; return view() }),
    deleteNanoBananaProfile: vi.fn(async ({ profileId }) => {
      state.profiles = state.profiles.filter((profile) => profile.id !== profileId)
      if (state.active === profileId) state.active = state.profiles[0]?.id ?? null
      return view()
    }),
    nanoBananaHasKey: vi.fn(async () => Boolean(state.profiles.find((profile) => profile.id === state.active)?.hasKey)),
    readSettings: vi.fn(async () => ({})),
    writeSettings: vi.fn(async () => ({})),
    ...overrides,
  }
}

/** @param {any} backend */
async function mount(backend) {
  render(NanoBananaSettings, { props: { backend } })
  await vi.waitFor(() => expect(backend.readSettings).toHaveBeenCalled())
}

/** The row of the profile named `name`. @param {string} name */
const rowOf = (name) => /** @type {HTMLElement} */ (screen.getByText(name, { selector: 'label' }).closest('li'))

describe('Nano Banana profiles', () => {
  it('says there are none yet, and adds the first with a name, a key and a model', async () => {
    const backend = backendWith()
    await mount(backend)
    expect(await screen.findByText(t('nanoBanana.settings.empty'))).toBeTruthy()
    await fireEvent.click(screen.getByRole('button', { name: t('nanoBanana.settings.add') }))
    const key = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.key')))
    expect(key.type).toBe('password')
    expect([key.getAttribute('autocomplete'), key.getAttribute('autocapitalize'), key.getAttribute('spellcheck')]).toEqual(['off', 'off', 'false'])
    expect(Number(key.getAttribute('maxlength'))).toBeGreaterThan(39)
    const name = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.name')))
    await fireEvent.input(name, { target: { value: 'Work' } })
    await fireEvent.input(key, { target: { value: 'AIza-key' } })
    await fireEvent.change(screen.getByLabelText(t('nanoBanana.settings.model')), { target: { value: PRO } })
    await fireEvent.submit(/** @type {HTMLFormElement} */ (key.form))
    await vi.waitFor(() => expect(capabilities.nanoBanana).toBe(true))
    expect(backend.createNanoBananaProfile).toHaveBeenCalledWith({ name: 'Work', model: PRO, key: 'AIza-key' })
    await vi.waitFor(() => expect(screen.queryByLabelText(t('nanoBanana.settings.key'))).toBeNull())
    const row = rowOf('Work')
    expect(within(row).getByText(t('nanoBanana.settings.active'))).toBeTruthy()
    expect(within(row).getByText(t('nanoBanana.settings.keySaved'))).toBeTruthy()
  })

  it('creates once however often Save is pressed while the first is going', async () => {
    /** @type {() => void} */
    let finish = () => {}
    const backend = backendWith([], {
      createNanoBananaProfile: vi.fn(() => new Promise((resolve) => { finish = () => resolve({ active: 'p1', profiles: [] }) })),
    })
    await mount(backend)
    await fireEvent.click(await screen.findByRole('button', { name: t('nanoBanana.settings.add') }))
    const key = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.key')))
    await fireEvent.input(screen.getByLabelText(t('nanoBanana.settings.name')), { target: { value: 'Work' } })
    await fireEvent.input(key, { target: { value: 'AIza-key' } })
    await fireEvent.submit(/** @type {HTMLFormElement} */ (key.form))
    await fireEvent.submit(/** @type {HTMLFormElement} */ (key.form))
    expect(key.disabled).toBe(true)
    finish()
    await vi.waitFor(() => expect(screen.queryByLabelText(t('nanoBanana.settings.key'))).toBeNull())
    expect(backend.createNanoBananaProfile).toHaveBeenCalledTimes(1)
  })

  it('lists every profile, says which is active, and makes another active when it is picked', async () => {
    const backend = backendWith([
      { id: 'pa', name: 'Work', model: PRO },
      { id: 'pb', name: 'Home', model: FLASH, hasKey: false },
    ])
    await mount(backend)
    await screen.findByText('Home', { selector: 'label' })
    expect(screen.getByText(t('nanoBanana.settings.switchNote'))).toBeTruthy()
    expect(within(rowOf('Home')).getByText(t('nanoBanana.settings.keyMissing'))).toBeTruthy()
    expect(within(rowOf('Home')).queryByText(t('nanoBanana.settings.active'))).toBeNull()
    await fireEvent.click(screen.getByLabelText(t('nanoBanana.settings.useProfile', { name: 'Home' })))
    await vi.waitFor(() => expect(within(rowOf('Home')).getByText(t('nanoBanana.settings.active'))).toBeTruthy())
    expect(backend.selectNanoBananaProfile).toHaveBeenCalledWith({ profileId: 'pb' })
    expect(backend.nanoBananaHasKey).toHaveBeenCalled()
  })

  it('renames a profile and changes its model', async () => {
    const backend = backendWith([{ id: 'pa', name: 'Work', model: FLASH }])
    await mount(backend)
    await fireEvent.click(within(await screen.findByRole('listitem')).getByRole('button', { name: t('nanoBanana.settings.rename') }))
    const box = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.renameLabel', { name: 'Work' })))
    expect(box.value).toBe('Work')
    await fireEvent.input(box, { target: { value: 'Studio' } })
    await fireEvent.submit(/** @type {HTMLFormElement} */ (box.form))
    await screen.findByText('Studio', { selector: 'label' })
    expect(backend.updateNanoBananaProfile).toHaveBeenCalledWith({ profileId: 'pa', name: 'Studio' })
    const model = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('nanoBanana.settings.modelOf', { name: 'Studio' })))
    expect(model.value).toBe(FLASH)
    await fireEvent.change(model, { target: { value: PRO } })
    await vi.waitFor(() => expect(backend.updateNanoBananaProfile).toHaveBeenCalledWith({ profileId: 'pa', model: PRO }))
  })

  it('replaces one profile\'s key, write-only', async () => {
    const backend = backendWith([{ id: 'pa', name: 'Work', model: FLASH, hasKey: false }])
    await mount(backend)
    await fireEvent.click(within(await screen.findByRole('listitem')).getByRole('button', { name: t('nanoBanana.settings.addKey') }))
    const box = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.keyLabel', { name: 'Work' })))
    expect(box.type).toBe('password')
    await fireEvent.input(box, { target: { value: 'AIza-new' } })
    await fireEvent.submit(/** @type {HTMLFormElement} */ (box.form))
    await vi.waitFor(() => expect(within(rowOf('Work')).getByText(t('nanoBanana.settings.keySaved'))).toBeTruthy())
    expect(backend.storeNanoBananaKey).toHaveBeenCalledWith({ profileId: 'pa', key: 'AIza-new' })
  })

  it('deletes only after the confirm, and the next profile becomes active', async () => {
    const backend = backendWith([{ id: 'pa', name: 'Work', model: FLASH }, { id: 'pb', name: 'Home', model: FLASH }])
    await mount(backend)
    await fireEvent.click(within(await screen.findByText('Work', { selector: 'label' }).then(() => rowOf('Work')))
      .getByRole('button', { name: t('nanoBanana.settings.remove') }))
    expect(screen.getByText(t('nanoBanana.settings.removeConfirm', { name: 'Work' }))).toBeTruthy()
    expect(backend.deleteNanoBananaProfile).not.toHaveBeenCalled()
    await fireEvent.click(screen.getByRole('button', { name: t('nanoBanana.settings.cancel') }))
    expect(screen.queryByText(t('nanoBanana.settings.removeConfirm', { name: 'Work' }))).toBeNull()
    await fireEvent.click(within(rowOf('Work')).getByRole('button', { name: t('nanoBanana.settings.remove') }))
    await fireEvent.click(screen.getByRole('button', { name: t('nanoBanana.settings.removeButton') }))
    await vi.waitFor(() => expect(screen.queryByText('Work', { selector: 'label' })).toBeNull())
    expect(backend.deleteNanoBananaProfile).toHaveBeenCalledWith({ profileId: 'pa' })
    expect(within(rowOf('Home')).getByText(t('nanoBanana.settings.active'))).toBeTruthy()
  })

  it('says when a key is kept only until the app closes, or could not be checked', async () => {
    await mount(backendWith([
      { id: 'pa', name: 'Work', model: FLASH, sessionOnly: true },
      { id: 'pb', name: 'Home', model: FLASH, hasKey: null },
    ]))
    expect(within(await screen.findByText('Work', { selector: 'label' }).then(() => rowOf('Work')))
      .getByText(t('nanoBanana.settings.sessionOnly'))).toBeTruthy()
    expect(within(rowOf('Home')).getByText(t('nanoBanana.settings.keyUnknown'))).toBeTruthy()
  })

  for (const code of ['nanoBanana.key.noStore', 'nanoBanana.key.invalid', 'nanoBanana.key.failed', 'nanoBanana.profile.badName', 'nanoBanana.profile.tooMany']) {
    it(`says what ${code} means, and keeps the form`, async () => {
      const backend = backendWith([], { createNanoBananaProfile: vi.fn(async () => { throw code }) })
      vi.spyOn(console, 'error').mockImplementation(() => {})
      await mount(backend)
      await fireEvent.click(await screen.findByRole('button', { name: t('nanoBanana.settings.add') }))
      const key = /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.key')))
      await fireEvent.input(screen.getByLabelText(t('nanoBanana.settings.name')), { target: { value: 'Work' } })
      await fireEvent.input(key, { target: { value: 'AIza-key' } })
      await fireEvent.submit(/** @type {HTMLFormElement} */ (key.form))
      expect((await screen.findByRole('alert')).textContent).toBe(t(code))
      expect(key.value).toBe('AIza-key')
      expect(capabilities.nanoBanana).toBe(false)
    })
  }

  it('says when the profile could not be made active', async () => {
    const backend = backendWith([{ id: 'pa', name: 'Work', model: FLASH }, { id: 'pb', name: 'Home', model: FLASH }], {
      selectNanoBananaProfile: vi.fn(async () => { throw new Error('disk') }),
    })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    await mount(backend)
    await fireEvent.click(await screen.findByLabelText(t('nanoBanana.settings.useProfile', { name: 'Home' })))
    expect((await screen.findByRole('alert')).textContent).toBe(t('nanoBanana.settings.selectFailed'))
  })

  it('offers to try again when the list could not be read', async () => {
    const backend = backendWith([{ id: 'pa', name: 'Work', model: FLASH }])
    backend.listNanoBananaProfiles.mockRejectedValueOnce(new Error('keyring'))
    vi.spyOn(console, 'error').mockImplementation(() => {})
    await mount(backend)
    expect((await screen.findByRole('alert')).textContent).toContain(t('nanoBanana.settings.loadFailed'))
    await fireEvent.click(screen.getByRole('button', { name: t('nanoBanana.settings.retry') }))
    expect(await screen.findByText('Work', { selector: 'label' })).toBeTruthy()
  })

  it('has no finishing of its own: the shared Post-processing choice finishes Nano Banana', async () => {
    await mount(backendWith([{ id: 'pa', name: 'Work', model: FLASH }]))
    await screen.findByText('Work', { selector: 'label' })
    expect(screen.queryByText(t('settings.postprocessing.method'))).toBeNull()
    expect(screen.queryByRole('combobox', { name: /finish/i })).toBeNull()
  })
})

describe('what Nano Banana sends', () => {
  const showCrop = () => /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.showCrop')))
  const restrictive = () => /** @type {HTMLInputElement} */ (screen.getByLabelText(t('nanoBanana.settings.restrictive')))

  it('is off by default, and shows what settings hold', async () => {
    await mount(backendWith())
    expect([showCrop().checked, restrictive().checked]).toEqual([false, false])
    cleanup()
    const backend = backendWith([], { readSettings: vi.fn(async () => ({ nanoBananaShowCrop: true, nanoBananaRestrictive: true })) })
    await mount(backend)
    await vi.waitFor(() => expect([showCrop().checked, restrictive().checked]).toEqual([true, true]))
  })

  it('saves each choice under its own key', async () => {
    const backend = backendWith()
    await mount(backend)
    await fireEvent.click(showCrop())
    await vi.waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith({ nanoBananaShowCrop: true }))
    await fireEvent.click(restrictive())
    await vi.waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith({ nanoBananaRestrictive: true }))
    expect(showCrop().checked && restrictive().checked).toBe(true)
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('goes back to what it was when it could not be saved, and says so', async () => {
    const backend = backendWith([], { writeSettings: vi.fn(async () => { throw new Error('disk') }) })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    await mount(backend)
    await fireEvent.click(restrictive())
    expect((await screen.findByRole('alert')).textContent).toBe(t('nanoBanana.settings.sendingFailed'))
    expect(restrictive().checked).toBe(false)
  })
})
