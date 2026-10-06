/**
 * The Settings dialog's token row, mounted.
 *
 * `SettingsDialog.test.js` beside this file asserts the one piece of
 * arithmetic the dialog exports and mounts nothing, which is right for a pure
 * function. This is the other half, and it is the half that could not be
 * written until the suite had a jsdom project:
 * what the interface *does* when the credential store refuses to give the
 * token up. The backend end of that refusal is asserted in
 * `weights.rs` and `settings.rs` - `write_token` answers `NotCleared` and
 * `settings::write` returns `Err` rather than reporting a deletion that did
 * not happen - and until here nothing asserted that a user ever heard about
 * it.
 *
 * The seam is stubbed through `setBackend`, which is what that export is for.
 * A hand-written stub rather than the mock: the dialog opens six calls on
 * mount and only one of them is what this file is about, so the rest answer
 * the emptiest true thing - no models, no accelerators, no sidecar - and the
 * catalogue says a token is saved, because that is what makes Clear pressable.
 *
 * The token lives in **Models**, under Download access, so `open()` opens
 * the screen there and asserts that tab is selected and its panel shown: the
 * panels are all mounted and merely `hidden`, so a query would find the field
 * in a hidden panel too, and a test that cannot fail is not one.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { session, setCloseToTray } from '../state/session.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'

/** The spec `pushModal({kind: 'settings'})` would have handed the dialog. */
const SPEC = {
  id: 'modal-1',
  kind: 'settings',
  titleKey: 'modal.title.settings',
  props: {},
  actions: [{ id: 'close', labelKey: 'shell.action.close' }],
  blocking: false,
  dismissable: true,
  onresolve: null,
}

/**
 * A catalogue with nothing installed and a token saved in the keychain: the
 * one row this file is about, and no rows it is not.
 */
function view() {
  return {
    models: [],
    runtime: {
      installed: false,
      available: false,
      readOnly: false,
      downloading: false,
      path: null,
      version: '',
      flavour: 'stock',
      flavours: [],
      bytes: null,
    },
    modelsDir: null,
    hasToken: true,
    tokenStore: 'keychain',
  }
}

/** The message `settings::write` rejects with when the store kept the secret. */
const KEPT = 'the credential store kept the token: the entry is locked'

/** @type {ReturnType<typeof vi.fn>} */
let writeSettings

beforeEach(() => {
  writeSettings = vi.fn(async () => ({}))
  setBackend(
    /** @type {any} */ ({
      listModels: vi.fn(async () => view()),
      listAccelerators: vi.fn(async () => ({ providers: [], models: [] })),
      listSidecarModels: vi.fn(async () => []),
      sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
      about: vi.fn(async () => ({ appVersion: '0.0.0-test', facts: [] })),
      subscribe: vi.fn(() => () => {}),
      writeSettings: (/** @type {any} */ patch) => writeSettings(patch),
    }),
  )
})

afterEach(() => {
  cleanup()
  setBackend(null)
  vi.clearAllMocks()
})

/**
 * Mount the screen on Models, check it is the section on show, and wait for the
 * catalogue - which is what enables Clear: the button is disabled until the
 * backend has said there is a token to clear.
 */
async function open() {
  const view = render(SettingsDialog, { props: { spec: { ...SPEC, props: { tab: 'models', anchor: 'access' } } } })

  const tab = view.getByRole('tab', { name: t('settings.section.models') })
  expect(tab.getAttribute('aria-selected')).toBe('true')

  const panel = /** @type {HTMLElement} */ (
    view.container.querySelector(`#${tab.getAttribute('aria-controls')}`)
  )
  expect(panel.hasAttribute('hidden')).toBe(false)

  const field = /** @type {HTMLInputElement} */ (panel.querySelector('#settings-hf-token'))
  const button = (/** @type {string} */ key) =>
    /** @type {HTMLButtonElement} */ (view.getByText(t(key)).closest('button'))
  await waitFor(() => expect(button('settings.models.token.clear').disabled).toBe(false))
  return { view, field, button }
}

describe('a Clear the credential store refuses', () => {
  it('says so, and keeps what the field was holding', async () => {
    writeSettings.mockRejectedValue(new Error(KEPT))
    const { view, field, button } = await open()

    // Before the press: the row says a token is saved and nothing has failed.
    expect(view.getByText(t('settings.models.token.saved'))).not.toBeNull()
    expect(view.queryByText(t('settings.models.token.clearFailed'))).toBeNull()

    await fireEvent.input(field, { target: { value: 'hf_typed' } })
    await fireEvent.click(button('settings.models.token.clear'))

    await waitFor(() =>
      expect(view.getByText(t('settings.models.token.clearFailed'))).not.toBeNull(),
    )
    // The failure replaces the note rather than joining it: "a token is saved"
    // and "the token could not be removed" are one piece of news.
    expect(view.queryByText(t('settings.models.token.saved'))).toBeNull()
    // The press asked for a deletion and did not get one, so nothing about the
    // field has changed - including what the user had typed into it.
    expect(field.value).toBe('hf_typed')
    expect(writeSettings).toHaveBeenCalledWith({ hfToken: '' })
  })

  it('says something else entirely when the write itself failed', async () => {
    // Every other way a Clear can fail - an unwritable settings file, an
    // adapter that is not answering - is a write that did not happen at all.
    // Telling that user their token is still in a credential store sends them
    // to a keychain to look for a secret that is not there.
    writeSettings.mockRejectedValue(new Error('failed to write settings.json: disk full'))
    const { view, button } = await open()

    await fireEvent.click(button('settings.models.token.clear'))

    await waitFor(() =>
      expect(view.getByText(t('settings.models.token.clearFailedOther'))).not.toBeNull(),
    )
    // And specifically **not** the keychain sentence, which is the whole point
    // of telling the two apart: the two rejections are separated by the words
    // `settings::write` chose, because the seam carries one string per call.
    expect(view.queryByText(t('settings.models.token.clearFailed'))).toBeNull()
  })

  it('is taken back by the next Clear the store accepts', async () => {
    writeSettings.mockRejectedValue(new Error(KEPT))
    const { view, field, button } = await open()

    await fireEvent.input(field, { target: { value: 'hf_typed' } })
    await fireEvent.click(button('settings.models.token.clear'))
    await waitFor(() =>
      expect(view.getByText(t('settings.models.token.clearFailed'))).not.toBeNull(),
    )

    // The user unlocked their keychain and pressed again.
    writeSettings.mockResolvedValue({})
    await fireEvent.click(button('settings.models.token.clear'))

    await waitFor(() =>
      expect(view.queryByText(t('settings.models.token.clearFailed'))).toBeNull(),
    )
    expect(field.value).toBe('')
    expect(writeSettings).toHaveBeenCalledTimes(2)
  })

  it('is taken back by a Save, which is a press about the same secret', async () => {
    writeSettings.mockRejectedValue(new Error(KEPT))
    const { view, field, button } = await open()

    await fireEvent.input(field, { target: { value: 'hf_typed' } })
    await fireEvent.click(button('settings.models.token.clear'))
    await waitFor(() =>
      expect(view.getByText(t('settings.models.token.clearFailed'))).not.toBeNull(),
    )

    writeSettings.mockResolvedValue({})
    await fireEvent.click(button('settings.models.token.save'))

    await waitFor(() =>
      expect(view.queryByText(t('settings.models.token.clearFailed'))).toBeNull(),
    )
    expect(field.value).toBe('')
    expect(writeSettings).toHaveBeenLastCalledWith({ hfToken: 'hf_typed' })
  })
})

describe('keep running in background', () => {
  afterEach(() => setCloseToTray(false))

  it('keeps the old value, and says so, when the write is refused', async () => {
    writeSettings.mockRejectedValue(new Error('failed to write settings.json: disk full'))
    const view = render(SettingsDialog, { props: { spec: SPEC } })
    const box = /** @type {HTMLInputElement} */ (view.getByLabelText(t('settings.background.label')))
    expect(session.closeToTray).toBe(false)

    await fireEvent.click(box)
    await waitFor(() => expect(view.getByText(t('settings.background.saveFailed'))).toBeTruthy())
    expect(session.closeToTray).toBe(false)
    expect(box.checked).toBe(false)
    expect(writeSettings).toHaveBeenCalledWith({ closeToTray: true })
  })

  it('takes the new value when the write lands', async () => {
    const view = render(SettingsDialog, { props: { spec: SPEC } })
    const box = /** @type {HTMLInputElement} */ (view.getByLabelText(t('settings.background.label')))

    await fireEvent.click(box)
    await waitFor(() => expect(session.closeToTray).toBe(true))
    expect(writeSettings).toHaveBeenCalledWith({ closeToTray: true })
    expect(view.queryByText(t('settings.background.saveFailed'))).toBe(null)
  })
})
