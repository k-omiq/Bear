/**
 * The Layers panel's ways into the cloud, and the one that stays local, against
 * the browser mock with every delay zero.
 *
 * Clean with > Cloud and Try again on a mask the cloud rendered put the
 * consent dialog up before anything is sent, the first time in a project.
 * Confirming it stands for the rest of that project: later renders there go
 * without the dialog, each still on its own single-use grant. Clean anyway
 * never goes to the cloud. The dialog is answered through the modal stack,
 * which is what its buttons do.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { CLOUD_ENGINE, isCloudMask } from '../model/masks.js'
import { app, clearNotices, closeAllModals, closeModal } from '../state/app.svelte.js'
import { stopCloud } from '../state/cloud.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { setCloudAllowed } from '../state/session.svelte.js'
import { requestCloudConsent, runCloudJob } from './cloudflow.svelte.js'
import { reviewQwen } from './qwenflow.js'
import { cleanAnyway, rerunMask, runRegionMenuItem, updateLayer } from './maskactions.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const PROFILE = {
  id: 'mc-abc123',
  name: 'Studio GPU',
  endpointUrl: 'https://ws--mc-abc123-gateway.modal.run/mc/v1',
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
  closeAllModals()
  stopCloud()
  setCloudAllowed(false)
  clearNotices()
  editor.chapter = null
  setBackend(null)
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/**
 * A mock with cloud allowed and one Modal endpoint as the default, and the
 * chapter open on a region that has a local Fill mask.
 */
async function opened() {
  const backend = createMockBackend({ timing: ZERO })
  await backend.writeSettings({ cloudEngines: 'allowed' })
  setCloudAllowed(true)
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
  await backend.storeCloudSecret({
    provider: 'modal',
    profileId: PROFILE.id,
    role: 'runtime',
    secret: 'rt-secret',
    tokenId: 'rt-id',
  })
  setBackend(backend)

  const indices = [0, 1, 2, 3, 4, 5]
  const first = await backend.loadPages({ chapterId: CHAPTER, indices })
  const page = /** @type {any} */ (first.find((candidate) => candidate.regions.length > 0))
  const regionId = page.regions[0].id
  const local = await backend.applyTool({
    tool: 'contentAwareFill',
    params: { engine: 'local' },
    chapterId: CHAPTER,
    pageIndex: page.index,
    regionId,
  })
  expect(local.status).toBe('applied')
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: await backend.loadPages({ chapterId: CHAPTER, indices }),
  })
  return { backend, regionId }
}

/**
 * The region as the open chapter holds it now.
 *
 * @param {string} regionId
 * @returns {any}
 */
function current(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const found = page.regions.find((candidate) => candidate.id === regionId)
    if (found) return found
  }
  return null
}

/**
 * Wait for the consent dialog, the only one up, then answer it. Confirm
 * answers as the dialog does once both statements are ticked.
 *
 * @param {'confirm'|'cancel'} action
 */
async function answerConsent(action) {
  await vi.waitFor(() => expect(app.modals.map((modal) => modal.kind)).toEqual(['cloudConsent']))
  closeModal(action === 'confirm' ? { rightsAttested: true, retentionAcknowledged: true } : action)
}

describe('the cloud from the Layers panel', () => {
  it('preserves rapid layer changes while the first save is pending', async () => {
    const { backend, regionId } = await opened()
    const original = backend.setLayerStyle.bind(backend)
    let release
    const pending = new Promise((resolve) => { release = resolve })
    const save = vi.spyOn(backend, 'setLayerStyle').mockImplementationOnce(async (args) => {
      await pending
      return original(args)
    })
    const stale = current(regionId)
    const opacity = updateLayer(stale, { opacity: 45 })
    const move = updateLayer(stale, { offsetX: 12 })
    await vi.waitFor(() => expect(save).toHaveBeenCalledTimes(1))
    release()
    expect(await opacity).toBe(true)
    expect(await move).toBe(true)
    expect(save).toHaveBeenCalledTimes(2)
    expect(save.mock.calls[1][0].layer).toMatchObject({ opacity: 45, offsetX: 12 })
    expect(current(regionId).mask.layer).toMatchObject({ opacity: 45, offsetX: 12 })
    // Both undo entries reach this backend's journal before the next test
    // swaps the backend; a write still queued would land in that one's.
    await editor.history.running
  })

  it('cleans with Cloud only after consent, and lands the render as one undoable edit', async () => {
    const { backend, regionId } = await opened()
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    const rerun = vi.spyOn(backend, 'rerunMask')
    const before = current(regionId)
    const maskId = before.mask.id
    expect(isCloudMask(before.mask)).toBe(false)
    const undoable = editor.history.entries.length

    const done = runRegionMenuItem(`engine:${CLOUD_ENGINE}`, before)
    await answerConsent('confirm')
    expect(await done).toBe(true)

    expect(prepare).toHaveBeenCalledTimes(1)
    expect(prepare.mock.calls[0][0]).toMatchObject({
      regionId,
      chapterId: CHAPTER,
      intent: { action: 'rerunMask', mask_id: maskId, kind: 'engine', engine: CLOUD_ENGINE },
    })
    expect(rerun).toHaveBeenCalledTimes(1)
    expect(rerun.mock.calls[0][0]).toMatchObject({
      maskId,
      kind: 'engine',
      engine: CLOUD_ENGINE,
      params: { grantNonce: expect.stringMatching(/^grant-/) },
    })

    const after = current(regionId)
    expect(isCloudMask(after.mask)).toBe(true)
    expect(after.mask.provenance.cloud).toMatchObject({ provider: 'modal', profile_id: PROFILE.id })
    // A cloud render runs the recipe a local FLUX run does, so it is not held
    // for review on that account.
    expect(editor.chapter?.review.map((entry) => entry.id)).not.toContain(regionId)
    expect(editor.history.entries).toHaveLength(undoable + 1)
    expect(app.notices.map((notice) => notice.key)).toContain('notice.cloud.finished')
  })

  it('renders Try again on a cloud mask in the cloud again, on a new grant and without asking again', async () => {
    const { backend, regionId } = await opened()
    const first = runRegionMenuItem(`engine:${CLOUD_ENGINE}`, current(regionId))
    await answerConsent('confirm')
    expect(await first).toBe(true)
    const rendered = current(regionId)
    expect(isCloudMask(rendered.mask)).toBe(true)

    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    const confirm = vi.spyOn(backend, 'confirmCloudConsent')
    const rerun = vi.spyOn(backend, 'rerunMask')
    // The project consented with the first render.
    expect(await rerunMask(rendered, 'retry')).toBe(true)
    expect(app.modals).toEqual([])

    expect(prepare).toHaveBeenCalledTimes(1)
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(prepare.mock.calls[0][0].intent).toEqual({
      action: 'rerunMask',
      mask_id: rendered.mask.id,
      kind: 'engine',
      engine: CLOUD_ENGINE,
    })
    expect(rerun).toHaveBeenCalledTimes(1)
    expect(rerun.mock.calls[0][0]).toMatchObject({ kind: 'engine', engine: CLOUD_ENGINE })
    // Never a grantless retry, which the native side would refuse.
    expect(rerun.mock.calls[0][0].params.grantNonce).toEqual(expect.stringMatching(/^grant-/))
    const after = current(regionId)
    expect(isCloudMask(after.mask)).toBe(true)
    expect(after.mask.provenance.cloud.attempt_id).not.toBe(rendered.mask.provenance.cloud.attempt_id)
  })

  it('asks once per project: a second region goes without the dialog, another project asks', async () => {
    const { backend, regionId } = await opened()
    const first = runRegionMenuItem(`engine:${CLOUD_ENGINE}`, current(regionId))
    await answerConsent('confirm')
    expect(await first).toBe(true)

    const intent = { action: 'applyTool', tool: 'contentAwareFill' }
    const other = (editor.chapter?.pages ?? [])
      .flatMap((page) => page.regions.map((region) => ({ page, region })))
      .find(({ region }) => region.id !== regionId)
    expect(other).toBeTruthy()
    const confirm = vi.spyOn(backend, 'confirmCloudConsent')
    const second = await requestCloudConsent(
      { regionId: other.region.id, chapterId: CHAPTER, pageIndex: other.page.index, intent }, backend)
    expect(app.modals).toEqual([])
    expect(second?.params.grantNonce).toEqual(expect.stringMatching(/^grant-/))
    expect(confirm).toHaveBeenCalledTimes(1)
    // The standing region is confirmed without the statements: the project
    // holds the ones ticked for the first.
    expect(confirm.mock.calls[0][0]).toMatchObject({ rightsAttested: false, retentionAcknowledged: false })

    // Another project has not consented, so it asks, and a Cancel sends nothing.
    const elsewhere = requestCloudConsent({ regionId: '', chapterId: 'wandering-moon-ch12', pageIndex: 0, intent }, backend)
    await answerConsent('cancel')
    expect(await elsewhere).toBeNull()
    expect(confirm).toHaveBeenCalledTimes(1)
  })

  it('sends nothing and changes nothing when the consent is cancelled', async () => {
    const { backend, regionId } = await opened()
    const confirm = vi.spyOn(backend, 'confirmCloudConsent')
    const rerun = vi.spyOn(backend, 'rerunMask')
    const before = current(regionId)
    const undoable = editor.history.entries.length

    const done = rerunMask(before, 'engine', CLOUD_ENGINE)
    await answerConsent('cancel')
    expect(await done).toBe(false)

    expect(confirm).not.toHaveBeenCalled()
    expect(rerun).not.toHaveBeenCalled()
    expect(current(regionId).mask.id).toBe(before.mask.id)
    expect(editor.history.entries).toHaveLength(undoable)
  })

  it('re-runs a local mask locally, with no dialog', async () => {
    const { backend, regionId } = await opened()
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    const rerun = vi.spyOn(backend, 'rerunMask')
    const before = current(regionId)

    expect(await rerunMask(before, 'retry')).toBe(true)
    expect(app.modals).toHaveLength(0)
    expect(prepare).not.toHaveBeenCalled()
    expect(rerun).toHaveBeenCalledWith({ maskId: before.mask.id, kind: 'retry', engine: undefined })
    expect(isCloudMask(current(regionId).mask)).toBe(false)
  })

  it('keeps Clean anyway local', async () => {
    const { backend } = await opened()
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    const clean = vi.spyOn(backend, 'cleanAnyway').mockResolvedValue(null)
    const region = /** @type {any} */ ({ id: 'skipped-1', gateSkipCause: 'outside-bubble', mask: null })

    expect(await cleanAnyway(region)).toBe(false)
    expect(app.modals).toHaveLength(0)
    expect(prepare).not.toHaveBeenCalled()
    expect(clean).toHaveBeenCalledTimes(1)
    const [request] = clean.mock.calls[0]
    expect(Object.keys(request).sort()).toEqual(['engine', 'regionId'])
    expect(request.engine).not.toBe(CLOUD_ENGINE)
  })

  it('runs a model picked for a declined region as named, not as a start', async () => {
    const { backend } = await opened()
    const clean = vi.spyOn(backend, 'cleanAnyway').mockResolvedValue(null)
    const region = /** @type {any} */ ({ id: 'declined-1', outcome: 'declined', mask: null })

    expect(await runRegionMenuItem('approve:lama', region)).toBe(false)
    expect(clean).toHaveBeenCalledWith({ regionId: 'declined-1', engine: 'lama', params: { exact: true } })
  })
})

/**
 * A detected region's Clean with: the model picked is the one that runs.
 * Cloud goes to the cloud GPU, never through the run's batch plan that keeps
 * a LaMa pick on this computer.
 */
describe('Clean with on a detected region', () => {
  async function detectedRegion() {
    const { backend } = await opened()
    const indices = [0, 1, 2, 3, 4, 5]
    const pages = await backend.loadPages({ chapterId: CHAPTER, indices })
    const page = /** @type {any} */ (pages.find((candidate) => candidate.regions.some((region) => region.outcome === 'pending')))
    let done = () => {}
    const finished = new Promise((resolve) => { done = /** @type {any} */ (resolve) })
    const stop = backend.subscribe((event) => { if (event.type === 'run-finished') done() })
    await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: page.index, mode: 'detect' })
    await finished
    stop()
    editor.chapter = /** @type {any} */ ({ id: CHAPTER, review: [], pages: await backend.loadPages({ chapterId: CHAPTER, indices }) })
    const region = current(page.regions.find((/** @type {any} */ candidate) => candidate.outcome === 'pending').id)
    expect(region.outcome).toBe('detected')
    return { backend, region }
  }

  it('cleans here with exactly the local model named, with no dialog', async () => {
    const { backend, region } = await detectedRegion()
    const applyTool = vi.spyOn(backend, 'applyTool')
    expect(await runRegionMenuItem('engine:lama', region)).toBe(true)
    expect(app.modals).toEqual([])
    expect(applyTool).toHaveBeenCalledWith(expect.objectContaining({
      tool: 'autoClean', regionId: region.id, params: expect.objectContaining({ engine: 'lama' }) }))
    const after = current(region.id)
    expect(after.outcome).toBe('cleaned')
    expect(after.mask.provenance.engine).toBe('lama')
  })

  it('cleans with Cloud on the cloud GPU after consent, and never through the batch plan', async () => {
    const { backend, region } = await detectedRegion()
    const batch = vi.spyOn(backend, 'prepareCloudClean')
    const applyTool = vi.spyOn(backend, 'applyTool')
    const done = runRegionMenuItem(`engine:${CLOUD_ENGINE}`, region)
    await answerConsent('confirm')
    expect(await done).toBe(true)
    expect(batch).not.toHaveBeenCalled()
    expect(applyTool).toHaveBeenCalledTimes(1)
    expect(applyTool.mock.calls[0][0]).toMatchObject({ tool: 'autoClean', regionId: region.id,
      params: { engine: CLOUD_ENGINE, grantNonce: expect.stringMatching(/^grant-/) } })
    const after = current(region.id)
    expect(after.outcome).toBe('cleaned')
    expect(isCloudMask(after.mask)).toBe(true)
  })

  it('sends nothing and leaves the detection when the consent is cancelled', async () => {
    const { backend, region } = await detectedRegion()
    const applyTool = vi.spyOn(backend, 'applyTool')
    const done = runRegionMenuItem(`engine:${CLOUD_ENGINE}`, region)
    await answerConsent('cancel')
    expect(await done).toBe(false)
    expect(applyTool).not.toHaveBeenCalled()
    expect(current(region.id).outcome).toBe('detected')
  })
})

describe('Qwen descriptions before consent', () => {
  const model = { pinnedModelId: 'Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32', pinnedRecipeId: 'mc-qwen-image-edit-2511-v4',
    pinnedModelRevision: 'a'.repeat(40), preprocessingVersion: '2.0.0', nativeMaskConditioning: false }

  it('binds ordinary words and the chosen target to the prepared recipe, including standing consent', async () => {
    const { backend, regionId } = await opened()
    vi.spyOn(backend, 'getCloudModelInfo').mockResolvedValue(model)
    const prepare = vi.spyOn(backend, 'prepareCloudConsent').mockResolvedValue({ proposalId: 'qwen-prop', standing: true })
    vi.spyOn(backend, 'confirmCloudConsent').mockResolvedValue({ nonce: 'qwen-grant' })
    const request = { chapterId: CHAPTER, pageIndex: 0, regionId, intent: { action: 'cleanAnyway' } }
    const work = requestCloudConsent(request, backend)
    await vi.waitFor(() => expect(app.modals.at(-1)?.kind).toBe('qwenPrompt'))
    expect(prepare).not.toHaveBeenCalled()
    closeModal({ target: 'sound_effect', description: '  Black letters beside the hand  ' })
    const grant = await work
    expect(prepare.mock.calls[0][0].recipe.qwen_edit).toEqual({ target: 'sound_effect', description: 'Black letters beside the hand' })
    expect(grant.params.recipe.qwen_edit).toEqual(prepare.mock.calls[0][0].recipe.qwen_edit)
    expect(app.modals.length).toBe(0)
  })

  it('cancel sends no crop and creates no consent proposal', async () => {
    const { backend, regionId } = await opened()
    vi.spyOn(backend, 'getCloudModelInfo').mockResolvedValue(model)
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    const work = requestCloudConsent({ chapterId: CHAPTER, pageIndex: 0, regionId, intent: { action: 'cleanAnyway' } }, backend)
    await vi.waitFor(() => expect(app.modals.at(-1)?.kind).toBe('qwenPrompt'))
    closeModal(null)
    expect(await work).toBeNull()
    expect(prepare).not.toHaveBeenCalled()
  })

  it('an older Qwen worker is refused before the description can be silently ignored', async () => {
    const { backend, regionId } = await opened()
    vi.spyOn(backend, 'getCloudModelInfo').mockResolvedValue({ ...model, pinnedRecipeId: 'mc-qwen-image-edit-2511-v3' })
    const prepare = vi.spyOn(backend, 'prepareCloudConsent')
    expect(await requestCloudConsent({ chapterId: CHAPTER, pageIndex: 0, regionId, intent: { action: 'cleanAnyway' } }, backend)).toBeNull()
    expect(prepare).not.toHaveBeenCalled()
    expect(app.notices.at(-1).key).toBe('qwen.prompt.updateRequired')
  })
})

it('a review retry acquires a fresh grant with the revised description before rendering again', async () => {
  const { backend, regionId } = await opened()
  const model = { pinnedModelId: 'Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32', pinnedRecipeId: 'mc-qwen-image-edit-2511-v4',
    pinnedModelRevision: 'a'.repeat(40), preprocessingVersion: '2.0.0', nativeMaskConditioning: false }
  vi.spyOn(backend, 'getCloudModelInfo').mockResolvedValue(model)
  vi.spyOn(backend, 'prepareCloudConsent').mockResolvedValue({ proposalId: 'retry-prop', standing: true })
  const confirm = vi.spyOn(backend, 'confirmCloudConsent').mockResolvedValue({ nonce: 'second-grant' })
  vi.spyOn(backend, 'resolveQwenReview').mockResolvedValue(undefined)
  let finishFirst
  const call = vi.fn().mockImplementationOnce(() => new Promise((resolve) => { finishFirst = resolve }))
    .mockResolvedValueOnce({ status: 'applied' })
  const grant = { attemptId: 'att-' + 'a'.repeat(24), params: { grantNonce: 'first-grant',
    executionTarget: { type: 'modal', profile_id: PROFILE.id }, intent: { action: 'cleanAnyway' },
    recipe: { model_id: model.pinnedModelId, qwen_edit: { target: 'sound_effect', description: 'Black letters' } } } }
  const work = runCloudJob(grant, { chapterId: CHAPTER, pageIndex: 0, regionId }, call,
    (answer) => ({ phase: answer.status === 'applied' ? 'committed' : 'cancelled' }))
  reviewQwen({ attemptId: grant.attemptId, before: 'data:image/png;base64,AA==', after: 'data:image/png;base64,AQ==',
    edit: grant.params.recipe.qwen_edit, canRetry: true }, backend)
  closeModal({ choice: 'retry', edit: { target: 'sound_effect', description: 'Also the thick strokes at the top edge' } })
  finishFirst({ status: 'cancelled' })
  await vi.waitFor(() => expect(app.modals.at(-1)?.kind).toBe('qwenPrompt'))
  expect(app.modals.at(-1).props.initial.description).toBe('Also the thick strokes at the top edge')
  expect(call).toHaveBeenCalledTimes(1)
  closeModal({ target: 'sound_effect', description: 'Also the thick strokes at the top edge' })
  expect(await work).toEqual({ status: 'applied' })
  expect(confirm).toHaveBeenCalledOnce()
  expect(call.mock.calls[1][0].grantNonce).toBe('second-grant')
  expect(call.mock.calls[1][0].recipe.qwen_edit.description).toBe('Also the thick strokes at the top edge')
})
