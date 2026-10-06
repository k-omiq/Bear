/**
 * Clean on the cloud GPU: the run detects, and only once it has finished is
 * the plan prepared, shown and confirmed. Nothing is sent before the consent,
 * a declined proposal is dropped, and nothing falls back to a local clean.
 * The cloud means the cloud: `localFirst` is false unless the tool's mixed
 * choice is on, and one consent covers a plan of any number of batches.
 *
 * `runFinished` is the one stub: it is what the flow waits on between its two
 * halves, and holding it open is how these tests see the order.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { app, closeModal } from '../state/app.svelte.js'
import { hasKey } from '../i18n/index.js'
import { stopCloud } from '../state/cloud.svelte.js'
import { stopCloudGpuWatch } from '../state/cloudgpu.svelte.js'
import { editor, runFinished } from '../state/editor.svelte.js'
import { setAnalysisTarget, setCleanLocalFirst, setCleanTarget, setCloudAllowed, setDetectorModels } from '../state/session.svelte.js'
import { cleanLocalFirst, cloudCleanErrorKey, startCleanRun } from './cloudrun.js'

vi.mock('../state/editor.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  runFinished: vi.fn(),
}))

const SAM = 'text_mask_sam_ts@1'
const PROPOSAL = { proposalId: 'clean-prop', chapterId: 'c1', regionIds: ['c1-p0-d1', 'c1-p0-d2'], regions: 2, pages: 1,
  totalCropPixels: 120000, totalWorkPixels: 1179648, localCleaned: 0, execution: 'cloud', localCandidates: 0,
  chunkRegions: 256, chunks: 1, unresolvedIds: [], estimatedGpuSeconds: { low: 22, high: 677 },
  estimatedCostUsd: { low: 0.02, high: 0.05 }, gpu: 'L4', provider: 'modal', profileName: 'Studio A100',
  planDigest: 'a'.repeat(64), expiresAtMs: Date.now() + 300000 }
// As the clean consent dialog answers Confirm: the statements and the plan it showed.
const ANSWER = { rightsAttested: true, retentionAcknowledged: true, planDigest: PROPOSAL.planDigest }

/** The end of the detection run, released by the test. */
let finish = /** @type {(event: any) => void} */ (() => {})

function backend() {
  const api = {
    getCloudModelInfo: vi.fn(async () => ({ pinnedModelId: 'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic' })),
    readSettings: async () => ({ cloudEngines: 'allowed' }),
    readInferenceConfig: async () => ({ selectedTarget: { type: 'modal', profile_id: 'p1' }, beamProfiles: {},
      modalProfiles: { p1: { name: 'Studio A100', endpointUrl: 'https://studio.modal.run' } } }),
    getCloudSecretSummary: async () => ({ present: true }),
    proposeRunAnalysis: vi.fn(async () => ({ proposalId: 'run-prop', chapterId: 'c1', pageIndices: [0], provider: 'modal',
      profileId: 'p1', profileName: 'Studio A100', capabilities: [SAM], pages: 1, models: [] })),
    confirmRunAnalysis: vi.fn(async () => ({ grantId: 'run-grant' })),
    cancelRunAnalysis: vi.fn(async () => true),
    runClean: vi.fn(async () => ({ runId: 'run-1', pages: [{ index: 0 }] })),
    prepareCloudClean: vi.fn(async () => PROPOSAL),
    confirmCloudClean: vi.fn(async () => ({ grantId: 'clean-grant', expiresAtMs: Date.now() + 120000 })),
    startCloudClean: vi.fn(async () => ({ runId: 'run-2', pages: [{ index: 0 }] })),
    cancelCloudClean: vi.fn(async () => true),
    reloadPage: vi.fn(async () => null),
  }
  setBackend(/** @type {any} */ (api))
  setCloudAllowed(true)
  return api
}

/** @param {string} kind */
async function dialog(kind) {
  await vi.waitFor(() => expect(app.modals.at(-1)?.kind).toBe(kind))
  return app.modals.at(-1)
}

/** End the detection run the way its `run-finished` event does. */
function detectionEnds(reason = 'completed') {
  editor.run = { ...editor.run, active: false, runId: null }
  finish({ type: 'run-finished', runId: 'run-1', chapterId: 'c1', reason })
}

beforeEach(() => {
  setCleanLocalFirst(false)
  vi.mocked(runFinished).mockImplementation(() => new Promise((resolve) => { finish = resolve }))
  editor.project = /** @type {any} */ ({ id: 'p', mode: 'paginated' })
  editor.chapter = /** @type {any} */ ({ id: 'c1', pages: [{ index: 0, number: 1, regions: [] }], review: [] })
  editor.pageIndex = 0
  editor.toolParams = { ...editor.toolParams, autoClean: { ...(editor.toolParams.autoClean ?? {}), step: 'auto' } }
  setDetectorModels(['ctd', 'rtSmall'])
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  setCleanTarget('cloud')
})

afterEach(async () => {
  setCleanLocalFirst(false)
  delete editor.toolParams.autoClean.localFirst
  while (app.modals.length) closeModal('cancel')
  await new Promise((resolve) => setTimeout(resolve, 0))
  app.modals = []
  app.notices = []
  editor.run = { ...editor.run, active: false, runId: null }
  editor.chapter = null
  editor.project = null
  setCleanTarget('local')
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  setDetectorModels(['ctd', 'rtSmall'])
  setCloudAllowed(false)
  stopCloud()
  stopCloudGpuWatch()
  setBackend(null)
  vi.clearAllMocks()
})

it('detects here, then prepares, asks and starts the cloud clean only after the run has finished', async () => {
  const api = backend()
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBe('run-1')
  // Detection only: nothing is cleaned here that is to be cleaned there.
  expect(api.runClean).toHaveBeenCalledWith(expect.objectContaining({ scope: 'page', mode: 'detect' }))
  expect(api.prepareCloudClean).not.toHaveBeenCalled()

  detectionEnds()
  const consent = await dialog('cloudCleanConsent')
  // Strictly the cloud: nothing is cleaned here, before or after the consent.
  // The panel's picks go with it: they are this clean's.
  expect(api.prepareCloudClean).toHaveBeenCalledWith({ chapterId: 'c1', scope: 'page', pageIndices: [0], regionIds: null,
    localFirst: false, bubbleEngine: 'fill', outsideEngine: 'lama' })
  expect(consent.blocking).toBe(true)
  expect(consent.props).toEqual({ proposal: PROPOSAL, endpoint: 'https://studio.modal.run/' })
  expect(api.confirmCloudClean).not.toHaveBeenCalled()
  expect(api.startCloudClean).not.toHaveBeenCalled()

  closeModal(ANSWER)
  await vi.waitFor(() => expect(api.startCloudClean).toHaveBeenCalledWith({ grantId: 'clean-grant' }))
  expect(api.confirmCloudClean).toHaveBeenCalledWith({ proposalId: 'clean-prop', ...ANSWER })
  expect(editor.run).toMatchObject({ active: true, runId: 'run-2', scope: 'page', mode: 'clean' })
})

it('asks for the detection consent before anything is sent, then the clean consent after', async () => {
  const api = backend()
  setDetectorModels(['samTs'])
  setAnalysisTarget('rtFull', 'cloud')
  setAnalysisTarget('samTs', 'cloud')
  const started = startCleanRun('page', /** @type {any} */ (api))
  const run = await dialog('cloudRunConsent')
  // The detection consent says the clean asks separately, and covers no clean.
  expect(run.props).toMatchObject({ scope: 'page', cleanFollows: true })
  expect(api.runClean).not.toHaveBeenCalled()
  closeModal(ANSWER)
  expect(await started).toBe('run-1')
  expect(api.runClean).toHaveBeenCalledWith(expect.objectContaining({ cloudGrant: 'run-grant', mode: 'detect' }))
  expect(api.prepareCloudClean).not.toHaveBeenCalled()
  detectionEnds()
  await dialog('cloudCleanConsent')
  expect(api.startCloudClean).not.toHaveBeenCalled()
})

it('drops the proposal and sends nothing when the clean consent is declined', async () => {
  const api = backend()
  await startCleanRun('page', /** @type {any} */ (api))
  detectionEnds()
  await dialog('cloudCleanConsent')
  closeModal('cancel')
  await vi.waitFor(() => expect(api.cancelCloudClean).toHaveBeenCalledWith({ proposalId: 'clean-prop' }))
  expect(api.confirmCloudClean).not.toHaveBeenCalled()
  expect(api.startCloudClean).not.toHaveBeenCalled()
  // Detected stays detected: no local clean is started in its place.
  expect(api.runClean).toHaveBeenCalledTimes(1)
  expect(app.notices.at(-1)?.key).toBe('cloud.clean.declined')
})

it('stops at a cancelled detection run, and at a chapter closed meanwhile', async () => {
  const api = backend()
  await startCleanRun('page', /** @type {any} */ (api))
  detectionEnds('cancelled')
  await Promise.resolve()
  await Promise.resolve()
  expect(api.prepareCloudClean).not.toHaveBeenCalled()

  await startCleanRun('page', /** @type {any} */ (api))
  editor.chapter = /** @type {any} */ ({ id: 'c2', pages: [], review: [] })
  detectionEnds()
  await vi.waitFor(() => expect(app.notices.at(-1)?.key).toBe('cloud.clean.leftDetected'))
  expect(api.prepareCloudClean).not.toHaveBeenCalled()
})

it('with Step set to Clean, prepares straight away and runs no detection', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  const started = startCleanRun('chapter', /** @type {any} */ (api))
  await dialog('cloudCleanConsent')
  expect(api.prepareCloudClean).toHaveBeenCalledWith(expect.objectContaining({ scope: 'chapter', pageIndices: null }))
  expect(api.runClean).not.toHaveBeenCalled()
  closeModal(ANSWER)
  expect(await started).toBe('run-2')
  expect(editor.run.scope).toBe('chapter')
})

it('cancels a clean proposal that answers after navigation, without showing consent', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  let answer
  api.prepareCloudClean.mockImplementationOnce(() => new Promise((resolve) => { answer = resolve }))
  const started = startCleanRun('page', /** @type {any} */ (api))
  await vi.waitFor(() => expect(api.prepareCloudClean).toHaveBeenCalledTimes(1))
  editor.pageIndex = 1
  answer(PROPOSAL)
  expect(await started).toBeNull()
  expect(api.cancelCloudClean).toHaveBeenCalledWith({ proposalId: 'clean-prop' })
  expect(app.modals).toEqual([])
})

it('shows the endpoint path without its credentials or query', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.readInferenceConfig = async () => ({ selectedTarget: { type: 'modal', profile_id: 'p1' }, beamProfiles: {},
    modalProfiles: { p1: { name: 'Studio A100', endpointUrl: 'https://reader:secret@studio.modal.run:8443/api/v1?token=hidden' } } })
  const started = startCleanRun('page', /** @type {any} */ (api))
  const consent = await dialog('cloudCleanConsent')
  expect(consent.props.endpoint).toBe('https://studio.modal.run:8443/api/v1')
  closeModal('cancel')
  expect(await started).toBeNull()
})

it('says so when every region waits on an unresolved request, and asks nothing', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.prepareCloudClean.mockResolvedValueOnce({ ...PROPOSAL, proposalId: null, regionIds: [], regions: 0, chunks: 0,
    unresolvedIds: ['c1-p0-d1', 'c1-p0-d2'] })
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)).toMatchObject({ key: 'cloud.clean.unresolvedOnly', params: { count: 2 }, tone: 'warn' })
  expect(app.modals).toEqual([])
  expect(api.confirmCloudClean).not.toHaveBeenCalled()

  api.prepareCloudClean.mockResolvedValueOnce({ ...PROPOSAL, proposalId: null, regionIds: [], regions: 0, chunks: 0 })
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.clean.nothing')
  expect(api.startCloudClean).not.toHaveBeenCalled()
})

it('says so when every region is too large for the render service, and asks nothing', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.prepareCloudClean.mockResolvedValueOnce({ ...PROPOSAL, proposalId: null, regionIds: [], regions: 0, chunks: 0,
    tooLargeIds: ['c1-p0-d1'] })
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)).toMatchObject({ key: 'cloud.clean.tooLargeOnly', params: { count: 1 }, tone: 'warn' })
  expect(app.notices.some((notice) => notice.key === 'cloud.clean.nothing')).toBe(false)
  expect(app.modals).toEqual([])
  expect(api.confirmCloudClean).not.toHaveBeenCalled()
})

it('asks once for a 1,025-region plan and starts it once, however many batches it takes', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  const ids = Array.from({ length: 1025 }, (_, n) => `c1-p${Math.floor(n / 25)}-d${n % 25}`)
  const plan = { ...PROPOSAL, regionIds: ids, regions: 1025, pages: 41, chunks: 5, chunkRegions: 256 }
  api.prepareCloudClean.mockResolvedValueOnce(plan)
  const started = startCleanRun('chapter', /** @type {any} */ (api))
  const consent = await dialog('cloudCleanConsent')
  expect(consent.props.proposal).toEqual(plan)
  expect(api.startCloudClean).not.toHaveBeenCalled()
  closeModal(ANSWER)
  expect(await started).toBe('run-2')
  // One plan, one consent, one grant, one start: the batches are the native run's.
  expect(api.prepareCloudClean).toHaveBeenCalledTimes(1)
  expect(api.prepareCloudClean).toHaveBeenCalledWith(expect.objectContaining({ scope: 'chapter', localFirst: false }))
  expect(api.confirmCloudClean).toHaveBeenCalledTimes(1)
  expect(api.confirmCloudClean).toHaveBeenCalledWith({ proposalId: 'clean-prop', ...ANSWER })
  expect(api.startCloudClean).toHaveBeenCalledTimes(1)
  expect(api.startCloudClean).toHaveBeenCalledWith({ grantId: 'clean-grant' })
  expect(app.modals.filter((modal) => modal.kind === 'cloudCleanConsent')).toEqual([])
})

it('asks for mixed execution only when the tool\'s choice is on, from a run and from a Layers row', async () => {
  expect(cleanLocalFirst({})).toBe(false)
  expect(cleanLocalFirst({ localFirst: 'yes' })).toBe(false)
  expect(cleanLocalFirst({ localFirst: true })).toBe(true)
  setCleanLocalFirst(true)
  expect(cleanLocalFirst({})).toBe(true)
  expect(cleanLocalFirst({ localFirst: false })).toBe(false)

  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  editor.toolParams.autoClean.localFirst = true
  const mixed = { ...PROPOSAL, execution: 'mixed', localCandidates: 1 }
  api.prepareCloudClean.mockResolvedValueOnce(mixed)
  const started = startCleanRun('page', /** @type {any} */ (api))
  const consent = await dialog('cloudCleanConsent')
  expect(api.prepareCloudClean).toHaveBeenCalledWith(expect.objectContaining({ localFirst: true }))
  expect(consent.props.proposal.execution).toBe('mixed')
  closeModal(ANSWER)
  expect(await started).toBe('run-2')
})

it('confirms the plan the dialog showed, and says so when it is refused', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.confirmCloudClean.mockRejectedValueOnce(new Error('cloud_clean_plan_mismatch'))
  const started = startCleanRun('page', /** @type {any} */ (api))
  await dialog('cloudCleanConsent')
  closeModal({ ...ANSWER, planDigest: 'c'.repeat(64) })
  expect(await started).toBeNull()
  expect(api.confirmCloudClean).toHaveBeenCalledWith({ proposalId: 'clean-prop', ...ANSWER, planDigest: 'c'.repeat(64) })
  expect(app.notices.at(-1)).toMatchObject({ key: 'cloud.clean.error.planChanged', tone: 'warn' })
  expect(api.startCloudClean).not.toHaveBeenCalled()

  // An answer without a plan sends none, and the native side refuses it.
  const again = startCleanRun('page', /** @type {any} */ (api))
  await dialog('cloudCleanConsent')
  closeModal({ rightsAttested: true, retentionAcknowledged: true })
  await again
  expect(api.confirmCloudClean).toHaveBeenLastCalledWith({ proposalId: 'clean-prop', rightsAttested: true,
    retentionAcknowledged: true, planDigest: '' })
})

it('says a chapter held by another process as such when preparing', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.prepareCloudClean.mockRejectedValueOnce(new Error('job_busy: another Manga Cleaner process is using /lib/c1.mtclean'))
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)).toMatchObject({ key: 'notice.job.busy', tone: 'warn' })
  expect(app.modals).toEqual([])
})

it('names a failed start and does not clean locally instead', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.startCloudClean.mockRejectedValueOnce(new Error('cloud_clean_failed: gateway_unreachable'))
  const started = startCleanRun('page', /** @type {any} */ (api))
  await dialog('cloudCleanConsent')
  closeModal(ANSWER)
  expect(await started).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.clean.failure.start')
  expect(api.runClean).not.toHaveBeenCalled()
  expect(editor.run.active).toBe(false)
})

it('skips the question for a project that already consented, and confirms the prepared plan', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.prepareCloudClean.mockResolvedValueOnce({ ...PROPOSAL, standing: true })
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBe('run-2')
  expect(app.modals).toEqual([])
  // No statement is answered for the user: the native side holds the ones
  // the project's consent was given with.
  expect(api.confirmCloudClean).toHaveBeenCalledWith({ proposalId: 'clean-prop', planDigest: PROPOSAL.planDigest,
    rightsAttested: false, retentionAcknowledged: false })
  expect(api.startCloudClean).toHaveBeenCalledWith({ grantId: 'clean-grant' })
})

it('keeps the regions detected and adopts nothing when another run holds the slot', async () => {
  const api = backend()
  editor.toolParams.autoClean.step = 'clean'
  api.startCloudClean.mockResolvedValueOnce({ runId: 'other-run', pages: [], alreadyRunning: true })
  const started = startCleanRun('page', /** @type {any} */ (api))
  await dialog('cloudCleanConsent')
  closeModal(ANSWER)
  expect(await started).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.clean.busy')
  expect(editor.run.active).toBe(false)
  expect(editor.run.runId).not.toBe('other-run')
})

describe('the cloud clean refusals, in plain words', () => {
  const CODES = {
    prepare: ['cloud_disabled', 'cloud_clean_profile_not_active', 'credential_missing', 'endpoint_invalid',
      'cloud_clean_run_active', 'cloud_clean_scope_unsupported', 'cloud_clean_no_pages', 'cloud_clean_page_not_found',
      'cloud_clean_no_regions', 'cloud_clean_region_not_found', 'cloud_clean_source_missing',
      'cloud_clean_too_many_regions', 'cloud_clean_proposal_limit', 'gateway_unauthorized', 'gateway_unreachable',
      'gateway_error', 'gateway_protocol', 'job_busy: another Manga Cleaner process is using /x'],
    confirm: ['rights_attestation_required', 'retention_acknowledgement_required', 'cloud_clean_proposal_missing',
      'cloud_clean_proposal_expired', 'cloud_clean_profile_changed', 'cloud_clean_grant_limit',
      'cloud_clean_plan_mismatch'],
    start: ['cloud_clean_grant_missing', 'cloud_clean_grant_expired', 'cloud_clean_grant_mismatch: profile',
      'cloud_clean_grant_mismatch: endpoint', 'cloud_clean_grant_mismatch: recipe', 'cloud_clean_profile_changed'],
  }

  it('gives every native code a sentence of its own, not the step\'s fallback', () => {
    for (const [step, codes] of Object.entries(CODES)) {
      const fallback = `cloud.clean.failure.${step}`
      for (const code of codes) {
        const key = cloudCleanErrorKey(new Error(code), fallback)
        expect(key, code).not.toBe(fallback)
        expect(hasKey(key), `${code} -> ${key}`).toBe(true)
      }
    }
  })

  it('groups codes that mean the same thing to the reader', () => {
    const key = (code) => cloudCleanErrorKey(code, 'fallback')
    expect(new Set(['cloud_clean_profile_not_active', 'credential_missing', 'endpoint_invalid'].map(key)).size).toBe(1)
    expect(new Set(['gateway_error', 'gateway_protocol'].map(key)).size).toBe(1)
    expect(key('cloud_clean_grant_mismatch: recipe')).toBe(key('cloud_clean_profile_changed'))
    expect(key('cloud_clean_proposal_expired')).toBe(key('cloud_clean_grant_expired'))
  })

  it('reads an unknown code as the step that failed', () => {
    expect(cloudCleanErrorKey(new Error('unknown_prepare_code'), 'cloud.clean.failure.prepare')).toBe('cloud.clean.failure.prepare')
    expect(cloudCleanErrorKey('something else', 'cloud.clean.failure.start')).toBe('cloud.clean.failure.start')
  })
})

it('Qwen batch guidance is requested before a plan is prepared, even with standing consent', async () => {
  const api = backend()
  api.getCloudModelInfo.mockResolvedValue({ pinnedModelId: 'Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32', pinnedRecipeId: 'mc-qwen-image-edit-2511-v4' })
  api.prepareCloudClean.mockResolvedValue({ ...PROPOSAL, standing: true })
  editor.toolParams.autoClean.step = 'clean'
  const work = startCleanRun('page')
  await dialog('qwenPrompt')
  expect(api.prepareCloudClean).not.toHaveBeenCalled()
  closeModal({ target: 'auto', description: '' })
  await work
  expect(api.prepareCloudClean.mock.calls[0][0].qwenEdit).toEqual({ target: 'auto', description: '' })
  expect(api.startCloudClean).toHaveBeenCalledWith({ grantId: 'clean-grant' })
})
