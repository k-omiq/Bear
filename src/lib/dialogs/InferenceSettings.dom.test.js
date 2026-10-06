/**
 * Settings > Cloud against the mock backend (every delay zero): the way in
 * from elsewhere in the app, the status line, the permission switch, the
 * endpoint list and what each row does, the manual form, the recovery
 * section, and the provisioner opened in place.
 */
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { t } from '../i18n/index.js'
import ModalHost from '../shell/ModalHost.svelte'
import { app, clearNotices, closeAllModals, pushModal } from '../state/app.svelte.js'
import { cloud, openCloudSettings, stopCloud } from '../state/cloud.svelte.js'
import { cloudGpu } from '../state/cloudgpu.svelte.js'
import { session, setCloudAllowed } from '../state/session.svelte.js'
import InferenceSettings, { isValidEndpointUrl, isValidProfileName } from './InferenceSettings.svelte'
import { forgetUnfinished, setup } from './provisioning.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const MODAL_KEY = 'settings.inference.provider.modal'
const BEAM_KEY = 'settings.inference.provider.beam'

/** An endpoint setup made: its id is the installation id. */
const MADE = {
  provider: /** @type {const} */ ('modal'),
  id: 'mc-abc123',
  name: 'Modal (mc-abc123)',
  url: 'https://ws--mc-mc-abc123-gateway.modal.run/mc/v1',
  token: true,
  createdAtMs: 1,
}
/** An endpoint added by hand. */
const MANUAL = {
  provider: /** @type {const} */ ('beam'),
  id: 'ep_manual',
  name: 'Studio Beam',
  url: 'https://studio.app.beam.cloud/mc/v1',
  token: true,
  createdAtMs: 2,
}

/** A Modal endpoint added by hand. */
const MANUAL_MODAL = {
  provider: /** @type {const} */ ('modal'),
  id: 'ep_studio',
  name: 'Studio Modal',
  url: 'https://studio--gateway.modal.run/mc/v1',
  token: true,
  createdAtMs: 3,
}

/** A second setup, in another Modal account. */
const OTHER_ACCOUNT = {
  provider: /** @type {const} */ ('modal'),
  id: 'mc-def456',
  name: 'Modal (mc-def456)',
  url: 'https://studio--mc-def456-gateway.modal.run/mc/v1',
  token: true,
  createdAtMs: 4,
}

/** Local storage for the test: the node running the suite has none. */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() {
      return values.size
    },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
})

afterEach(() => {
  cleanup()
  closeAllModals()
  setup.run = null
  forgetUnfinished()
  stopCloud()
  setCloudAllowed(false)
  clearNotices()
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/**
 * A mock backend holding these endpoints, with the permission as given.
 *
 * @param {{allowed?: boolean, endpoints?: Array<typeof MADE|typeof MANUAL>, selected?: {type: string, profile_id?: string}}} [options]
 */
async function seeded({ allowed = true, endpoints = [], selected = { type: 'local' } } = {}) {
  const backend = createMockBackend({ timing: ZERO })
  await backend.writeSettings({ cloudEngines: allowed ? 'allowed' : 'blocked' })
  setCloudAllowed(allowed)
  /** @type {any} */
  const config = { schemaVersion: 1, selectedTarget: selected, modalProfiles: {}, beamProfiles: {} }
  for (const ep of endpoints) {
    config[ep.provider === 'beam' ? 'beamProfiles' : 'modalProfiles'][ep.id] = {
      id: ep.id,
      name: ep.name,
      endpointUrl: ep.url,
      canonicalOrigin: new URL(ep.url).origin,
      canonicalOriginFingerprint: '',
      createdAtMs: ep.createdAtMs,
      updatedAtMs: ep.createdAtMs,
    }
  }
  await backend.writeInferenceConfig({ config })
  for (const ep of endpoints) {
    if (!ep.token) continue
    await backend.storeCloudSecret({
      provider: ep.provider,
      profileId: ep.id,
      role: 'runtime',
      secret: 'rt-secret',
      ...(ep.provider === 'modal' ? { tokenId: 'rt-id' } : {}),
    })
  }
  setBackend(backend)
  return backend
}

/** @param {import('../api/backend.js').Backend} backend */
function show(backend) {
  return render(InferenceSettings, { props: { backend } })
}

/** @param {string} name */
const button = (name) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name }))

/** The status line, by what it says. @param {string} text */
const statusSays = (text) => waitFor(() => expect(screen.getAllByRole('status')[0].textContent?.trim()).toBe(text))

/** One endpoint's row, by its name. @param {string} name */
function row(name) {
  return /** @type {HTMLElement} */ (screen.getByText(name, { selector: 'label' }).closest('li'))
}

/** @param {string} name */
function defaultRadio(name) {
  return /** @type {HTMLInputElement} */ (
    screen.getByRole('radio', { name: t('settings.inference.endpoints.useDefault', { name }) })
  )
}

/**
 * @param {string} label
 * @param {string} value
 */
async function fill(label, value) {
  await fireEvent.input(screen.getByLabelText(label), { target: { value } })
}

const attentionHeading = () => screen.queryByRole('heading', { name: t('settings.inference.recovery.title') })

describe('InferenceSettings helper validations', () => {
  it('validates profile names correctly', () => {
    expect(isValidProfileName('Production GPU')).toBe(true)
    expect(isValidProfileName('Worker-1')).toBe(true)
    expect(isValidProfileName('a')).toBe(true)
    expect(isValidProfileName('N'.repeat(128))).toBe(true)

    expect(isValidProfileName('')).toBe(false)
    expect(isValidProfileName(' leading-space')).toBe(false)
    expect(isValidProfileName('trailing-space ')).toBe(false)
    expect(isValidProfileName('control\nchar')).toBe(false)
    expect(isValidProfileName('N'.repeat(129))).toBe(false)
  })

  it('validates HTTPS endpoint URLs correctly', () => {
    expect(isValidEndpointUrl('https://modal-cleaner.run.modal.com/mc/v1')).toBe(true)
    expect(isValidEndpointUrl('https://api.beam.cloud:8443/endpoint')).toBe(true)
    expect(isValidEndpointUrl('https://gpu-worker.internal-cloud.org/v1/infer')).toBe(true)

    expect(isValidEndpointUrl('')).toBe(false)
    expect(isValidEndpointUrl('http://insecure.example.com')).toBe(false)
    expect(isValidEndpointUrl('  https://api.beam.cloud/ep')).toBe(false)
    expect(isValidEndpointUrl('https://api.beam.cloud/ep  ')).toBe(false)
    expect(isValidEndpointUrl('https://user:pass@api.modal.com/mc/v1')).toBe(false)
    expect(isValidEndpointUrl('https://api.modal.com/mc/v1?query=1')).toBe(false)
    expect(isValidEndpointUrl('https://api.modal.com/mc/v1#fragment')).toBe(false)
    expect(isValidEndpointUrl('https://localhost:8080')).toBe(false)
    expect(isValidEndpointUrl('https://mybox.local/ep')).toBe(false)
    expect(isValidEndpointUrl('https://internal.internal/ep')).toBe(false)
    expect(isValidEndpointUrl('https://127.0.0.1:443/ep')).toBe(false)
    expect(isValidEndpointUrl('https://[::1]:8443/ep')).toBe(false)
    expect(isValidEndpointUrl('https://2130706433/')).toBe(false)
  })
})

describe('the way in', () => {
  /** @param {string} id */
  const tab = (id) => screen.findByRole('tab', { name: t(`settings.section.${id}`) })

  it('opens Settings on the Cloud tab when the app asks for it', async () => {
    await seeded()
    render(ModalHost)
    openCloudSettings()
    // `openCloudSettings` still asks for the old `inference` id; it lands on Cloud.
    expect((await tab('cloud')).getAttribute('aria-selected')).toBe('true')
    expect((await tab('general')).getAttribute('aria-selected')).toBe('false')
  })

  it('opens on General when the tab asked for is not one it has', async () => {
    await seeded()
    render(ModalHost)
    pushModal({ kind: 'settings', props: { tab: 'billing' } })
    expect((await tab('general')).getAttribute('aria-selected')).toBe('true')
    expect((await tab('cloud')).getAttribute('aria-selected')).toBe('false')
  })
})

describe('the status line and the permission switch', () => {
  it('off: says nothing is sent; turning it on saves the permission and reads readiness again', async () => {
    const backend = await seeded({ allowed: false })
    const write = vi.spyOn(backend, 'writeSettings')
    show(backend)
    await statusSays(t('settings.inference.status.off'))
    const on = screen.getByRole('radio', { name: t('settings.inference.permission.on') })
    expect(on.getAttribute('aria-checked')).toBe('false')

    await fireEvent.click(on)
    await waitFor(() => expect(session.cloudAllowed).toBe(true))
    expect(write).toHaveBeenLastCalledWith(expect.objectContaining({ cloudEngines: 'allowed' }))
    expect(on.getAttribute('aria-checked')).toBe('true')
    await statusSays(t('settings.inference.status.attention', { reasonKey: 'settings.inference.reason.none' }))
  })

  it('puts the switch back when the permission cannot be saved', async () => {
    const backend = await seeded({ allowed: false })
    vi.spyOn(backend, 'writeSettings').mockRejectedValueOnce(new Error('disk full'))
    show(backend)
    await fireEvent.click(screen.getByRole('radio', { name: t('settings.inference.permission.on') }))
    await waitFor(() => expect(app.notices.some((notice) => notice.key === 'notice.cloud.permissionFailed')).toBe(true))
    expect(session.cloudAllowed).toBe(false)
    await statusSays(t('settings.inference.status.off'))
  })

  it('ready: names the default endpoint, and nothing needs attention', async () => {
    const backend = await seeded({ endpoints: [MADE, MANUAL], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    expect(attentionHeading()).toBeNull()
    // Settings has no line saying cloud execution is unavailable any more.
    expect(document.body.textContent).not.toMatch(/unavailable/i)
  })

  it('needs attention: the default endpoint has no token, and adding one fixes it', async () => {
    const backend = await seeded({ endpoints: [{ ...MADE, token: false }], selected: { type: 'modal', profile_id: MADE.id } })
    const store = vi.spyOn(backend, 'storeCloudSecret')
    show(backend)
    await statusSays(t('settings.inference.status.attention', { reasonKey: 'settings.inference.reason.noSecret' }))

    const section = /** @type {HTMLElement} */ (
      screen.getByRole('region', { name: t('settings.inference.recovery.title') })
    )
    expect(within(section).getByText(t('settings.inference.recovery.noToken', { name: MADE.name }))).toBeTruthy()
    expect(within(section).getByRole('button', { name: t('settings.inference.recovery.newToken') })).toBeTruthy()

    await fireEvent.click(within(section).getByRole('button', { name: t('settings.inference.token.add') }))
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText(t('settings.inference.token.modalTokenId'))))
    await fill(t('settings.inference.token.modalTokenId'), 'pt-id')
    await fill(t('settings.inference.token.modalTokenSecret'), 'pt-secret')
    await fireEvent.click(button(t('settings.inference.token.save')))

    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    expect(store).toHaveBeenCalledWith({
      provider: 'modal',
      profileId: MADE.id,
      role: 'runtime',
      secret: 'pt-secret',
      tokenId: 'pt-id',
    })
    await waitFor(() => expect(within(row(MADE.name)).getByText(t('settings.inference.token.saved'))).toBeTruthy())
    expect(attentionHeading()).toBeNull()
    expect(document.body.innerHTML).not.toContain('pt-secret')
  })

  it('needs attention: a setup deployed by an older release offers to update its cloud code', async () => {
    const backend = await seeded({ endpoints: [MADE], selected: { type: 'modal', profile_id: MADE.id } })
    cloudGpu.unsupported = 'outdated'
    try {
      show(backend)
      const section = await screen.findByRole('region', { name: t('settings.inference.recovery.title') })
      expect(within(section).getByText(t('settings.inference.recovery.outdated', { name: MADE.name }))).toBeTruthy()
      expect(within(section).getByRole('button', { name: t('settings.inference.recovery.update') })).toBeTruthy()
    } finally {
      cloudGpu.unsupported = null
    }
  })
})

describe('updating a setup', () => {
  it('says when a setup runs older cloud code, and offers Update', async () => {
    const backend = await seeded({ endpoints: [MADE], selected: { type: 'modal', profile_id: MADE.id } })
    backend.checkCloudRelease = vi.fn(async () => ({ expected: 'a'.repeat(64), deployed: 'b'.repeat(64), current: false }))
    show(backend)
    const section = await screen.findByRole('region', { name: t('settings.inference.recovery.title') })
    expect(within(section).getByText(t('settings.inference.recovery.olderCode', { name: MADE.name }))).toBeTruthy()
    expect(within(section).getByRole('button', { name: t('settings.inference.recovery.update') })).toBeTruthy()
    expect(backend.checkCloudRelease).toHaveBeenCalledWith({ provider: 'modal', profileId: MADE.id })
  })

  it('says nothing about code a setup runs when it is current or cannot answer', async () => {
    for (const answer of [
      async () => ({ expected: 'a'.repeat(64), deployed: 'a'.repeat(64), current: true }),
      async () => { throw new Error('unreachable') },
    ]) {
      const backend = await seeded({ endpoints: [MADE], selected: { type: 'modal', profile_id: MADE.id } })
      backend.checkCloudRelease = vi.fn(answer)
      const view = show(backend)
      await waitFor(() => expect(backend.checkCloudRelease).toHaveBeenCalled())
      await waitFor(() => expect(within(row(MADE.name)).getByText(t('settings.inference.token.saved'))).toBeTruthy())
      expect(screen.queryByText(t('settings.inference.recovery.olderCode', { name: MADE.name }))).toBeNull()
      view.unmount()
    }
  })

  it('every setup-made Modal endpoint has Update, which opens its update', async () => {
    const backend = await seeded({ endpoints: [MADE, MANUAL], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await waitFor(() => expect(within(row(MADE.name)).getByText(t('settings.inference.token.saved'))).toBeTruthy())
    expect(within(row(MANUAL.name)).queryByRole('button', { name: t('settings.inference.endpoints.update') })).toBeNull()
    await fireEvent.click(within(row(MADE.name)).getByRole('button', { name: t('settings.inference.endpoints.update') }))
    expect(await screen.findByText(t('settings.cloud.setup.update.optionsNote'))).toBeTruthy()
  })
})

describe('the endpoint list', () => {
  it('shows each endpoint with its provider, host, token and which one is the default', async () => {
    const backend = await seeded({
      endpoints: [MADE, { ...MANUAL, token: false }],
      selected: { type: 'modal', profile_id: MADE.id },
    })
    show(backend)
    await waitFor(() => expect(within(row(MADE.name)).getByText(t('settings.inference.token.saved'))).toBeTruthy())

    const made = row(MADE.name)
    expect(within(made).getByText(t('settings.inference.endpoints.default'))).toBeTruthy()
    // A Modal endpoint names the account its address is in.
    expect(
      within(made).getByText(t('settings.inference.endpoints.account', { providerKey: MODAL_KEY, account: 'ws' })),
    ).toBeTruthy()
    expect(defaultRadio(MADE.name).checked).toBe(true)

    const manual = row(MANUAL.name)
    expect(within(manual).queryByText(t('settings.inference.endpoints.default'))).toBeNull()
    expect(
      within(manual).getByText(t('settings.inference.endpoints.meta', { providerKey: BEAM_KEY, host: 'studio.app.beam.cloud' })),
    ).toBeTruthy()
    expect(within(manual).getByText(t('settings.inference.token.missing'))).toBeTruthy()
    expect(defaultRadio(MANUAL.name).checked).toBe(false)
  })

  it('Test checks the connection and says what came back', async () => {
    const backend = await seeded({
      endpoints: [MADE, { ...MANUAL, token: false }],
      selected: { type: 'modal', profile_id: MADE.id },
    })
    show(backend)
    await screen.findByText(MADE.name, { selector: 'label' })

    await fireEvent.click(within(row(MADE.name)).getByRole('button', { name: t('settings.inference.endpoints.test') }))
    await waitFor(() => expect(within(row(MADE.name)).getByText(t('settings.inference.health.reachable'))).toBeTruthy())
    expect(within(row(MADE.name)).getByText(t('settings.inference.health.latency', { latency: 42 }))).toBeTruthy()

    await fireEvent.click(within(row(MANUAL.name)).getByRole('button', { name: t('settings.inference.endpoints.test') }))
    await waitFor(() =>
      expect(within(row(MANUAL.name)).getByText(t('settings.inference.health.credentialMissing'))).toBeTruthy(),
    )
  })

  it('choosing another default saves it', async () => {
    const backend = await seeded({ endpoints: [MADE, MANUAL_MODAL], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    await screen.findByText(MANUAL_MODAL.name, { selector: 'label' })

    await fireEvent.click(defaultRadio(MANUAL_MODAL.name))
    await statusSays(t('settings.inference.status.ready', { name: MANUAL_MODAL.name }))
    expect((await backend.readInferenceConfig()).selectedTarget).toEqual({ type: 'modal', profile_id: MANUAL_MODAL.id })
    expect(within(row(MANUAL_MODAL.name)).getByText(t('settings.inference.endpoints.default'))).toBeTruthy()
    expect(defaultRadio(MADE.name).checked).toBe(false)
  })

  it('marks a Beam endpoint Paused: it cannot be the default or be tested', async () => {
    const backend = await seeded({ endpoints: [MADE, MANUAL], selected: { type: 'modal', profile_id: MADE.id } })
    const test = vi.spyOn(backend, 'checkCloudConnection')
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    const beam = row(MANUAL.name)
    expect(within(beam).getByText(t('settings.inference.endpoints.paused'))).toBeTruthy()
    expect(defaultRadio(MANUAL.name).disabled).toBe(true)
    const testButton = /** @type {HTMLButtonElement} */ (within(beam).getByRole('button', { name: t('settings.inference.endpoints.test') }))
    expect(testButton.disabled).toBe(true)
    await fireEvent.click(defaultRadio(MANUAL.name))
    expect((await backend.readInferenceConfig()).selectedTarget).toEqual({ type: 'modal', profile_id: MADE.id })
    expect(test).not.toHaveBeenCalledWith(expect.objectContaining({ provider: 'beam' }))
  })

  it('switches between two Modal accounts without asking for a key', async () => {
    const backend = await seeded({ endpoints: [MADE, OTHER_ACCOUNT], selected: { type: 'modal', profile_id: MADE.id } })
    const store = vi.spyOn(backend, 'storeCloudSecret')
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    await screen.findByText(OTHER_ACCOUNT.name, { selector: 'label' })
    expect(screen.getByText(t('settings.inference.endpoints.switchNote'))).toBeTruthy()
    expect(
      within(row(OTHER_ACCOUNT.name)).getByText(
        t('settings.inference.endpoints.account', { providerKey: MODAL_KEY, account: 'studio' }),
      ),
    ).toBeTruthy()

    await fireEvent.click(defaultRadio(OTHER_ACCOUNT.name))
    await statusSays(t('settings.inference.status.ready', { name: OTHER_ACCOUNT.name }))
    await fireEvent.click(defaultRadio(MADE.name))
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    expect(store).not.toHaveBeenCalled()
    expect(document.querySelector('input[type="password"]')).toBeNull()
  })

  it('says nothing about switching while there is one endpoint', async () => {
    const backend = await seeded({ endpoints: [MADE], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    expect(screen.queryByText(t('settings.inference.endpoints.switchNote'))).toBeNull()
  })

  it('a default that cannot be saved stays where it was', async () => {
    const backend = await seeded({ endpoints: [MADE, MANUAL_MODAL], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))
    await screen.findByText(MANUAL_MODAL.name, { selector: 'label' })
    vi.spyOn(backend, 'selectCloudProfile').mockRejectedValueOnce(new Error('disk full'))

    await fireEvent.click(defaultRadio(MANUAL_MODAL.name))
    expect((await screen.findByRole('alert')).textContent).toBe(t('settings.inference.error.saveFailed'))
    expect(defaultRadio(MADE.name).checked).toBe(true)
    expect(defaultRadio(MANUAL_MODAL.name).checked).toBe(false)
    expect((await backend.readInferenceConfig()).selectedTarget).toEqual({ type: 'modal', profile_id: MADE.id })
  })
})

describe('Rename', () => {
  /** @param {string} name */
  const renameButton = (name) => within(row(name)).getByRole('button', { name: t('settings.inference.rename.action') })

  it('saves a new name and keeps the default and the token', async () => {
    const backend = await seeded({ endpoints: [MADE, OTHER_ACCOUNT], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))

    await fireEvent.click(renameButton(MADE.name))
    const field = screen.getByLabelText(t('settings.inference.rename.label', { name: MADE.name }))
    await waitFor(() => expect(document.activeElement).toBe(field))
    await fireEvent.input(field, { target: { value: 'Personal' } })
    await fireEvent.click(button(t('settings.inference.rename.save')))

    await statusSays(t('settings.inference.status.ready', { name: 'Personal' }))
    const saved = await backend.readInferenceConfig()
    expect(saved.modalProfiles[MADE.id].name).toBe('Personal')
    expect(saved.modalProfiles[MADE.id].endpointUrl).toBe(MADE.url)
    expect(saved.modalProfiles[OTHER_ACCOUNT.id].name).toBe(OTHER_ACCOUNT.name)
    expect(saved.selectedTarget).toEqual({ type: 'modal', profile_id: MADE.id })
    expect(
      (await backend.getCloudSecretSummary({ provider: 'modal', profileId: MADE.id, role: 'runtime' })).present,
    ).toBe(true)
    await waitFor(() => expect(document.activeElement).toBe(renameButton('Personal')))
  })

  it('refuses a blank name and saves nothing', async () => {
    const backend = await seeded({ endpoints: [MADE], selected: { type: 'modal', profile_id: MADE.id } })
    const write = vi.spyOn(backend, 'writeInferenceConfig')
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))

    await fireEvent.click(renameButton(MADE.name))
    await fireEvent.input(screen.getByLabelText(t('settings.inference.rename.label', { name: MADE.name })), {
      target: { value: '   ' },
    })
    await fireEvent.click(button(t('settings.inference.rename.save')))
    expect((await screen.findByRole('alert')).textContent).toBe(t('settings.inference.error.invalidName'))
    expect(write).not.toHaveBeenCalled()
  })

  it('Escape closes the form and leaves the name as it was', async () => {
    const backend = await seeded({ endpoints: [MADE], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))

    await fireEvent.click(renameButton(MADE.name))
    const field = screen.getByLabelText(t('settings.inference.rename.label', { name: MADE.name }))
    await fireEvent.input(field, { target: { value: 'Draft' } })
    await fireEvent.keyDown(field, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByLabelText(t('settings.inference.rename.label', { name: MADE.name }))).toBeNull())
    expect((await backend.readInferenceConfig()).modalProfiles[MADE.id].name).toBe(MADE.name)
  })
})

describe('Remove', () => {
  it('takes an endpoint off this computer and leaves the account alone', async () => {
    const backend = await seeded({ endpoints: [MADE, MANUAL], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await screen.findByText(MANUAL.name, { selector: 'label' })

    await fireEvent.click(within(row(MANUAL.name)).getByRole('button', { name: t('settings.inference.endpoints.remove') }))
    const confirm = /** @type {HTMLElement} */ (
      screen.getByRole('group', { name: t('settings.inference.remove.confirm', { name: MANUAL.name }) })
    )
    await waitFor(() => expect(document.activeElement?.textContent?.trim()).toBe(
      t('settings.inference.remove.confirm', { name: MANUAL.name }),
    ))
    expect(within(confirm).getByText(t('settings.inference.remove.keepNote', { providerKey: BEAM_KEY }))).toBeTruthy()
    // Only what setup made can be found again in the account.
    expect(within(confirm).queryByRole('checkbox')).toBeNull()

    await fireEvent.click(within(confirm).getByRole('button', { name: t('settings.inference.remove.confirmButton') }))
    await waitFor(() => expect(screen.queryByText(MANUAL.name, { selector: 'label' })).toBeNull())
    expect(
      app.notices.some(
        (notice) => notice.key === 'notice.cloud.endpointRemoved' && notice.params.name === MANUAL.name,
      ),
    ).toBe(true)
    const config = await backend.readInferenceConfig()
    expect(config.beamProfiles[MANUAL.id]).toBeUndefined()
    expect(config.selectedTarget).toEqual({ type: 'modal', profile_id: MADE.id })
    const token = await backend.getCloudSecretSummary({ provider: 'beam', profileId: MANUAL.id, role: 'runtime' })
    expect(token.present).toBe(false)
  })

  it('can also delete what setup created: it lists it, asks for the key, and removes the endpoint', async () => {
    const backend = await seeded()
    const credentials = { token_id: 'ak-old', token_secret: 'as-old' }
    const planned = await backend.runCloudProvisioner({
      op: 'plan',
      provider: 'modal',
      params: { credentials, installation_id: MADE.id },
    })
    await backend.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: MADE.id, approved_plan_hash: planned.data.plan_hash },
    })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))

    await fireEvent.click(within(row(MADE.name)).getByRole('button', { name: t('settings.inference.endpoints.remove') }))
    const confirm = /** @type {HTMLElement} */ (
      screen.getByRole('group', { name: t('settings.inference.remove.confirm', { name: MADE.name }) })
    )
    await fireEvent.click(within(confirm).getByRole('checkbox', { name: t('settings.inference.remove.alsoDelete') }))
    expect(within(confirm).getByText(t('settings.inference.remove.deleteNote', { providerKey: MODAL_KEY }))).toBeTruthy()
    await fireEvent.click(within(confirm).getByRole('button', { name: t('settings.inference.remove.review') }))

    const heading = await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.cleanup') })
    await waitFor(() => expect(document.activeElement).toBe(heading))
    await screen.findByText(`mc-weights-${MADE.id}`)
    await fill(t('settings.cloud.setup.connect.modalTokenId'), 'ak-old')
    await fill(t('settings.cloud.setup.connect.modalTokenSecret'), 'as-old')
    await fireEvent.click(screen.getByRole('checkbox', { name: t('settings.cloud.setup.cleanup.approve') }))
    await fireEvent.click(button(t('settings.cloud.setup.cleanup.delete')))

    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.cleaned') })
    await waitFor(() => expect(screen.queryByText(MADE.name, { selector: 'label' })).toBeNull())
    expect(screen.getByText(t('settings.inference.endpoints.empty'))).toBeTruthy()
    await statusSays(t('settings.inference.status.attention', { reasonKey: 'settings.inference.reason.none' }))
    expect((await backend.readInferenceConfig()).modalProfiles[MADE.id]).toBeUndefined()

    // The Remove it was opened from went with the endpoint, so focus goes to
    // the heading of the list it was in.
    await fireEvent.click(button(t('shell.action.close')))
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('heading', { name: t('settings.inference.endpoints.title') })),
    )
  })

  it('puts focus back on Remove when the clean-up is left without deleting anything', async () => {
    const backend = await seeded()
    const credentials = { token_id: 'ak-old', token_secret: 'as-old' }
    const planned = await backend.runCloudProvisioner({
      op: 'plan',
      provider: 'modal',
      params: { credentials, installation_id: MADE.id },
    })
    await backend.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: MADE.id, approved_plan_hash: planned.data.plan_hash },
    })
    show(backend)
    await statusSays(t('settings.inference.status.ready', { name: MADE.name }))

    await fireEvent.click(within(row(MADE.name)).getByRole('button', { name: t('settings.inference.endpoints.remove') }))
    const confirm = /** @type {HTMLElement} */ (
      screen.getByRole('group', { name: t('settings.inference.remove.confirm', { name: MADE.name }) })
    )
    await fireEvent.click(within(confirm).getByRole('checkbox', { name: t('settings.inference.remove.alsoDelete') }))
    await fireEvent.click(within(confirm).getByRole('button', { name: t('settings.inference.remove.review') }))
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.cleanup') })
    // Back waits for the plan of what would be deleted.
    await screen.findByText(`mc-weights-${MADE.id}`)

    await fireEvent.click(button(t('settings.cloud.setup.review.back')))
    await waitFor(() =>
      expect(document.activeElement).toBe(
        within(row(MADE.name)).getByRole('button', { name: t('settings.inference.endpoints.remove') }),
      ),
    )
  })
})

describe('Connect an existing endpoint', () => {
  async function openForm() {
    await fireEvent.click(button(t('settings.inference.connect.title')))
  }

  it('saves the endpoint with its token, makes it the default, and checks it', async () => {
    const backend = await seeded()
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    await openForm()
    await fill(t('settings.inference.connect.name'), 'My Modal')
    await fill(t('settings.inference.connect.endpoint'), 'https://me--gateway.modal.run/mc/v1')
    await fill(t('settings.inference.token.modalTokenId'), 'pt-id')
    await fill(t('settings.inference.token.modalTokenSecret'), 'pt-secret')
    await fireEvent.click(button(t('settings.inference.connect.connect')))

    await screen.findByText(t('settings.inference.connect.connected', { name: 'My Modal' }))
    const config = await backend.readInferenceConfig()
    const [id] = Object.keys(config.modalProfiles)
    expect(id).toMatch(/^ep_[a-z0-9]{6}$/)
    expect(config.modalProfiles[id]).toMatchObject({ name: 'My Modal', endpointUrl: 'https://me--gateway.modal.run/mc/v1' })
    expect(config.selectedTarget).toEqual({ type: 'modal', profile_id: id })
    expect((await backend.getCloudSecretSummary({ provider: 'modal', profileId: id, role: 'runtime' })).present).toBe(true)

    await waitFor(() => expect(within(row('My Modal')).getByText(t('settings.inference.token.saved'))).toBeTruthy())
    expect(within(row('My Modal')).getByText(t('settings.inference.health.reachable'))).toBeTruthy()
    await statusSays(t('settings.inference.status.ready', { name: 'My Modal' }))
    // The token fields are wiped once the keychain has answered.
    for (const label of [t('settings.inference.token.modalTokenId'), t('settings.inference.token.modalTokenSecret')]) {
      expect(/** @type {HTMLInputElement} */ (screen.getByLabelText(label)).value).toBe('')
    }
  })

  it('takes the endpoint back out when its token cannot be saved', async () => {
    const backend = await seeded()
    vi.spyOn(backend, 'storeCloudSecret').mockRejectedValueOnce(new Error('keychain locked'))
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    await openForm()
    const beam = /** @type {HTMLButtonElement} */ (screen.getByRole('radio', { name: t('settings.inference.provider.beamPaused') }))
    expect(beam.disabled || beam.getAttribute('aria-disabled') === 'true').toBe(true)
    await fill(t('settings.inference.connect.name'), 'My Modal')
    await fill(t('settings.inference.connect.endpoint'), 'https://me--gateway.modal.run/mc/v1')
    await fill(t('settings.inference.token.modalTokenId'), 'pt-id')
    await fill(t('settings.inference.token.modalTokenSecret'), 'pt-secret')
    await fireEvent.click(button(t('settings.inference.connect.connect')))

    expect((await screen.findByRole('alert')).textContent).toBe(t('settings.inference.error.tokenSaveFailed'))
    const config = await backend.readInferenceConfig()
    expect(config.modalProfiles).toEqual({})
    expect(config.selectedTarget).toEqual({ type: 'local' })
    expect(/** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.inference.token.modalTokenSecret'))).value).toBe('')
    expect(screen.queryByText('My Modal', { selector: 'label' })).toBeNull()
  })

  it('checks the address before saving anything', async () => {
    const backend = await seeded()
    const write = vi.spyOn(backend, 'writeInferenceConfig')
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    await openForm()
    await fill(t('settings.inference.connect.name'), 'Plain HTTP')
    await fill(t('settings.inference.connect.endpoint'), 'http://me.example.com/mc/v1')
    await fill(t('settings.inference.token.modalTokenId'), 'pt-id')
    await fill(t('settings.inference.token.modalTokenSecret'), 'pt-secret')
    await fireEvent.click(button(t('settings.inference.connect.connect')))
    expect((await screen.findByRole('alert')).textContent).toBe(t('settings.inference.error.invalidUrl'))
    expect(write).not.toHaveBeenCalled()
  })
})

describe('Needs attention', () => {
  it('offers a setup that did not finish for Resume, or forgets it', async () => {
    setup.unfinished = { provider: 'beam', installationId: 'mc-half01', options: {} }
    const backend = await seeded()
    show(backend)
    const text = t('settings.inference.recovery.unfinished', { providerKey: BEAM_KEY, id: 'mc-half01' })
    await screen.findByText(text)

    await fireEvent.click(button(t('settings.inference.recovery.resume')))
    const heading = await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.resume') })
    await waitFor(() => expect(document.activeElement).toBe(heading))
    expect(screen.getByLabelText(t('settings.cloud.setup.connect.beamToken'))).toBeTruthy()
    // One place for it at a time: the reminder waits while the provisioner has it.
    expect(screen.queryByText(text)).toBeNull()

    await fireEvent.click(button(t('shell.action.close')))
    await screen.findByText(text)
    // Back on the control it was opened from, which is drawn again with it.
    await waitFor(() => expect(document.activeElement).toBe(button(t('settings.inference.recovery.resume'))))
    await fireEvent.click(button(t('settings.inference.recovery.forget')))
    await waitFor(() => expect(screen.queryByText(text)).toBeNull())
    expect(setup.unfinished).toBeNull()
    expect(attentionHeading()).toBeNull()
  })

  it('puts focus back on Get a new token when its provisioner is closed and the token is still missing', async () => {
    const backend = await seeded({ endpoints: [{ ...MADE, token: false }], selected: { type: 'modal', profile_id: MADE.id } })
    show(backend)
    await statusSays(t('settings.inference.status.attention', { reasonKey: 'settings.inference.reason.noSecret' }))

    await fireEvent.click(button(t('settings.inference.recovery.newToken')))
    const heading = await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.resume') })
    await waitFor(() => expect(document.activeElement).toBe(heading))

    await fireEvent.click(button(t('shell.action.close')))
    await waitFor(() => expect(document.activeElement).toBe(button(t('settings.inference.recovery.newToken'))))
  })

  it('can rerun recovery even when no startup notice remains', async () => {
    const backend = await seeded()
    const reconcile = vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ attached: [], stillRunning: [], needsAttention: [] })
    show(backend)
    await fireEvent.click(await screen.findByRole('button', { name: t('settings.inference.logs.title', { count: 0 }) }))
    await fireEvent.click(await screen.findByRole('button', { name: t('cloud.recovery.check') }))
    await waitFor(() => expect(reconcile).toHaveBeenCalledWith({ apply: true }))
  })

  it('requires an explicit duplicate-render confirmation before abandoning an attempt', async () => {
    const backend = await seeded()
    const reconcile = vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ decision: 'abandon' })
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    cloud.recovery = { attached: [], stillRunning: [], needsAttention: [
      { attemptId: 'stuck', chapterId: 'chapter', pageIndex: 0, regionId: 'region', reason: 'ambiguous' },
    ] }
    await fireEvent.click(await screen.findByRole('button', { name: t('settings.inference.logs.title', { count: 1 }) }))
    await fireEvent.click(await screen.findByRole('button', { name: t('cloud.recovery.abandon') }))
    expect(screen.getByRole('alert').textContent).toContain(t('cloud.recovery.duplicateRisk'))
    expect(reconcile).not.toHaveBeenCalledWith(expect.objectContaining({ action: 'abandon' }))
    await fireEvent.click(screen.getByRole('button', { name: t('cloud.recovery.confirmAbandon') }))
    await waitFor(() => expect(reconcile).toHaveBeenCalledWith({ action: 'abandon', attemptId: 'stuck', acceptDuplicateRisk: true }))
    await waitFor(() => expect(cloud.recovery.needsAttention).toEqual([]))
  })

  it('keeps interrupted renders behind Cloud logs, out of Needs attention', async () => {
    const backend = await seeded()
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    cloud.recovery = { attached: [], stillRunning: [], needsAttention: [
      { attemptId: `att-${'2'.repeat(24)}`, chapterId: 'chapter', pageIndex: 5, regionId: 'region', reason: 'repair_needed' },
    ] }
    const line = t('settings.inference.recovery.attempt', { page: 6, reasonKey: 'cloud.recovery.repairNeeded' })
    const logs = await screen.findByRole('button', { name: t('settings.inference.logs.title', { count: 1 }) })
    expect(attentionHeading()).toBeNull()
    expect(screen.queryByText(line)).toBeNull()
    await fireEvent.click(logs)
    expect(await screen.findByText(line)).toBeTruthy()
  })

  it('lists the renders the last session could not settle until they are dismissed', async () => {
    const backend = await seeded()
    vi.spyOn(backend, 'reconcileCloudRecovery').mockResolvedValue({ decision: 'acknowledge' })
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    const first = `att-${'0'.repeat(24)}`
    cloud.recovery = {
      attached: [],
      stillRunning: [],
      needsAttention: [
        { attemptId: first, chapterId: 'chapter-1', pageIndex: 2, regionId: 'region-1', reason: 'ambiguous' },
        { attemptId: `att-${'1'.repeat(24)}`, chapterId: null, pageIndex: null, regionId: null, reason: 'failed' },
      ],
    }
    const ambiguous = t('settings.inference.recovery.attempt', {
      page: 3,
      reasonKey: 'settings.inference.recovery.reason.ambiguous',
    })
    const failed = t('settings.inference.recovery.attemptNoPage', { reasonKey: 'settings.inference.recovery.reason.failed' })
    await fireEvent.click(await screen.findByRole('button', { name: t('settings.inference.logs.title', { count: 2 }) }))
    await screen.findByText(ambiguous)
    expect(screen.getByText(failed)).toBeTruthy()

    const dismiss = screen.getAllByRole('button', { name: t('settings.inference.recovery.dismiss') })
    await fireEvent.click(dismiss[0])
    await waitFor(() => expect(screen.queryByText(ambiguous)).toBeNull())
    expect(screen.getByText(failed)).toBeTruthy()
    expect(cloud.recovery?.needsAttention.map((entry) => entry.attemptId)).not.toContain(first)
  })
})

describe('Set up with Modal or Beam', () => {
  it('opens in place, and closing it puts focus back on the button', async () => {
    const backend = await seeded()
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    await fireEvent.click(button(t('settings.inference.setup.action')))
    const heading = await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.connect') })
    await waitFor(() => expect(document.activeElement).toBe(heading))

    await fireEvent.click(button(t('shell.action.cancel')))
    await waitFor(() => expect(document.activeElement).toBe(button(t('settings.inference.setup.action'))))
  })

  it('lists the new endpoint as the default once setup has saved it', async () => {
    const backend = await seeded()
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    await fireEvent.click(button(t('settings.inference.setup.action')))
    await fill(t('settings.cloud.setup.connect.modalTokenId'), 'ak-new')
    await fill(t('settings.cloud.setup.connect.modalTokenSecret'), 'as-new')
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.review') })
    await fireEvent.click(
      screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.approve', { providerKey: MODAL_KEY }) }),
    )
    await fireEvent.click(button(t('settings.cloud.setup.review.start')))
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.done') }, { timeout: 10_000 })

    const label = await screen.findByText(/^Modal \(mc-[a-z0-9]{6}\)$/, { selector: 'label' })
    const name = label.textContent?.trim() ?? ''
    expect(within(row(name)).getByText(t('settings.inference.endpoints.default'))).toBeTruthy()
    await statusSays(t('settings.inference.status.ready', { name }))
  }, 15_000)

  it('turns the cloud on when a setup ends with a healthy endpoint', async () => {
    const backend = await seeded({ allowed: false })
    show(backend)
    await screen.findByText(t('settings.inference.endpoints.empty'))
    await fireEvent.click(button(t('settings.inference.setup.action')))
    await fill(t('settings.cloud.setup.connect.modalTokenId'), 'ak-new')
    await fill(t('settings.cloud.setup.connect.modalTokenSecret'), 'as-new')
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.review') })
    await fireEvent.click(
      screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.approve', { providerKey: MODAL_KEY }) }),
    )
    expect(session.cloudAllowed).toBe(false)
    await fireEvent.click(button(t('settings.cloud.setup.review.start')))
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.done') }, { timeout: 10_000 })

    await waitFor(() => expect(session.cloudAllowed).toBe(true))
    expect((await backend.readSettings()).cloudEngines).toBe('allowed')
    const label = await screen.findByText(/^Modal \(mc-[a-z0-9]{6}\)$/, { selector: 'label' })
    await statusSays(t('settings.inference.status.ready', { name: label.textContent?.trim() ?? '' }))
  }, 15_000)

  it('picks the checklist back up when Settings opens while a setup runs', async () => {
    const backend = await seeded()
    setup.run = {
      id: 9_001,
      op: 'apply',
      provider: 'modal',
      installationId: 'mc-run001',
      status: 'running',
      errorCode: null,
      steps: [{ id: 'image', state: 'running', startedAt: Date.now(), endedAt: null, pct: null }],
      startedAt: Date.now(),
      endedAt: null,
      data: null,
    }
    show(backend)
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.running') })
    const steps = screen.getByRole('list', { name: t('settings.cloud.setup.heading.running') })
    expect(within(steps).getByText(t('settings.cloud.setup.step.image'))).toBeTruthy()
    expect(button(t('settings.cloud.setup.running.stop'))).toBeTruthy()
  })
})
