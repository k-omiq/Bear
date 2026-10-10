import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createMockBackend } from '../api/mock.js'
import { setBackend } from '../api/backend.js'
import { createHistory, settled } from '../model/history.js'
import { layerOf } from '../model/layers.js'
import { clearNotices } from '../state/app.svelte.js'
import { editor, redo, select, undo } from '../state/editor.svelte.js'
import { duplicateRegion, openLayerEdit, runRegionMenuItem } from './maskactions.svelte.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const ZERO = { method: 0, openChapter: 0, noticeStagger: 0 }

beforeEach(() => {
  editor.chapter = null
  editor.history = createHistory()
  editor.selectionId = null
  editor.run.active = false
  editor.areaRuns = []
})
afterEach(async () => {
  await settled(editor.history)
  editor.chapter = null
  setBackend(null)
  clearNotices()
  vi.restoreAllMocks()
})

async function setup() {
  const backend = createMockBackend({ timing: ZERO })
  setBackend(backend)
  const [page] = await backend.loadPages({ chapterId: CHAPTER, indices: [0] })
  const made = await backend.createRegion({ chapterId: CHAPTER, pageIndex: page.index,
    bbox: { x: 20, y: 20, w: 15, h: 15 }, tool: 'cloneStamp', params: { mode: 'region', shape: 'rect' } })
  const region = await backend.setLayerStyle({ regionId: made.region.id,
    layer: { opacity: 68, offsetX: 12, offsetY: 14, rotation: 25, feather: 9, edgeOpacity: 57, locked: false } })
  const pages = await backend.loadPages({ chapterId: CHAPTER, indices: [0] })
  editor.chapter = { id: CHAPTER, review: [], pages }
  select(region.id)
  return { backend, region, page: editor.chapter.pages[0] }
}

describe('duplicate regional copies', () => {
  it('selects an independent copy, preserves blending, and undoes/redoes only its creation', async () => {
    const { backend, region, page } = await setup()
    const original = JSON.parse(JSON.stringify(region))
    expect(await runRegionMenuItem('duplicate', region)).toBe(true)
    const id = editor.selectionId
    const copy = page.regions.find(region => region.id === id)
    expect(id).not.toBe(region.id)
    expect(copy.mask.id).not.toBe(region.mask.id)
    expect(layerOf(copy)).toEqual({ ...layerOf(region), offsetX: 28, offsetY: 30 })
    expect(page.regions.find(item => item.id === region.id)).toEqual(original)
    await settled(editor.history)
    expect(editor.history.entries.at(-1).label).toBe('regionCopy.duplicateUndo')
    undo()
    await settled(editor.history)
    expect(editor.chapter.pages[0].regions.some(item => item.id === id)).toBe(false)
    expect(editor.chapter.pages[0].regions.find(item => item.id === region.id)).toEqual(original)
    redo()
    await settled(editor.history)
    expect(editor.chapter.pages[0].regions.find(item => item.id === id)).toEqual(copy)
    await backend.setLayerStyle({ regionId: id, layer: { ...layerOf(copy), opacity: 20, rotation: 70 } })
    const saved = await backend.loadPages({ chapterId: CHAPTER, indices: [0] })
    expect(saved[0].regions.find(item => item.id === region.id)).toEqual(original)
  })

  it('waits for a pending blend write and prevents overlapping duplicate requests', async () => {
    const { backend, region } = await setup()
    const pending = openLayerEdit(region)
    pending.preview({ feather: 15, edgeOpacity: 40 })
    const call = vi.spyOn(backend, 'duplicateRegion')
    const first = duplicateRegion(region)
    expect(await duplicateRegion(region)).toBe(false)
    expect(await first).toBe(true)
    expect(call).toHaveBeenCalledTimes(1)
    const copy = editor.chapter.pages[0].regions.find(item => item.id === editor.selectionId)
    expect(layerOf(copy)).toMatchObject({ feather: 15, edgeOpacity: 40 })
    await pending.commit()
    expect(editor.history.entries.map(entry => entry.label)).toEqual(['masks.command.layerStyle', 'regionCopy.duplicateUndo'])
  })

  it('does nothing for other layers and busy pages, and leaves a switched chapter untouched', async () => {
    const { backend, region } = await setup()
    const call = vi.spyOn(backend, 'duplicateRegion')
    expect(await duplicateRegion({ id: 'other', mask: {} })).toBe(false)
    editor.run.active = true
    editor.run.busyPages = [0]
    expect(await duplicateRegion(region)).toBe(false)
    expect(call).not.toHaveBeenCalled()
    editor.run.active = false
    const pending = duplicateRegion(region)
    editor.chapter = { id: 'other-chapter', pages: [], review: [] }
    editor.selectionId = null
    expect(await pending).toBe(true)
    expect(editor.chapter.pages).toEqual([])
    expect(editor.selectionId).toBeNull()
  })
})
