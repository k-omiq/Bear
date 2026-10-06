/**
 * The browser mock's background jobs, held to the contract the native side is
 * built against: several runs at once, one per chapter and three in all;
 * `cancelRun` by id; `listJobs` naming what is going; a cloud denoise that
 * reports progress under the caller's run id and stops on `cancelDenoiseLocal`;
 * and the quit guard's question, raised by hand.
 */

import { describe, expect, it } from 'vitest'

import { createMockBackend } from './mock.js'
import { DENOISE_PRESETS } from '../model/denoise.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const OTHER = 'wandering-moon-ch12'
const MODAL_KEYS = { token_id: 'ak-fake', token_secret: 'as-fake' }
const ANSWER = { rightsAttested: true, retentionAcknowledged: true }

/** Slow enough that a run is still going when the next call lands. */
function makeMock(options = {}) {
  return createMockBackend({ timing: { method: 0, cloud: 0, provision: 0, analysis: 0, region: 30, pageTail: 0 }, ...options })
}

/** A new runnable chapter in a project, since only two fixture chapters are uncleaned. */
async function freshChapter(mock, projectId, number) {
  const chapter = await mock.createChapter({ projectId, name: `Fresh ${number}`, number })
  expect(chapter?.id).toBeTruthy()
  return chapter.id
}

/** Wait for a run's end. */
function ended(mock, runId) {
  return new Promise((resolve) => {
    const stop = mock.subscribe((event) => {
      if (event.type === 'run-finished' && event.runId === runId) {
        stop()
        resolve(event)
      }
    })
  })
}

describe('several runs at once', () => {
  it('runs two chapters side by side and lists both', async () => {
    const mock = makeMock()
    const first = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER, mode: 'detect' })
    const second = await mock.runClean({ scope: 'chapter', chapterId: OTHER })
    expect(first.runId).toBeTruthy()
    expect(second.runId).toBeTruthy()
    expect(second.runId).not.toBe(first.runId)
    expect(second.alreadyRunning).toBeUndefined()

    const listed = await mock.listJobs()
    expect(listed).toEqual(expect.arrayContaining([
      expect.objectContaining({ runId: first.runId, kind: 'detect', chapterId: CHAPTER, total: first.pages.length }),
      expect.objectContaining({ runId: second.runId, kind: 'clean', chapterId: OTHER, total: second.pages.length }),
    ]))

    const firstEnd = ended(mock, first.runId)
    expect(await mock.cancelRun({ runId: first.runId })).toBe(first.runId)
    expect(await firstEnd).toMatchObject({ reason: 'cancelled', chapterId: CHAPTER })
    expect((await mock.listJobs()).map((job) => job.runId)).toEqual([second.runId])
    await mock.cancelRun({ runId: second.runId })
    expect(await mock.listJobs()).toEqual([])
  })

  it('answers a chapter that already has a run with that run, and a full house with atCapacity', async () => {
    const mock = makeMock()
    const first = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER })
    expect(await mock.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0 }))
      .toEqual({ runId: first.runId, pages: [], alreadyRunning: true })

    const second = await mock.runClean({ scope: 'chapter', chapterId: OTHER })
    const third = await mock.runClean({ scope: 'chapter', chapterId: await freshChapter(mock, 'tsuki-to-hane', 200) })
    expect(second.runId && third.runId).toBeTruthy()
    const fourth = await mock.runClean({ scope: 'chapter', chapterId: await freshChapter(mock, 'tsuki-to-hane', 201) })
    expect(fourth).toEqual({ runId: null, pages: [], alreadyRunning: true, atCapacity: true })

    // With no id, `cancelRun` cannot mean one of three.
    expect(await mock.cancelRun()).toBeNull()
    for (const handle of [first, second, third]) await mock.cancelRun({ runId: handle.runId })
    expect(await mock.listJobs()).toEqual([])
  })

  it('refuses a cloud clean only for the chapter that has a run', async () => {
    const mock = makeMock()
    await mock.writeSettings({ cloudEngines: 'allowed' })
    const plan = await mock.runCloudProvisioner({ op: 'plan', provider: 'modal',
      params: { credentials: MODAL_KEYS, installation_id: 'mc-jobs01', options: {} } })
    await mock.runCloudProvisioner({ op: 'apply', provider: 'modal',
      params: { credentials: MODAL_KEYS, installation_id: 'mc-jobs01', approved_plan_hash: plan.data.plan_hash } })
    const detected = await mock.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect' })
    await ended(mock, detected.runId)

    const other = await mock.runClean({ scope: 'chapter', chapterId: OTHER })
    expect((await mock.listJobs()).map((job) => job.chapterId)).toEqual([OTHER])
    // Another chapter's run does not stand in the way; this chapter's does.
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    expect(proposal.proposalId).toBeTruthy()
    await mock.cancelCloudClean({ proposalId: proposal.proposalId })
    const own = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER })
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false }))
      .rejects.toThrow('cloud_clean_run_active')
    // A grant nobody minted is refused as such, not answered as busy.
    await expect(mock.startCloudClean({ grantId: 'nope' })).rejects.toThrow('cloud_clean_grant_missing')
    await mock.cancelRun({ runId: own.runId })
    await mock.cancelRun({ runId: other.runId })
  })
})

describe('denoise jobs', () => {
  it('lists a local denoise with its progress, and stops it by run id', async () => {
    const mock = createMockBackend({ timing: { method: 0, analysis: 400, region: 40, pageTail: 0 } })
    await mock.downloadModelGroup({ id: 'pageDenoise' })
    await expect.poll(async () => (await mock.listModels()).models
      .filter((row) => row.requiredBy.includes('pageDenoise')).every((row) => row.installed), { timeout: 5000 }).toBe(true)

    const seen = []
    await mock.onDenoiseProgress((event) => seen.push(event))
    const answer = mock.denoiseChapterLocal({ runId: 'den-local', chapterId: CHAPTER, presetId: 'waifu2x-scan-4x-n2', outDir: '/tmp/out' })
    await expect.poll(() => seen.length).toBeGreaterThan(2)
    expect(await mock.listJobs()).toEqual([expect.objectContaining({ runId: 'den-local', kind: 'denoise', chapterId: CHAPTER, total: 20 })])
    expect(await mock.cancelDenoiseLocal({ runId: 'den-local' })).toBe(true)
    const report = await answer
    expect(report.cancelled).toBe(true)
    expect(await mock.listJobs()).toEqual([])
  })

  it('reports a cloud denoise under the caller\'s run id, page by page, and stops it after the page in flight', async () => {
    const mock = createMockBackend({ timing: { method: 0, cloud: 400, provision: 0, analysis: 0, region: 40, pageTail: 0 } })
    await mock.writeSettings({ cloudEngines: 'allowed' })
    const plan = await mock.runCloudProvisioner({ op: 'plan', provider: 'modal',
      params: { credentials: MODAL_KEYS, installation_id: 'mc-jobs02', options: { denoise: true } } })
    await mock.runCloudProvisioner({ op: 'apply', provider: 'modal',
      params: { credentials: MODAL_KEYS, installation_id: 'mc-jobs02', approved_plan_hash: plan.data.plan_hash } })
    const recipe = DENOISE_PRESETS.find((entry) => entry.targets.includes('cloud')).recipe
    const proposal = await mock.prepareCloudDenoise({ chapterId: CHAPTER, recipe })
    const grant = await mock.confirmCloudDenoise({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })

    const seen = []
    await mock.onDenoiseProgress((event) => seen.push(event))
    const answer = mock.startCloudDenoise({ grantId: grant.grantId, outDir: '/tmp/out', runId: 'den-cloud' })
    await expect.poll(() => seen.some((event) => event.done >= 1)).toBe(true)
    expect(seen.every((event) => event.runId === 'den-cloud' && event.total === proposal.pages)).toBe(true)
    expect(seen.some((event) => event.page === 0.5)).toBe(true)
    // After a page `page` is 0 again; 1 comes only after the last page.
    expect(seen.filter((event) => event.page === 1)).toEqual([])
    expect(await mock.listJobs()).toEqual([expect.objectContaining({ runId: 'den-cloud', kind: 'cloudDenoise', chapterId: CHAPTER })])

    expect(await mock.cancelDenoiseLocal({ runId: 'den-cloud' })).toBe(true)
    const report = await answer
    expect(report.cancelled).toBe(true)
    // The page in flight when Stop landed is finished and saved.
    const lastDone = Math.max(...seen.map((event) => event.done))
    expect(report.written.length).toBe(lastDone)
    expect(report.written.length).toBeLessThan(proposal.pages)
    expect(await mock.listJobs()).toEqual([])
  })
})

describe('the quit guard', () => {
  it('raises the question with the running count, and confirmQuit stops every job', async () => {
    const mock = makeMock()
    const requests = []
    const unlisten = await mock.onQuitRequested((request) => requests.push(request))
    const run = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER })
    const end = ended(mock, run.runId)

    expect(mock.simulateQuitRequest({ canHide: true })).toBe(1)
    expect(requests).toEqual([{ jobs: 1, canHide: true }])
    expect(await mock.hideToTray()).toBe(false)

    await mock.confirmQuit()
    expect(await end).toMatchObject({ reason: 'cancelled' })
    expect(await mock.listJobs()).toEqual([])
    unlisten()
  })
})
