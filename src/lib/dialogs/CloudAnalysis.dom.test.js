import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { stopCloud } from '../state/cloud.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { setCloudAllowed } from '../state/session.svelte.js'
import CloudAnalysis from './CloudAnalysis.svelte'
import Modal from '../ui/Modal.svelte'

afterEach(() => {
  cleanup()
  setBackend(null)
  stopCloud()
  setCloudAllowed(false)
  editor.project = null
  vi.clearAllMocks()
})

const SAM = 'text_mask_sam_ts@1'
const RT = 'text_regions_rt@1'

function proposalFor(capability, overrides = {}) {
  const issuedAtMs = Date.UTC(2026, 8, 25, 14, 0)
  return { proposalId: 'prop-1', chapterId: 'c1', pageIndex: 0, provider: 'modal', profileId: 'p1',
    profileName: 'Studio A100', capability, graphSha256s: ['a'.repeat(64), 'b'.repeat(64)], modelRevision: 'c'.repeat(40),
    sourcePageSha256: 'd'.repeat(64), underlaySha256: 'e'.repeat(64), predecessorsSha256: 'f'.repeat(64),
    projectRevisionSha256: '0'.repeat(64), pageWidth: 1600, pageHeight: 2400, pages: 1,
    totalTilePixels: 3840000, totalEncodedBytes: 2621440, includesSurroundingArt: true, costEstimateUsd: null,
    issuedAtMs, expiresAtMs: issuedAtMs + 300000,
    tiles: [{ rect: { x: 0, y: 0, width: 1024, height: 1024 } }, { rect: { x: 1024, y: 0, width: 576, height: 1024 } }],
    ...overrides }
}

function record(phase, extra = {}) {
  return { schema_version: 2, proposal_id: 'prop-1', provider: 'modal', profile_id: 'p1', capability: SAM,
    source_sha256: 'd'.repeat(64), underlay_sha256: 'e'.repeat(64), total_tiles: 2, completed_tiles: 0,
    reported_cost_usd: null, phase, ...extra }
}

/**
 * A backend with a Modal endpoint selected and its key stored. `emit` plays
 * a `cloud://analysis` journal record to the subscribed review.
 */
function cloud({ settings = { cloudEngines: 'allowed' }, target = { type: 'modal', profile_id: 'p1' }, secret = true,
  offered = [SAM, RT], proposal = {}, confirm, status, host, extraProps = {} } = {}) {
  const listeners = new Set()
  const api = {
    readSettings: vi.fn(async () => settings),
    readInferenceConfig: async () => ({ selectedTarget: target, beamProfiles: {},
      modalProfiles: { p1: { name: 'Studio A100', endpointUrl: 'https://studio.modal.run' } } }),
    getCloudSecretSummary: async () => ({ present: secret }),
    onRemoteAnalysis: vi.fn(async (handler) => { listeners.add(handler); return () => listeners.delete(handler) }),
    listRemoteAnalysisCapabilities: vi.fn(async () => ({ protocol_version: '1.0.0',
      capabilities: offered.map((capability) => ({ capability, graph_sha256s: ['a'.repeat(64)], model_revision: 'c'.repeat(40) })) })),
    proposeRemoteAnalysis: vi.fn(async ({ capability }) => proposalFor(capability, proposal)),
    confirmRemoteAnalysis: vi.fn(confirm ?? (async () => ({ analysisId: 'remote:modal:x' }))),
    cancelRemoteAnalysis: vi.fn(async () => true),
    getRemoteAnalysisStatus: vi.fn(status ?? (async () => { throw 'analysis_proposal_missing' })),
  }
  setBackend(/** @type {any} */ (api))
  // The session's switch and the shared readiness agree, as they do in the app.
  setCloudAllowed(settings.cloudEngines === 'allowed')
  const onresult = vi.fn()
  const onactive = vi.fn()
  const screen = render(CloudAnalysis, { ...(host ? { target: host } : {}),
    props: { chapterId: 'c1', pageIndex: 0, pageNumber: 1, onresult, onactive, ...extraProps } })
  return { api, screen, onresult, onactive, emit: (entry) => listeners.forEach((handler) => handler(entry)) }
}

async function openConsent(screen) {
  await fireEvent.click(await screen.findByRole('button', { name: 'Analyze with cloud GPU' }))
  return screen.findByRole('region', { name: 'Send page 1 to your cloud GPU?' })
}

async function consentAndSend(screen) {
  await openConsent(screen)
  await fireEvent.click(screen.getByLabelText(/I have the rights/))
  await fireEvent.click(screen.getByLabelText(/I have reviewed and accept/))
  await fireEvent.click(screen.getByRole('button', { name: 'Send to cloud GPU' }))
}

it('shows a disabled strip, with the reason and the way to Settings, while the cloud GPU cannot be used', async () => {
  const off = cloud({ settings: { cloudEngines: 'off' } })
  expect(await off.screen.findByText(/Cloud engines are off\. Turn them on in Settings > Cloud/)).toBeTruthy()
  expect(off.screen.getByRole('button', { name: 'Open Cloud settings' })).toBeTruthy()
  expect(off.screen.queryByRole('button', { name: 'Analyze with cloud GPU' })).toBeNull()
  expect(off.api.listRemoteAnalysisCapabilities).not.toHaveBeenCalled()
  cleanup()
  stopCloud()

  const noTarget = cloud({ target: { type: 'local' } })
  expect(await noTarget.screen.findByText('To analyze on a cloud GPU, choose a Modal or Beam endpoint in Settings.')).toBeTruthy()
  expect(noTarget.screen.queryByRole('button', { name: 'Analyze with cloud GPU' })).toBeNull()
  cleanup()
  stopCloud()

  const noSecret = cloud({ secret: false })
  expect(await noSecret.screen.findByText(/The selected cloud endpoint has no stored key/)).toBeTruthy()
  expect(noSecret.screen.getByRole('button', { name: 'Open Cloud settings' })).toBeTruthy()
})

it('refuses a long strip before asking the endpoint anything', async () => {
  editor.project = /** @type {any} */ ({ id: 'p', mode: 'longstrip' })
  const { api, screen } = cloud()
  expect(await screen.findByText(/Cloud analysis needs a paginated chapter/)).toBeTruthy()
  expect(screen.queryByRole('button', { name: 'Analyze with cloud GPU' })).toBeNull()
  expect(api.listRemoteAnalysisCapabilities).not.toHaveBeenCalled()
})

it('proposes the owner\'s capability with its companion, and names both in the consent', async () => {
  const both = { models: [
    { capability: SAM, graphSha256s: ['a'.repeat(64)], modelRevision: 'c'.repeat(40) },
    { capability: RT, graphSha256s: ['b'.repeat(64)], modelRevision: 'd'.repeat(40) },
  ] }
  const { api, screen } = cloud({ proposal: both, extraProps: { request: { capability: SAM, companion: RT } } })
  expect(await screen.findByText(/Analyze sends this page to Studio A100/)).toBeTruthy()
  expect(screen.queryByRole('button', { name: 'Analyze with cloud GPU' })).toBeNull()
  expect(screen.getByText('☁ SAM-TS-L lettering mask with Ogkalu comic text & bubble detector (Full)')).toBeTruthy()
  await screen.component.begin()
  const consent = await screen.findByRole('region', { name: 'Send page 1 to your cloud GPU?' })
  expect(api.proposeRemoteAnalysis).toHaveBeenCalledWith({ chapterId: 'c1', pageIndex: 0, provider: 'modal',
    profileId: 'p1', capability: SAM, companion: RT })
  expect(consent.textContent).toContain(`☁ SAM-TS-L lettering mask, revision ${'c'.repeat(12)}…`)
  expect(consent.textContent).toContain(`☁ Ogkalu comic text & bubble detector (Full), revision ${'d'.repeat(12)}…`)
  expect(consent.textContent).toContain('Every tile goes to each model listed.')
})

it('refuses a companion the endpoint does not offer, before proposing', async () => {
  const { api, screen } = cloud({ offered: [SAM], extraProps: { request: { capability: SAM, companion: RT } } })
  await screen.findByText(/Analyze sends this page to Studio A100/)
  await screen.component.begin()
  await waitFor(() => expect(api.listRemoteAnalysisCapabilities).toHaveBeenCalled())
  expect(api.proposeRemoteAnalysis).not.toHaveBeenCalled()
})

it('shows exactly what would be sent and sends nothing until both statements are checked', async () => {
  const { api, screen } = cloud()
  expect(await screen.findByText('Studio A100 on Modal')).toBeTruthy()
  const consent = await openConsent(screen)
  expect(api.listRemoteAnalysisCapabilities).toHaveBeenCalledWith({ provider: 'modal', profileId: 'p1' })
  expect(api.proposeRemoteAnalysis).toHaveBeenCalledWith({ chapterId: 'c1', pageIndex: 0, provider: 'modal',
    profileId: 'p1', capability: SAM })
  expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Send page 1 to your cloud GPU?' }))

  const text = consent.textContent
  expect(text).toContain('1 page as 2 tiles: 3,840,000 pixels, 2,621,440 bytes encoded (2.5 MB).')
  expect(text).toContain('The surrounding art is included, not only the lettering.')
  expect(text).toContain('Studio A100, your endpoint on Modal')
  expect(text).toContain(`☁ SAM-TS-L lettering mask, revision ${'c'.repeat(12)}…`)
  expect(text).toContain(`Graph SHA-256 ${'a'.repeat(12)}…, ${'b'.repeat(12)}…`)
  expect(text).toContain('a'.repeat(64))
  expect(text).toContain('Estimated cost unknown')
  expect(text).toContain('Cloud results are for review. They cannot prepare a component write.')

  const rights = screen.getByLabelText(/I have the rights to send these page pixels/)
  const retention = screen.getByLabelText(/retention, human review, and training terms/)
  const send = screen.getByRole('button', { name: 'Send to cloud GPU' })
  expect(rights.checked).toBe(false)
  expect(retention.checked).toBe(false)
  expect(send.disabled).toBe(true)
  await fireEvent.click(rights)
  expect(send.disabled).toBe(true)
  await fireEvent.click(retention)
  expect(send.disabled).toBe(false)
  await fireEvent.click(rights)
  expect(send.disabled).toBe(true)
  expect(api.confirmRemoteAnalysis).not.toHaveBeenCalled()

  // Cancel discards the proposal: the native side is told, and nothing is confirmed.
  await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  await waitFor(() => expect(api.cancelRemoteAnalysis).toHaveBeenCalledWith({ proposalId: 'prop-1' }))
  expect(screen.queryByRole('region', { name: /Send page 1/ })).toBeNull()
  expect(api.confirmRemoteAnalysis).not.toHaveBeenCalled()
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Analyze with cloud GPU' })))
})

it('shows a cost the proposal carries as an estimate', async () => {
  const { screen } = cloud({ proposal: { costEstimateUsd: 0.042 } })
  const consent = await openConsent(screen)
  expect(consent.textContent).toContain('Estimated $0.042')
  expect(consent.textContent).not.toContain('Estimated cost unknown')
})

it('shows a negative cost as unknown, never as a dollar amount', async () => {
  const { screen } = cloud({ proposal: { costEstimateUsd: -1 } })
  const consent = await openConsent(screen)
  expect(consent.querySelector('[data-cost]')?.getAttribute('data-cost')).toBe('unknown')
  expect(consent.textContent).toContain('Estimated cost unknown')
  expect(consent.textContent).not.toContain('$')
})

// The review is a Modal, which closes on an Escape that reaches `window`.
// Inside the consent step Escape backs out of the consent only, wherever
// focus sits in the page, so the local analysis behind it survives.
it('backs out of the consent on Escape without closing the review', async () => {
  const modal = vi.fn()
  window.addEventListener('keydown', modal)
  try {
    const { api, screen } = cloud()
    const consent = await openConsent(screen)
    await fireEvent.keyDown(within(consent).getByRole('heading'), { key: 'Escape' })
    await waitFor(() => expect(api.cancelRemoteAnalysis).toHaveBeenCalledWith({ proposalId: 'prop-1' }))
    expect(screen.queryByRole('region', { name: /Send page 1/ })).toBeNull()
    expect(modal).not.toHaveBeenCalled()
    const start = () => screen.getByRole('button', { name: 'Analyze with cloud GPU' })
    await waitFor(() => expect(document.activeElement).toBe(start()))

    // Focus on nothing (a press on plain text in WebKit leaves it there) is still the consent step.
    await openConsent(screen)
    const held = /** @type {HTMLElement} */ (document.activeElement)
    held.blur()
    await fireEvent.keyDown(document.body, { key: 'Escape' })
    await waitFor(() => expect(api.cancelRemoteAnalysis).toHaveBeenCalledTimes(2))
    expect(modal).not.toHaveBeenCalled()
    await waitFor(() => expect(document.activeElement).toBe(start()))

    // With the consent gone, Escape is the review's again.
    await fireEvent.keyDown(start(), { key: 'Escape' })
    expect(modal).toHaveBeenCalledTimes(1)
  } finally {
    window.removeEventListener('keydown', modal)
  }
})

// Escape belongs to the dialog it was pressed in. A dialog shown over the
// review answers its own, and with focus on nothing it is the topmost
// dialog's, so the consent under it stays open.
it('leaves Escape in another dialog to that dialog', async () => {
  const { api, screen } = cloud()
  await openConsent(screen)
  const onclose = vi.fn()
  const other = render(Modal, { props: { title: 'Another dialog', onclose } })
  const dialog = other.getByRole('dialog', { name: 'Another dialog' })
  await waitFor(() => expect(document.activeElement).toBe(dialog))
  await fireEvent.keyDown(dialog, { key: 'Escape' })
  expect(onclose).toHaveBeenCalledTimes(1)
  expect(api.cancelRemoteAnalysis).not.toHaveBeenCalled()
  expect(screen.getByRole('region', { name: /Send page 1/ })).toBeTruthy()

  dialog.blur()
  await fireEvent.keyDown(document.body, { key: 'Escape' })
  expect(onclose).toHaveBeenCalledTimes(2)
  expect(api.cancelRemoteAnalysis).not.toHaveBeenCalled()
  expect(screen.getByRole('region', { name: /Send page 1/ })).toBeTruthy()
})

it('backs out of the consent from anywhere in the review that holds it', async () => {
  const modal = vi.fn()
  window.addEventListener('keydown', modal)
  const review = document.createElement('div')
  review.setAttribute('role', 'dialog')
  review.setAttribute('aria-modal', 'true')
  const elsewhere = document.createElement('button')
  elsewhere.textContent = 'Page 1'
  review.append(elsewhere)
  document.body.append(review)
  try {
    const { api, screen } = cloud({ host: review })
    await openConsent(screen)
    await fireEvent.keyDown(elsewhere, { key: 'Escape' })
    await waitFor(() => expect(api.cancelRemoteAnalysis).toHaveBeenCalledTimes(1))
    expect(modal).not.toHaveBeenCalled()

    // Focus on nothing, and the review is the topmost dialog.
    await openConsent(screen)
    const held = /** @type {HTMLElement} */ (document.activeElement)
    held.blur()
    await fireEvent.keyDown(document.body, { key: 'Escape' })
    await waitFor(() => expect(api.cancelRemoteAnalysis).toHaveBeenCalledTimes(2))
    expect(modal).not.toHaveBeenCalled()
  } finally {
    window.removeEventListener('keydown', modal)
    cleanup()
    review.remove()
  }
})

it('leaves other keys in the consent step to the review', async () => {
  const modal = vi.fn()
  window.addEventListener('keydown', modal)
  try {
    const { api, screen } = cloud()
    const consent = await openConsent(screen)
    await fireEvent.keyDown(within(consent).getByRole('heading'), { key: 'Tab' })
    expect(modal).toHaveBeenCalledTimes(1)
    expect(api.cancelRemoteAnalysis).not.toHaveBeenCalled()
  } finally {
    window.removeEventListener('keydown', modal)
  }
})

it('names a model the endpoint does not offer and sends nothing', async () => {
  const { api, screen } = cloud({ offered: [RT] })
  await fireEvent.click(await screen.findByRole('button', { name: 'Analyze with cloud GPU' }))
  expect(await screen.findByText('Model not offered')).toBeTruthy()
  expect(screen.getByText('This cloud GPU does not offer the selected analysis model. Nothing was sent.')).toBeTruthy()
  expect(api.proposeRemoteAnalysis).not.toHaveBeenCalled()
  expect(api.confirmRemoteAnalysis).not.toHaveBeenCalled()
  expect(screen.getByRole('option', { name: '☁ SAM-TS-L lettering mask, not offered' })).toBeTruthy()

  // The offered model still proposes.
  await fireEvent.change(screen.getByLabelText('Cloud model'), { target: { value: RT } })
  await openConsent(screen)
  expect(api.proposeRemoteAnalysis).toHaveBeenCalledWith(expect.objectContaining({ capability: RT }))
})

it('never offers the SFX finder, whatever the endpoint advertises', async () => {
  const { screen } = cloud({ offered: [SAM, RT, 'sfx_coo@1'] })
  await screen.findByRole('button', { name: 'Analyze with cloud GPU' })
  const options = [...screen.getByLabelText('Cloud model').querySelectorAll('option')].map((option) => option.value)
  expect(options).toEqual([SAM, RT])
})

it('counts tiles, cancels on request, and names the outcome without retrying', async () => {
  let fail
  const { api, screen, emit, onactive } = cloud({
    confirm: () => new Promise((_resolve, reject) => { fail = reject }),
    status: async () => record({ phase: 'cancelled' }, { completed_tiles: 1, cancel_requested: true }),
  })
  await consentAndSend(screen)
  expect(api.confirmRemoteAnalysis).toHaveBeenCalledWith({ proposalId: 'prop-1', rightsAttested: true, retentionAcknowledged: true })
  expect(await screen.findByText('0 of 2 tiles analyzed on Studio A100')).toBeTruthy()
  expect(onactive).toHaveBeenLastCalledWith(true)

  emit(record({ phase: 'submitted_tile', index: 0 }))
  emit(record({ phase: 'result_cached_tile', index: 0 }, { completed_tiles: 1 }))
  // A record for another proposal is not this run's progress.
  emit({ ...record({ phase: 'result_cached_tile', index: 1 }, { completed_tiles: 2 }), proposal_id: 'other' })
  expect(await screen.findByText('1 of 2 tiles analyzed on Studio A100')).toBeTruthy()
  const bar = screen.getByRole('progressbar', { name: 'Cloud analysis progress' })
  expect(bar.getAttribute('aria-valuenow')).toBe('1')
  expect(bar.getAttribute('aria-valuemax')).toBe('2')

  const cancel = screen.getByRole('button', { name: 'Cancel cloud analysis' })
  expect(document.activeElement).toBe(cancel)
  await fireEvent.click(cancel)
  expect(api.cancelRemoteAnalysis).toHaveBeenCalledWith({ proposalId: 'prop-1' })
  expect(await screen.findByText(/Cancelling. A tile already sent may still be billed/)).toBeTruthy()

  fail('analysis_cancelled')
  expect(await screen.findByText('Cloud analysis cancelled')).toBeTruthy()
  expect(screen.getByText('Cloud analysis cancelled. Remaining tiles were not sent.')).toBeTruthy()
  expect(screen.queryByRole('progressbar')).toBeNull()
  expect(onactive).toHaveBeenLastCalledWith(false)
  expect(api.confirmRemoteAnalysis).toHaveBeenCalledTimes(1)
  expect(api.proposeRemoteAnalysis).toHaveBeenCalledTimes(1)
})

it('reports a page that changed during the run and does not send it again', async () => {
  const { api, screen } = cloud({
    confirm: async () => { throw 'analysis_stale: source or underlay changed during batch' },
    status: async () => record({ phase: 'failed', code: 'analysis_stale' }),
  })
  await consentAndSend(screen)
  expect(await screen.findByText('Page changed during analysis')).toBeTruthy()
  expect(screen.getByText('The page or its visible edits changed. No further tiles were sent.')).toBeTruthy()
  expect(api.confirmRemoteAnalysis).toHaveBeenCalledTimes(1)
})

it('says a tile may have run when its state is unknown, and that cancel was asked for when it was', async () => {
  const unknown = cloud({
    confirm: async () => { throw 'transport error: connection reset while the tile was in flight' },
    status: async () => record({ phase: 'unknown_remote_state', index: 1 }, { completed_tiles: 1 }),
  })
  await consentAndSend(unknown.screen)
  expect(await unknown.screen.findByText('Last tile state unknown')).toBeTruthy()
  expect(unknown.screen.getByText(/The last tile may have run. It will not be sent again automatically/)).toBeTruthy()
  expect(unknown.screen.getByText('transport error: connection reset while the tile was in flight').closest('details')).toBeTruthy()
  cleanup()

  // After a restart the journal says `unknown`, with the user's cancel recorded.
  const requested = cloud({
    confirm: async () => { throw 'transport error: connection reset while the tile was in flight' },
    status: async () => record({ phase: 'unknown', index: 1 }, { completed_tiles: 1, cancel_requested: true }),
  })
  await consentAndSend(requested.screen)
  expect(await requested.screen.findByText('Last tile state unknown')).toBeTruthy()
  expect(requested.screen.getByText(/Cancel was requested while a tile was out. That tile may have run. It will not be sent again automatically/)).toBeTruthy()
  expect(requested.screen.queryByText('Cloud analysis cancelled')).toBeNull()
})

it('names a refused proposal and an expired consent', async () => {
  const refused = cloud()
  refused.api.proposeRemoteAnalysis.mockImplementationOnce(async () => { throw 'Remote analysis currently requires a paginated chapter' })
  await fireEvent.click(await refused.screen.findByRole('button', { name: 'Analyze with cloud GPU' }))
  expect(await refused.screen.findByText('Page cannot be analyzed')).toBeTruthy()
  cleanup()

  const expired = cloud({ confirm: async () => { throw 'analysis_proposal_expired' },
    status: async () => record({ phase: 'proposed' }) })
  await consentAndSend(expired.screen)
  expect(await expired.screen.findByText('Upload review expired')).toBeTruthy()
})

it('hands the evidence to the review with where it came from', async () => {
  const analysis = { analysisId: 'remote:modal:text_mask_sam_ts@1:abc', samBackend: 'remote' }
  const { screen, onresult } = cloud({ confirm: async () => analysis })
  await consentAndSend(screen)
  await waitFor(() => expect(onresult).toHaveBeenCalledWith(analysis,
    { provider: 'modal', profileName: 'Studio A100', capability: SAM }))
  expect(screen.getByRole('button', { name: 'Analyze with cloud GPU' }).disabled).toBe(false)
})

it('discards an open proposal when the review closes', async () => {
  const { api, screen } = cloud()
  await openConsent(screen)
  screen.unmount()
  await waitFor(() => expect(api.cancelRemoteAnalysis).toHaveBeenCalledWith({ proposalId: 'prop-1' }))
})
