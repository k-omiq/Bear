/**
 * Auto clean with detection on the cloud GPU asks first, for this page only,
 * and starts the run with the grant the answer minted. Nothing about the run
 * changes while every stage is local.
 */

import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { app, closeModal } from '../state/app.svelte.js'
import { stopCloud } from '../state/cloud.svelte.js'
import { stopCloudGpuWatch } from '../state/cloudgpu.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { setAnalysisTarget, setCloudAllowed, setDetectorModels } from '../state/session.svelte.js'
import { cloudRunErrorKey, cloudScopeRefusal, startCleanRun } from './cloudrun.js'
import { applyActiveToolToRegion } from './toolapply.svelte.js'
import { cleanDetected } from './maskactions.svelte.js'

const SAM = 'text_mask_sam_ts@1'
const RT = 'text_regions_rt@1'
/** What a cloud run sends: both stages, sorted, whatever this computer selected. */
const BOTH = [SAM, RT]
const PROPOSAL = { proposalId: 'run-prop', chapterId: 'c1', pageIndices: [0], provider: 'modal', profileId: 'p1',
  profileName: 'Studio A100', capabilities: BOTH, pages: 1, totalTiles: 2, totalTilePixels: 3840000,
  costEstimateUsd: null, models: [{ capability: SAM, graphSha256s: ['a'.repeat(64)], modelRevision: 'c'.repeat(40) }] }

function backend({ allowed = true } = {}) {
  const api = {
    readSettings: async () => ({ cloudEngines: allowed ? 'allowed' : 'blocked' }),
    readInferenceConfig: async () => ({ selectedTarget: { type: 'modal', profile_id: 'p1' }, beamProfiles: {},
      modalProfiles: { p1: { name: 'Studio A100', endpointUrl: 'https://studio.modal.run' } } }),
    getCloudSecretSummary: async () => ({ present: true }),
    proposeRunAnalysis: vi.fn(async () => PROPOSAL),
    confirmRunAnalysis: vi.fn(async () => ({ grantId: 'grant-1', chapterId: 'c1', pageIndices: [0], capabilities: BOTH })),
    cancelRunAnalysis: vi.fn(async () => true),
    runClean: vi.fn(async () => ({ runId: 'run-1', pages: [{ index: 0 }] })),
  }
  setBackend(/** @type {any} */ (api))
  setCloudAllowed(allowed)
  return api
}

/** The consent dialog, once the flow has pushed it. */
async function consent() {
  await vi.waitFor(() => expect(app.modals.at(-1)?.kind).toBe('cloudRunConsent'))
  return app.modals.at(-1)
}

beforeEach(() => {
  editor.project = /** @type {any} */ ({ id: 'p', mode: 'paginated' })
  editor.chapter = /** @type {any} */ ({ id: 'c1', pages: [{ index: 0, number: 1 }], review: [] })
  editor.pageIndex = 0
  // This computer's own choice is deliberately not the cloud combination: a
  // cloud run uses CTD + Full + SAM-TS-L + the reader whatever it holds.
  setDetectorModels(['samTs'])
  setAnalysisTarget('rtFull', 'cloud')
  setAnalysisTarget('samTs', 'cloud')
})

afterEach(() => {
  app.modals = []
  app.notices = []
  editor.run = { ...editor.run, active: false, runId: null }
  editor.chapter = null
  editor.project = null
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  setDetectorModels(['ctd', 'rtSmall'])
  setCloudAllowed(false)
  stopCloud()
  stopCloudGpuWatch()
  setBackend(null)
})

it('proposes this page, asks, and starts the run with the grant the answer minted', async () => {
  const api = backend()
  const started = startCleanRun('page', /** @type {any} */ (api))
  const dialog = await consent()
  expect(dialog.blocking).toBe(true)
  expect(dialog.props.proposal).toEqual(PROPOSAL)
  expect(api.proposeRunAnalysis).toHaveBeenCalledWith({ chapterId: 'c1', scope: 'page', pageIndices: [0],
    capabilities: BOTH, provider: 'modal', profileId: 'p1' })
  expect(api.runClean).not.toHaveBeenCalled()
  closeModal({ rightsAttested: true, retentionAcknowledged: true })
  expect(await started).toBe('run-1')
  expect(api.confirmRunAnalysis).toHaveBeenCalledWith({ proposalId: 'run-prop', rightsAttested: true, retentionAcknowledged: true })
  expect(api.runClean).toHaveBeenCalledWith(expect.objectContaining({ scope: 'page', cloudGrant: 'grant-1',
    analysisTargets: { rtFull: 'cloud', samTs: 'cloud' }, detectorModels: ['ctd', 'rtFull', 'samTs'], ocrRescue: true }))
})

it('skips the question for a project that already consented, and still spends a fresh grant', async () => {
  const api = backend()
  api.proposeRunAnalysis.mockResolvedValueOnce({ ...PROPOSAL, standing: true })
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBe('run-1')
  expect(app.modals).toEqual([])
  // No statement is answered for the user: the native side holds the ones
  // the project's consent was given with.
  expect(api.confirmRunAnalysis).toHaveBeenCalledWith({ proposalId: 'run-prop', rightsAttested: false, retentionAcknowledged: false })
  expect(api.runClean).toHaveBeenCalledWith(expect.objectContaining({ cloudGrant: 'grant-1' }))
})

it('runs a cloud Detect with CTD and the reader here whatever this computer selected, Small included', async () => {
  setDetectorModels(['ctd', 'rtSmall'])
  const api = backend()
  const started = startCleanRun('page', /** @type {any} */ (api))
  await consent()
  expect(api.proposeRunAnalysis).toHaveBeenCalledWith(expect.objectContaining({ capabilities: BOTH }))
  closeModal({ rightsAttested: true, retentionAcknowledged: true })
  expect(await started).toBe('run-1')
  expect(api.runClean).toHaveBeenCalledWith(expect.objectContaining({ detectorModels: ['ctd', 'rtFull', 'samTs'], ocrRescue: true }))
  expect(app.notices).toEqual([])
  // This computer's own combination is left as it was, for This computer.
  const { session } = await import('../state/session.svelte.js')
  expect(session.detectorModels).toEqual(['ctd', 'rtSmall'])
  // The native refusal of a cloud run holding Small still reads as a sentence.
  expect(cloudRunErrorKey(new Error('cloud_detect_small_unsupported'))).toBe('tools.target.cloudSmallRefused')
})

// A refusal the run has no code of its own for reads as the review dialog
// reads it: a key the app cannot read says so, and only what nobody named is
// a failure to prepare the review.
it('says a missing or unreadable key as a key problem, not as a failed review', () => {
  expect(cloudRunErrorKey('credential missing: modal: runtime credential missing for profile \'p1\''))
    .toBe('cloud.analysis.profile.credential')
  expect(cloudRunErrorKey('analysis_profile_not_active')).toBe('cloud.analysis.profile.inactive')
  expect(cloudRunErrorKey(new Error('something nobody named'))).toBe('cloud.analysis.failure.propose')
})

it('proposes a chapter run for the pages the run will walk, with one consent for all of them', async () => {
  const api = backend()
  const chapterProposal = { ...PROPOSAL, pageIndices: [0, 1, 2], pages: 3, totalTiles: 6 }
  api.proposeRunAnalysis.mockResolvedValueOnce(chapterProposal)
  api.confirmRunAnalysis.mockResolvedValueOnce({ grantId: 'grant-ch', chapterId: 'c1', pageIndices: [0, 1, 2], capabilities: BOTH })
  const started = startCleanRun('chapter', /** @type {any} */ (api))
  const dialog = await consent()
  expect(api.proposeRunAnalysis).toHaveBeenCalledWith({ chapterId: 'c1', scope: 'chapter', pageIndices: null,
    capabilities: BOTH, provider: 'modal', profileId: 'p1' })
  expect(dialog.props).toMatchObject({ scope: 'chapter', proposal: { pages: 3 } })
  // Turning the page while the dialog is open does not void a chapter's consent.
  editor.pageIndex = 2
  closeModal({ rightsAttested: true, retentionAcknowledged: true })
  expect(await started).toBe('run-1')
  expect(api.runClean).toHaveBeenCalledWith(expect.objectContaining({ scope: 'chapter', cloudGrant: 'grant-ch' }))
})

it('names a chapter over the cap, and a chapter with nothing left to clean', async () => {
  const api = backend()
  api.proposeRunAnalysis.mockRejectedValueOnce(new Error('cloud_run_too_many_pages'))
  expect(await startCleanRun('chapter', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.analysis.run.tooMany')
  api.proposeRunAnalysis.mockRejectedValueOnce('cloud_run_no_pages')
  expect(await startCleanRun('chapter', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.analysis.run.nothing')
  expect(app.modals).toEqual([])
  expect(api.runClean).not.toHaveBeenCalled()
})

it('discards the proposal and starts nothing when the consent is cancelled', async () => {
  const api = backend()
  const started = startCleanRun('page', /** @type {any} */ (api))
  await consent()
  closeModal('cancel')
  expect(await started).toBeNull()
  await vi.waitFor(() => expect(api.cancelRunAnalysis).toHaveBeenCalledWith({ proposalId: 'run-prop' }))
  expect(api.confirmRunAnalysis).not.toHaveBeenCalled()
  expect(api.runClean).not.toHaveBeenCalled()
})

it('refuses a project run and an unusable cloud GPU before proposing anything, and never a long strip', async () => {
  const api = backend()
  expect(await startCleanRun('project', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.analysis.run.scopeProject')

  editor.project = /** @type {any} */ ({ id: 'p', mode: 'longstrip' })
  expect(cloudScopeRefusal('page', { detect: true, clean: true })).toBeNull()
  expect(cloudScopeRefusal('chapter', { detect: true, clean: false })).toBeNull()

  editor.project = /** @type {any} */ ({ id: 'p', mode: 'paginated' })
  const off = backend({ allowed: false })
  expect(await startCleanRun('page', /** @type {any} */ (off))).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.analysis.run.unavailable')

  expect(api.proposeRunAnalysis).not.toHaveBeenCalled()
  expect(off.proposeRunAnalysis).not.toHaveBeenCalled()
  expect(api.runClean).not.toHaveBeenCalled()
  expect(off.runClean).not.toHaveBeenCalled()
})

it('names a refused grant and does not fall back to a local run', async () => {
  const api = backend()
  api.confirmRunAnalysis.mockRejectedValueOnce(new Error('cloud_run_profile_changed'))
  const started = startCleanRun('page', /** @type {any} */ (api))
  await consent()
  closeModal({ rightsAttested: true, retentionAcknowledged: true })
  expect(await started).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('cloud.analysis.run.profileChanged')
  expect(api.runClean).not.toHaveBeenCalled()
})

it('starts a run with every stage local exactly as before, with no consent', async () => {
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
  const api = backend()
  expect(await startCleanRun('project', /** @type {any} */ (api))).toBe('run-1')
  expect(api.proposeRunAnalysis).not.toHaveBeenCalled()
  expect(api.runClean.mock.calls[0][0]).not.toHaveProperty('cloudGrant')
  // This computer's own combination and reader switch, as stored.
  expect(api.runClean.mock.calls[0][0]).toMatchObject({ detectorModels: ['samTs'], ocrRescue: false })
  expect(app.modals).toEqual([])
})

it('keeps one detection proposal open for repeated Run input', async () => {
  const api = backend()
  const first = startCleanRun('page', /** @type {any} */ (api))
  await consent()
  expect(await startCleanRun('page', /** @type {any} */ (api))).toBeNull()
  expect(app.notices.at(-1)?.key).toBe('notice.run.busy')
  expect(api.proposeRunAnalysis).toHaveBeenCalledTimes(1)
  closeModal('cancel')
  expect(await first).toBeNull()
})

it('blocks canvas and Layers clean actions while a run proposal is pending', async () => {
  const api = backend()
  api.applyTool = vi.fn(async () => ({ status: 'applied' }))
  const region = { id: 'r1', pageId: 'p0', detected: true, outcome: 'detected', mask: null }
  editor.chapter.pages[0].regions = [region]
  editor.tool = 'autoClean'
  const first = startCleanRun('page', /** @type {any} */ (api))
  await consent()
  expect(await applyActiveToolToRegion('r1')).toBe(false)
  expect(await cleanDetected(region)).toBe(false)
  expect(api.applyTool).not.toHaveBeenCalled()
  closeModal('cancel')
  expect(await first).toBeNull()
})

it('cancels a proposal that answers after the reader changes page', async () => {
  const api = backend()
  let answer
  api.proposeRunAnalysis.mockImplementationOnce(() => new Promise((resolve) => { answer = resolve }))
  const started = startCleanRun('page', /** @type {any} */ (api))
  await vi.waitFor(() => expect(api.proposeRunAnalysis).toHaveBeenCalledTimes(1))
  editor.pageIndex = 1
  answer(PROPOSAL)
  expect(await started).toBeNull()
  expect(api.cancelRunAnalysis).toHaveBeenCalledWith({ proposalId: 'run-prop' })
  expect(app.modals).toEqual([])
})
