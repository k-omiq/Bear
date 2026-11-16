/**
 * The browser mock's variations, held to what `variations.rs` and
 * `library.rs#pick_variation` decide: one per answer a Nano Banana, cloud or
 * local FLUX result had, the newest result of each, in the order they were
 * first made; a finishing change is no new one; a pick renders nothing,
 * loses nothing and is one `region-state` step whose undo puts the earlier
 * one back.
 */
import { describe, expect, it } from 'vitest'

import { createMockBackend } from './mock.js'
import { DEFAULT_NANO_BANANA_MODEL } from '../model/nanobanana.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const KEY = 'AIzaSyTestKeyOnly0123456789'
const ZERO = { method: 0, cloud: 0, provision: 0, analysis: 0, region: 0, pageTail: 0, noticeStagger: 0 }

/** @param {any} mock */
async function detected(mock) {
  let done = () => {}
  const finished = new Promise((resolve) => { done = resolve })
  const stop = mock.subscribe((/** @type {any} */ event) => { if (event.type === 'run-finished') done() })
  const handle = await mock.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect' })
  if (handle?.runId) await finished
  stop()
  const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
  return page.regions.filter((/** @type {any} */ region) => region.outcome === 'detected')
}

/** A Nano Banana layer, cleaned once, with a grant for `count` more requests. */
async function layer(count = 3) {
  const mock = /** @type {any} */ (createMockBackend({ timing: ZERO }))
  await mock.writeSettings({ cloudEngines: 'allowed' })
  await mock.createNanoBananaProfile({ name: 'Work', model: DEFAULT_NANO_BANANA_MODEL, key: KEY })
  const [first] = await detected(mock)
  const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, chapterId: CHAPTER, count: count + 1 })
  const done = await mock.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id,
    params: { engine: 'nanobanana', nanoBananaGrant: grant } })
  return { mock, grant, regionId: first.id, maskId: done.mask?.id ?? done.region.mask.id }
}

/** @param {any} region */
const answerOf = (region) => region.mask.provenance.params_snapshot.nano_banana.answer_sha256

describe('a region\'s variations on the mock', () => {
  it('are one per Nano Banana answer, in the order they were made, the live one current', async () => {
    const { mock, grant, regionId, maskId } = await layer()
    expect((await mock.regionVariations({ regionId })).map((/** @type {any} */ v) => v.current)).toEqual([true])
    let id = maskId
    for (let at = 0; at < 2; at += 1) {
      const again = await mock.rerunMask({ maskId: id, kind: 'retry', params: { nanoBananaGrant: grant } })
      id = again.mask.id
    }
    const listed = await mock.regionVariations({ regionId })
    expect(listed.map((/** @type {any} */ v) => [v.engine, v.current])).toEqual([
      ['nanobanana', false], ['nanobanana', false], ['nanobanana', true],
    ])
    expect(new Set(listed.map((/** @type {any} */ v) => v.revision)).size).toBe(3)
    expect(listed[0]).not.toHaveProperty('region')
  })

  it('count a finishing change as the answer it finishes, shown as changed', async () => {
    const { mock, grant, regionId, maskId } = await layer()
    await mock.rerunMask({ maskId, kind: 'retry', params: { nanoBananaGrant: grant } })
    const before = await mock.regionVariations({ regionId })
    const changed = await mock.setRegionFinishing({ regionId, finishing: 'poisson', grayscale: false })
    const after = await mock.regionVariations({ regionId })
    expect(after).toHaveLength(2)
    expect(after[1].current).toBe(true)
    expect(after[1].revision).toBe(changed.region.mask.legacyPatchRevision)
    expect(after[1].revision).not.toBe(before[1].revision)
  })

  it('are picked with no render, nothing lost, as one step whose undo puts the earlier one back', async () => {
    const { mock, grant, regionId, maskId } = await layer()
    const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
    const firstAnswer = answerOf(page.regions.find((/** @type {any} */ region) => region.id === regionId))
    const again = await mock.rerunMask({ maskId, kind: 'retry', params: { nanoBananaGrant: grant } })
    const lastAnswer = answerOf(again.region)
    const listed = await mock.regionVariations({ regionId })

    const picked = await mock.pickVariation({ regionId, revision: listed[0].revision })
    expect(answerOf(picked.region)).toBe(firstAnswer)
    expect(picked.history.entries.at(-1)).toMatchObject({ label: 'masks.command.pickVariation', page: 0 })
    const now = await mock.regionVariations({ regionId })
    expect(now.map((/** @type {any} */ v) => [v.revision, v.current]))
      .toEqual(listed.map((/** @type {any} */ v, /** @type {number} */ at) => [v.revision, at === 0]))

    // The one it shows writes nothing; one that is none of them is refused.
    expect((await mock.pickVariation({ regionId, revision: listed[0].revision })).history).toBeNull()
    expect(await mock.pickVariation({ regionId, revision: 'no-such-revision' })).toBeNull()

    const step = await mock.historyMove({ chapterId: CHAPTER, pageIndex: 0, direction: 'undo' })
    expect(step.entry).toMatchObject({ op: 'region-state', regionId, label: 'masks.command.pickVariation' })
    const back = await mock.restoreRegion({ regionId, region: step.entry.before.region })
    expect(answerOf(back)).toBe(lastAnswer)
  })

  // LaMa answers one hole one way, so a LaMa result's variations are one per
  // hole: the first clean, then each Try again wider. A plain re-run of the
  // same hole is the same answer.
  it('are one per hole for a LaMa result: the first clean, then each Try again wider', async () => {
    const mock = /** @type {any} */ (createMockBackend({ timing: ZERO }))
    const [first] = await detected(mock)
    const done = await mock.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id,
      params: { engine: 'lama' } })
    expect(done.region.mask.provenance.engine).toBe('lama')
    const shape = async () => (await mock.regionVariations({ regionId: first.id }))
      .map((variation) => [variation.engine, variation.current])
    expect(await shape()).toEqual([['lama', true]])
    // Every re-run gives the layer a new mask id.
    let maskId = done.region.mask.id
    const again = async (/** @type {string} */ kind) => {
      maskId = (await mock.rerunMask({ maskId, kind })).region.mask.id
    }
    await again('retry')
    expect(await shape()).toEqual([['lama', true]])
    await again('retryWider')
    expect(await shape()).toEqual([['lama', false], ['lama', true]])
    await again('retryWider')
    expect(await shape()).toEqual([['lama', false], ['lama', false], ['lama', true]])

    // Picking the first shows it again, one undo step, and loses none; the
    // next wider try from it is the second hole again, not a fourth answer.
    const listed = await mock.regionVariations({ regionId: first.id })
    const picked = await mock.pickVariation({ regionId: first.id, revision: listed[0].revision })
    expect(picked.history).not.toBeNull()
    expect(await shape()).toEqual([['lama', true], ['lama', false], ['lama', false]])
    maskId = picked.region.mask.id
    await again('retryWider')
    expect(await shape()).toEqual([['lama', false], ['lama', true], ['lama', false]])
  })
})

/**
 * Run to its end. Answers the run's `run-finished` and the Nano Banana
 * report, when there is one.
 *
 * @param {any} mock @param {() => Promise<any>} start
 */
async function ran(mock, start) {
  /** @type {any} */
  let report = null
  let done = (/** @type {any} */ _event) => {}
  const finished = new Promise((resolve) => { done = resolve })
  const stop = mock.subscribe((/** @type {any} */ event) => {
    if (event.type === 'nano-banana-report') report = event.report
    if (event.type === 'run-finished') done(event)
  })
  const handle = await start()
  const summary = handle?.runId ? await finished : null
  stop()
  return { summary, report, handle }
}

describe('making a variation is no undo step', () => {
  it('Try again then undo keeps the new variation listed, not current', async () => {
    const { mock, grant, regionId, maskId } = await layer()
    const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
    const before = page.regions.find((/** @type {any} */ region) => region.id === regionId)
    const again = await mock.rerunMask({ maskId, kind: 'retry', params: { nanoBananaGrant: grant } })
    // The step the interface records for a Try again, and its undo as it replays it.
    await mock.historyPush({ chapterId: CHAPTER, entry: { label: 'masks.command.rerunMask', op: 'region-state', regionId,
      before: { present: true, pageStatus: 'cleaned', region: before }, after: { present: true, pageStatus: 'cleaned', region: again.region } } })
    const step = await mock.historyMove({ chapterId: CHAPTER, pageIndex: 0, direction: 'undo' })
    await mock.restoreRegion({ regionId, region: step.entry.before.region, pageStatus: step.entry.before.pageStatus })
    const listed = await mock.regionVariations({ regionId })
    expect(listed.map((/** @type {any} */ v) => v.current)).toEqual([true, false])
    expect(listed[1].revision).toBe(again.region.mask.legacyPatchRevision)
    // Redo shows it again; neither direction takes one off.
    const redone = await mock.historyMove({ chapterId: CHAPTER, pageIndex: 0, direction: 'redo' })
    await mock.restoreRegion({ regionId, region: redone.entry.after.region, pageStatus: redone.entry.after.pageStatus })
    expect((await mock.regionVariations({ regionId })).map((/** @type {any} */ v) => v.current)).toEqual([false, true])
  })
})

describe('Generations per region on the mock', () => {
  /** A detected page, then a clean on local FLUX at `generations`. */
  async function fluxClean(generations = 1) {
    const mock = /** @type {any} */ (createMockBackend({ timing: ZERO }))
    await mock.writeSettings({ generationsPerRegion: generations })
    const { summary } = await ran(mock, () => mock.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'auto',
      bubbleEngine: 'flux', outsideEngine: 'flux' }))
    const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
    const history = await mock.historyLoad({ chapterId: CHAPTER })
    return { mock, summary, page, history }
  }

  it('a local FLUX clean at 3 leaves three variations per region, the first applied, the steps as at 1', async () => {
    const one = await fluxClean(1)
    const three = await fluxClean(3)
    const flux = three.page.regions.filter((/** @type {any} */ region) => region.mask?.provenance?.engine === 'flux')
    expect(flux.length).toBeGreaterThan(0)
    for (const region of flux) {
      const listed = await three.mock.regionVariations({ regionId: region.id })
      expect(listed.map((/** @type {any} */ v) => v.current)).toEqual([true, false, false])
      expect(region.mask.provenance.params_snapshot.flux_seed).toBe(1)
      const alone = await one.mock.regionVariations({ regionId: region.id })
      expect(alone).toHaveLength(1)
    }
    expect(three.summary.regionsCleaned ?? three.summary.regions_cleaned).toEqual(one.summary.regionsCleaned ?? one.summary.regions_cleaned)
    expect(three.history.entries.map((/** @type {any} */ entry) => [entry.label, entry.page]))
      .toEqual(one.history.entries.map((/** @type {any} */ entry) => [entry.label, entry.page]))
  })

  it('is refused out of range, and a fill clean ignores it', async () => {
    const mock = /** @type {any} */ (createMockBackend({ timing: ZERO }))
    await mock.writeSettings({ generationsPerRegion: 3 })
    await detected(mock)
    await ran(mock, () => mock.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'clean',
      bubbleEngine: 'fill', outsideEngine: 'fill' }))
    const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
    for (const region of page.regions.filter((/** @type {any} */ r) => r.mask && r.outcome !== 'detected')) {
      expect(await mock.regionVariations({ regionId: region.id })).toEqual([])
    }
    await expect(mock.writeSettings({ generationsPerRegion: 9 })).rejects.toThrow('generationsPerRegion')
    await expect(mock.writeSettings({ generationsPerRegion: 0 })).rejects.toThrow('generationsPerRegion')
  })

  it('a Nano Banana batch at 2 spends two requests per region and keeps two variations of each', async () => {
    const mock = /** @type {any} */ (createMockBackend({ timing: ZERO }))
    await mock.writeSettings({ cloudEngines: 'allowed' })
    await mock.createNanoBananaProfile({ name: 'Work', model: DEFAULT_NANO_BANANA_MODEL, key: KEY })
    const regions = await detected(mock)
    const ids = regions.map((/** @type {any} */ region) => region.id)
    // A grant for one request per region does not cover two generations.
    const short = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, chapterId: CHAPTER, count: ids.length })
    await expect(mock.startNanoBananaRun({ chapterId: CHAPTER, regionIds: ids, nanoBananaGrant: short.grant, generations: 2 }))
      .rejects.toThrow('decline.reason.nanoBananaNoConsent')
    await expect(mock.startNanoBananaRun({ chapterId: CHAPTER, regionIds: ids, nanoBananaGrant: short.grant, generations: 9 }))
      .rejects.toThrow('generationsPerRegion')
    const before = (await mock.historyLoad({ chapterId: CHAPTER })).entries.length
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, chapterId: CHAPTER, count: ids.length * 2 })
    const { report, summary } = await ran(mock, () => mock.startNanoBananaRun({ chapterId: CHAPTER, regionIds: ids,
      nanoBananaGrant: grant, generations: 2 }))
    expect(summary.reason).toBe('completed')
    expect(report).toMatchObject({ total: ids.length, cleaned: ids.length, generationsNotMade: 0 })
    for (const id of ids) {
      const listed = await mock.regionVariations({ regionId: id })
      expect(listed.map((/** @type {any} */ v) => [v.engine, v.current])).toEqual([['nanobanana', true], ['nanobanana', false]])
    }
    // The batch is still one undo step; the variations add none.
    expect((await mock.historyLoad({ chapterId: CHAPTER })).entries.length).toBe(before + 1)
  })
})
