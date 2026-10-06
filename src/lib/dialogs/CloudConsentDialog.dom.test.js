/**
 * The one question before a cloud render, through the real modal host and the
 * mock backend: what it says, that it says it covers the project, and that
 * nothing is granted or sent until Confirm.
 */
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { requestCloudConsent } from '../editor/cloudflow.svelte.js'
import { t } from '../i18n/index.js'
import ModalHost from '../shell/ModalHost.svelte'
import { app, clearNotices, closeAllModals } from '../state/app.svelte.js'
import { stopCloud } from '../state/cloud.svelte.js'
import { setCloudAllowed } from '../state/session.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const PROFILE = {
  id: 'mc-abc123',
  name: 'Studio GPU',
  endpointUrl: 'https://ws--mc-abc123-gateway.modal.run/mc/v1',
}
const REQUEST = {
  regionId: 'region-1',
  chapterId: 'chapter-1',
  pageIndex: 0,
  intent: { action: 'applyTool', tool: 'contentAwareFill', params: { engine: 'cloud' } },
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
  stopCloud()
  setCloudAllowed(false)
  clearNotices()
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/**
 * A mock backend with one Modal endpoint as the default.
 *
 * @param {{allowed?: boolean, token?: boolean}} [options]
 */
async function seeded({ allowed = true, token = true } = {}) {
  const backend = createMockBackend({ timing: ZERO })
  await backend.writeSettings({ cloudEngines: allowed ? 'allowed' : 'blocked' })
  setCloudAllowed(allowed)
  await backend.writeInferenceConfig({
    config: {
      schemaVersion: 1,
      selectedTarget: { type: 'modal', profile_id: PROFILE.id },
      beamProfiles: {},
      modalProfiles: {
        [PROFILE.id]: {
          ...PROFILE,
          canonicalOrigin: new URL(PROFILE.endpointUrl).origin,
          canonicalOriginFingerprint: '',
          createdAtMs: 1,
          updatedAtMs: 1,
        },
      },
    },
  })
  if (token) {
    await backend.storeCloudSecret({
      provider: 'modal',
      profileId: PROFILE.id,
      role: 'runtime',
      secret: 'rt-secret',
      tokenId: 'rt-id',
    })
  }
  setBackend(backend)
  return backend
}

const consentDialog = () => screen.findByRole('dialog', { name: t('modal.title.cloudConsent') })

describe('the cloud consent dialog', () => {
  it('says what is sent, where and what it costs, and grants only after Confirm', async () => {
    const backend = await seeded()
    const confirm = vi.spyOn(backend, 'confirmCloudConsent')
    const submit = vi.spyOn(backend, 'submitCloudAttempt')
    render(ModalHost)

    const pending = requestCloudConsent(REQUEST, backend)
    const dialog = await consentDialog()
    expect(app.modals).toHaveLength(1)
    expect(within(dialog).getByText(t('modal.cloudConsent.whatValue', { width: 256, height: 256 }))).toBeTruthy()
    expect(
      within(dialog).getByText(
        t('modal.cloudConsent.whereValue', { name: PROFILE.name, providerKey: 'settings.inference.provider.modal' }),
      ),
    ).toBeTruthy()
    expect(within(dialog).getByText('ws--mc-abc123-gateway.modal.run')).toBeTruthy()
    expect(within(dialog).getByText(t('modal.cloudConsent.costUnknown'))).toBeTruthy()
    expect(within(dialog).getByText(t('cloud.projectConsent'))).toBeTruthy()
    expect(confirm).not.toHaveBeenCalled()

    // Confirm waits for both statements; neither is checked for the user.
    const send = within(dialog).getByRole('button', { name: t('shell.action.confirmSpend') })
    const rights = within(dialog).getByRole('checkbox', { name: t('cloud.analysis.rights') })
    const retention = within(dialog).getByRole('checkbox', { name: t('cloud.analysis.retention') })
    expect(/** @type {HTMLInputElement} */ (rights).checked).toBe(false)
    expect(/** @type {HTMLInputElement} */ (retention).checked).toBe(false)
    expect(/** @type {HTMLButtonElement} */ (send).disabled).toBe(true)
    await fireEvent.click(send)
    await fireEvent.click(rights)
    expect(/** @type {HTMLButtonElement} */ (send).disabled).toBe(true)
    await fireEvent.click(send)
    expect(app.modals).toHaveLength(1)
    await fireEvent.click(retention)
    expect(/** @type {HTMLButtonElement} */ (send).disabled).toBe(false)

    await fireEvent.click(send)
    const grant = await pending
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(confirm.mock.calls[0][0]).toMatchObject({ rightsAttested: true, retentionAcknowledged: true })
    expect(grant).toMatchObject({
      params: {
        grantNonce: expect.stringMatching(/^grant-/),
        executionTarget: { type: 'modal', profile_id: PROFILE.id },
        recipe: { recipe_id: expect.any(String), model_id: expect.any(String) },
        intent: REQUEST.intent,
      },
      attemptId: expect.stringMatching(/^att-[0-9a-f]{24}$/),
    })
    // One question per action: nothing else is asked after the answer, and
    // consent sends nothing by itself.
    expect(app.modals).toHaveLength(0)
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(submit).not.toHaveBeenCalled()
  })

  it('names the endpoint the proposal sends to, and says provider and host when it has no saved name', async () => {
    const backend = await seeded()
    const prepare = backend.prepareCloudConsent
    vi.spyOn(backend, 'prepareCloudConsent').mockImplementation(async (spec) => ({
      ...(await prepare(spec)),
      profileId: 'mc-other9',
      endpointUrl: 'https://other--mc-other9-gateway.modal.run/mc/v1',
    }))
    render(ModalHost)
    const pending = requestCloudConsent(REQUEST, backend)
    const dialog = await consentDialog()
    expect(
      within(dialog).getByText(t('modal.cloudConsent.whereUnnamed', { providerKey: 'settings.inference.provider.modal' })),
    ).toBeTruthy()
    expect(within(dialog).getByText('other--mc-other9-gateway.modal.run')).toBeTruthy()
    expect(within(dialog).queryByText(PROFILE.name, { exact: false })).toBeNull()
    await fireEvent.click(within(dialog).getByRole('button', { name: t('shell.action.cancel') }))
    expect(await pending).toBeNull()
  })

  it('shows the estimate when the proposal has one', async () => {
    const backend = await seeded()
    const prepare = backend.prepareCloudConsent
    vi.spyOn(backend, 'prepareCloudConsent').mockImplementation(async (spec) => ({
      ...(await prepare(spec)),
      estimatedCostUsd: 0.12,
    }))
    render(ModalHost)
    const pending = requestCloudConsent(REQUEST, backend)
    const dialog = await consentDialog()
    expect(within(dialog).getByText(t('modal.cloudConsent.costEstimate', { cost: 0.12 }))).toBeTruthy()
    await fireEvent.click(within(dialog).getByRole('button', { name: t('shell.action.cancel') }))
    expect(await pending).toBeNull()
  })

  it('Cancel and Escape grant nothing', async () => {
    const backend = await seeded()
    const confirm = vi.spyOn(backend, 'confirmCloudConsent')
    render(ModalHost)

    const cancelled = requestCloudConsent(REQUEST, backend)
    await fireEvent.click(within(await consentDialog()).getByRole('button', { name: t('shell.action.cancel') }))
    expect(await cancelled).toBeNull()

    const escaped = requestCloudConsent(REQUEST, backend)
    await consentDialog()
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(await escaped).toBeNull()

    expect(confirm).not.toHaveBeenCalled()
    expect(app.modals).toHaveLength(0)
    expect(app.notices).toHaveLength(0)
  })

  it('asks nothing when no endpoint is ready, and says why', async () => {
    const backend = await seeded({ token: false })
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    render(ModalHost)
    expect(await requestCloudConsent(REQUEST, backend)).toBeNull()
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.cloud.notReady'])
    expect(app.modals).toHaveLength(0)
    expect(prepare).not.toHaveBeenCalled()
  })

  it('asks nothing while cloud is off', async () => {
    const backend = await seeded({ allowed: false })
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    render(ModalHost)
    expect(await requestCloudConsent(REQUEST, backend)).toBeNull()
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.cloud.blocked'])
    expect(app.modals).toHaveLength(0)
    expect(prepare).not.toHaveBeenCalled()
  })

  it('says a region is too large for the cloud GPU when the native side refuses its crop', async () => {
    const backend = await seeded()
    vi.spyOn(backend, 'prepareCloudConsent').mockRejectedValueOnce(
      new Error("cloud_consent_crop_too_large: the region's crop is past the render service limits"))
    render(ModalHost)
    expect(await requestCloudConsent(REQUEST, backend)).toBeNull()
    expect(app.notices.map((notice) => notice.key)).toEqual(['cloud.clean.regionTooLarge'])
    expect(app.modals).toHaveLength(0)
  })

  it('says the request could not be prepared when the native side refuses it', async () => {
    const backend = await seeded()
    vi.spyOn(backend, 'prepareCloudConsent').mockRejectedValueOnce(new Error('config_unreadable'))
    render(ModalHost)
    expect(await requestCloudConsent(REQUEST, backend)).toBeNull()
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.cloud.consentFailed'])
    expect(app.modals).toHaveLength(0)
  })
})
