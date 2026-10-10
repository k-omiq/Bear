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
  it('queues multiple selected areas on one page and stops each independently', async () => {
    const mock = makeMock()
    const area = { x: 10, y: 20, w: 15, h: 25 }
    const spec = { scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect', area }
    const first = await mock.runClean(spec)
    const second = await mock.runClean(spec)
    expect(first.runId).toBeTruthy()
    expect(second.runId).toBeTruthy()
    expect(second.runId).not.toBe(first.runId)
    expect(await mock.listJobs()).toEqual(expect.arrayContaining([
      expect.objectContaining({ runId: first.runId, area: true, areaPageIndex: 0 }),
      expect.objectContaining({ runId: second.runId, area: true, areaPageIndex: 0 }),
    ]))
    const firstEnd = ended(mock, first.runId)
    expect(await mock.cancelRun({ runId: first.runId })).toBe(first.runId)
    expect(await firstEnd).toMatchObject({ reason: 'cancelled' })
    expect((await mock.listJobs()).map((job) => job.runId)).toEqual([second.runId])
    const blocked = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER })
    expect(blocked.alreadyRunning).toBe(true)
    await mock.cancelRun({ runId: second.runId })
  })

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

  // A run holds the page it is on and no other (`run.rs`): an edit of that
  // page is refused with `page_busy`, an edit of any other page lands, and a
  // write over the whole chapter waits for the run to leave the chapter.
  it('refuses an edit of the page a run is on, and takes one on another page', async () => {
    const mock = makeMock()
    const started = new Promise((resolve) => {
      const stop = mock.subscribe((event) => {
        if (event.type === 'page-started') {
          stop()
          resolve(event.pageIndex)
        }
      })
    })
    const run = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER, mode: 'detect' })
    const busy = await started
    const free = busy + 1
    expect((await mock.listJobs()).find((job) => job.runId === run.runId)?.pages).toEqual([busy])

    await expect(mock.setDetectionPadding({ chapterId: CHAPTER, pageIndex: busy, paddingPx: 4 })).rejects.toThrow('page_busy')
    await expect(mock.createRegion({ chapterId: CHAPTER, pageIndex: busy, bbox: { x: 10, y: 10, w: 10, h: 10 }, tool: 'shapes', params: { mode: 'solid' } }))
      .rejects.toThrow('page_busy')
    await expect(mock.setDetectionPadding({ chapterId: CHAPTER, paddingPx: 4 })).rejects.toThrow('run_in_progress')
    // Another page of the same chapter is the reader's to edit.
    await expect(mock.setDetectionPadding({ chapterId: CHAPTER, pageIndex: free, paddingPx: 4 })).resolves.toBeTruthy()
    const made = await mock.createRegion({ chapterId: CHAPTER, pageIndex: free, bbox: { x: 10, y: 10, w: 10, h: 10 }, tool: 'shapes', params: { mode: 'solid' } })
    expect(made?.region ?? made).toBeTruthy()

    const end = ended(mock, run.runId)
    await mock.cancelRun({ runId: run.runId })
    await end
    // The run gone, the page it was on is free again.
    await expect(mock.setDetectionPadding({ chapterId: CHAPTER, pageIndex: busy, paddingPx: 4 })).resolves.toBeTruthy()
    expect(await mock.listJobs()).toEqual([])
  })

  // The journal is the chapter's, and a step of it writes one page. A step
  // onto the page a run is on is refused before the cursor moves
  // (`history.rs#history_move`), and so is the restore it would have sent.
  it('refuses an undo onto the page a run is on, and leaves the cursor', async () => {
    const mock = makeMock()
    const made = await mock.createRegion({ chapterId: CHAPTER, pageIndex: 0, bbox: { x: 10, y: 10, w: 10, h: 10 }, tool: 'shapes', params: { mode: 'solid' } })
    const region = made?.region ?? made
    await mock.historyPush({
      chapterId: CHAPTER,
      entry: { op: 'region-state', label: 'history.label.shape', regionId: region.id, before: null, after: region },
    })
    const started = new Promise((resolve) => {
      const stop = mock.subscribe((event) => {
        if (event.type === 'page-started') {
          stop()
          resolve(event.pageIndex)
        }
      })
    })
    const run = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER, mode: 'detect' })
    expect(await started).toBe(0)

    await expect(mock.historyMove({ chapterId: CHAPTER, direction: 'undo' })).rejects.toThrow('page_busy')
    expect((await mock.historyLoad({ chapterId: CHAPTER })).cursor).toBe(1)
    await expect(mock.restoreRegion({ regionId: region.id, region: null })).rejects.toThrow('page_busy')
    const held = (await mock.loadPages({ chapterId: CHAPTER, indices: [0] }))[0]
    expect(held.regions.some((candidate) => candidate.id === region.id)).toBe(true)

    const end = ended(mock, run.runId)
    await mock.cancelRun({ runId: run.runId })
    await end
    expect((await mock.historyMove({ chapterId: CHAPTER, direction: 'undo' })).cursor).toBe(0)
  })

  // A resumed run cleans under the rule the cancelled one had (`run.rs` keeps
  // `runSolidBubblesOnly` in its snapshot).
  it('resumes a cancelled Solid bubbles only run as one', async () => {
    const mock = makeMock()
    const started = new Promise((resolve) => {
      const stop = mock.subscribe((event) => {
        if (event.type === 'page-started') {
          stop()
          resolve(event.pageIndex)
        }
      })
    })
    const run = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER, mode: 'auto', solidBubblesOnly: true })
    await started
    const end = ended(mock, run.runId)
    await mock.cancelRun({ runId: run.runId })
    await end

    /** @type {any[]} */
    const notices = []
    const stop = mock.subscribe((event) => {
      if (event.type === 'notice') notices.push(event.key)
    })
    const resumed = await mock.resumeJob({ projectId: 'tsuki-to-hane', chapterId: CHAPTER })
    expect(resumed?.runId).toBeTruthy()
    await ended(mock, resumed.runId)
    stop()
    expect(notices).toContain('notice.run.solidOnly')
    const pages = await mock.loadPages({ chapterId: CHAPTER, indices: [1] })
    // The sound effect on the second page is still a detection.
    expect(pages[0].regions.some((region) => region.kind !== 'bubble' && region.outcome === 'detected')).toBe(true)
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
