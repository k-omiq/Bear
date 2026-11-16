/**
 * Make n variations in the editor, against the browser mock with every delay
 * zero: the run is one undo step whose undo shows the result before it and
 * whose redo shows the newest, and neither takes a variation off the list.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'

const asked = vi.hoisted(() => ({ backend: /** @type {any} */ (null) }))
// The window's answer: a grant for the requests it states, as the mock gives one.
vi.mock('./nanobananaask.svelte.js', () => ({
  confirmNanoBanana: async (/** @type {number} */ count, /** @type {string} */ chapterId) =>
    (await asked.backend.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, chapterId, count })).grant,
}))

import { setBackend } from '../api/backend.js'
import { createMockBackend } from '../api/mock.js'
import { createHistory, settled } from '../model/history.js'
import { DEFAULT_NANO_BANANA_MODEL } from '../model/nanobanana.js'
import { clearNotices } from '../state/app.svelte.js'
import { capabilities } from '../state/capabilities.svelte.js'
import { editor, redo, undo } from '../state/editor.svelte.js'
import { makeVariations } from './maskactions.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'
const KEY = 'AIzaSyTestKeyOnly0123456789'

afterEach(() => {
  clearNotices()
  capabilities.nanoBanana = false
  editor.chapter = null
  editor.project = null
  editor.history = createHistory()
  setBackend(null)
})

/** The region as the open chapter holds it now. @param {string} regionId @returns {any} */
function current(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const found = page.regions.find((/** @type {any} */ candidate) => candidate.id === regionId)
    if (found) return found
  }
  return null
}

/** A Nano Banana layer cleaned once on the mock's first page, open in the editor. */
async function layer() {
  const backend = /** @type {any} */ (createMockBackend({ timing: ZERO }))
  asked.backend = backend
  setBackend(backend)
  capabilities.nanoBanana = true
  await backend.writeSettings({ cloudEngines: 'allowed' })
  await backend.storeNanoBananaKey({ key: KEY })
  let done = () => {}
  const finished = new Promise((resolve) => { done = /** @type {any} */ (resolve) })
  const stop = backend.subscribe((/** @type {any} */ event) => { if (event.type === 'run-finished') done() })
  await backend.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect' })
  await finished
  stop()
  const [page] = await backend.loadPages({ chapterId: CHAPTER, indices: [0] })
  const first = page.regions.find((/** @type {any} */ region) => region.outcome === 'detected')
  const { grant } = await backend.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, chapterId: CHAPTER, count: 1 })
  await backend.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id,
    params: { engine: 'nanobanana', nanoBananaGrant: grant } })
  editor.project = /** @type {any} */ ({ id: 'pr', mode: 'pages' })
  editor.pageIndex = 0
  editor.chapter = /** @type {any} */ ({ id: CHAPTER, review: [], pages: await backend.loadPages({ chapterId: CHAPTER, indices: [0] }) })
  editor.history = createHistory()
  return { backend, regionId: first.id }
}

describe('Make n variations through the mock', () => {
  it('is one undo step, and undo and redo keep every variation listed', async () => {
    const { backend, regionId } = await layer()
    const before = current(regionId).mask.legacyPatchRevision
    expect(await makeVariations(current(regionId), 3)).toBe(3)
    await settled(editor.history)
    const newest = current(regionId).mask.legacyPatchRevision
    expect(newest).not.toBe(before)
    const live = editor.history.entries.filter((/** @type {any} */ entry) => !entry.undone)
    expect(live.at(-1)?.label).toBe('masks.command.makeVariations')
    expect(live.filter((/** @type {any} */ entry) => entry.label === 'masks.command.makeVariations')).toHaveLength(1)
    expect(live.filter((/** @type {any} */ entry) => entry.label === 'masks.command.rerunMask')).toEqual([])
    const listed = async () => (await backend.regionVariations({ regionId })).map((/** @type {any} */ v) => v.current)
    expect(await listed()).toEqual([false, false, false, true])

    undo()
    await settled(editor.history)
    expect(current(regionId).mask.legacyPatchRevision).toBe(before)
    expect(await listed()).toEqual([true, false, false, false])

    redo()
    await settled(editor.history)
    expect(current(regionId).mask.legacyPatchRevision).toBe(newest)
    expect(await listed()).toEqual([false, false, false, true])
  })
})
