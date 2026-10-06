import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { editor, setScroll } from '../state/editor.svelte.js'
import { setBackend } from '../api/backend.js'
import { beginDraft, resetDraftState } from './draft.svelte.js'
import { commitDraft } from './drawing.svelte.js'

describe('DrawLayer focus and scroll safety', () => {
  it('calls surfaceEl.focus with preventScroll: true on pointerdown', () => {
    const filePath = resolve(import.meta.dirname, 'DrawLayer.svelte')
    const source = readFileSync(filePath, 'utf-8')
    expect(source).toMatch(/surfaceEl\?\.focus\(\s*\{\s*preventScroll:\s*true\s*\}\s*\)/)
  })

  it('calls element.focus with preventScroll: true in RegionLayer', () => {
    const filePath = resolve(import.meta.dirname, 'RegionLayer.svelte')
    const source = readFileSync(filePath, 'utf-8')
    expect(source).toMatch(/element\.focus\(\s*\{\s*preventScroll:\s*true\s*\}\s*\)/)
  })


  it('keeps viewport scroll position unchanged when an AI mask brush stroke creates and selects a region', async () => {
    editor.chapter = {
      id: 'c1',
      review: [],
      pages: [
        {
          id: 'c1-p001',
          index: 0,
          number: 1,
          status: 'unclean',
          width: 1600,
          height: 2400,
          regionCount: 0,
          regions: [],
        },
      ],
    }
    setScroll(450, 120)
    editor.tool = 'aiMaskBrush'
    editor.pageIndex = 0
    resetDraftState()

    setBackend(/** @type {any} */ ({
      historyPush: async () => ({ cursor: 1, entries: [] }),
      createRegion: async ({ bbox }) => ({
        region: { id: 'c1-p001-h1', pageId: 'c1-p001', bbox, source: 'hand', outcome: 'cleaned' },
        pageStatus: 'cleaned',
      }),
    }))

    beginDraft({
      tool: 'aiMaskBrush',
      kind: 'stroke',
      pageId: 'c1-p001',
      points: [{ x: 20, y: 30 }],
      bbox: { x: 20, y: 30, w: 12, h: 4 },
      mode: 'add',
      keyboard: false,
      moved: true,
    })

    const committed = await commitDraft()
    expect(committed).toBe(true)
    expect(editor.selectionId).toBe('c1-p001-h1')
    expect(editor.scroll).toEqual({ top: 450, left: 120 })

    setBackend(null)
    editor.chapter = null
    resetDraftState()
  })
})

/**
 * The region context menu is wired in three components and the wiring is the
 * whole of it - the entries it offers are `model/masks.js`'s and are tested
 * there, and what an entry does is `maskactions.svelte.js`'s. What can still
 * go wrong is a surface that forgets to raise it, or one that raises it and
 * lets a stroke start underneath. Source assertions, per this file's standing
 * pattern of not mounting components.
 */
describe('the region context menu is reachable from every surface', () => {
  /** @param {string} name @returns {string} */
  const read = (name) => readFileSync(resolve(import.meta.dirname, name), 'utf-8')

  it('is raised by the region buttons on the canvas, and selects first', () => {
    const source = read('RegionLayer.svelte')
    expect(source).toMatch(/oncontextmenu=\{\(event\) => onRegionMenu\(event, marker\.id\)\}/)
    expect(source).toMatch(/function onRegionMenu[\s\S]*?select\(regionId\)/)
    expect(source).toMatch(/<RegionMenu at=\{menu\}/)
  })

  it('is raised by the drawing surface, which the region buttons are under', () => {
    const source = read('DrawLayer.svelte')
    expect(source).toMatch(/function oncontextmenu[\s\S]*?select\(region\.id\)/)
    expect(source).toMatch(/<RegionMenu at=\{menu\}/)
  })

  it('never lets a non-left button start a stroke', () => {
    // The one invariant a right-click on the canvas depends on: `onpointerdown`
    // drops every button but the left one, so the menu cannot open on top of a
    // draft it just created.
    expect(read('DrawLayer.svelte')).toMatch(/function onpointerdown\(event\)\s*\{\s*\n\s*if \(event\.button !== 0\) return/)
  })

  it('is raised by a Layers row, over the same region', () => {
    const source = read('MaskRow.svelte')
    expect(source).toMatch(/function oncontextmenu[\s\S]*?select\(row\.id\)/)
    expect(source).toMatch(/<RegionMenu at=\{menu\}/)
  })

  it('runs the two calls the row already makes, and no third meaning of them', () => {
    const source = read('maskactions.svelte.js')
    expect(source).toMatch(/export async function runRegionMenuItem/)
    expect(source).toMatch(/if \(id === 'delete'\) return deleteRow\(region\)/)
    expect(source).toMatch(/rerunMask\(region, 'engine', engine\)/)
  })
})

/**
 * **Every shape the tool offers is drawable, and the surface is the only place
 * that can say so.** Source assertions, per this file's standing pattern of
 * not mounting components: what is checked is that each of the four kinds has
 * a route through the pointer handlers, that `Shift` constrains the two that
 * can be constrained, and that a polygon has all three of its endings.
 */
describe('the four shapes are all drawable', () => {
  /** @returns {string} */
  const source = () => readFileSync(resolve(import.meta.dirname, 'DrawLayer.svelte'), 'utf-8')

  it('drags a rectangle or an ellipse between two corners', () => {
    expect(source()).toMatch(/active\.kind === 'rect' \|\| active\.kind === 'ellipse'/)
    expect(source()).toMatch(/rectBetween\(active\.points\[0\], point\)/)
  })

  // A square in *percent* is an oblong on a page that is not square, so the
  // constraint is computed in page pixels and converted back per axis.
  it('constrains a rectangle to a square and an ellipse to a circle with Shift', () => {
    const text = source()
    expect(text).toMatch(/event\.shiftKey\s*\n?\s*\? squareBetween\(active\.points\[0\], point\)/)
    expect(text).toMatch(/function squareBetween/)
    expect(text).toMatch(/Math\.max\(\s*\(Math\.abs\(to\.x - from\.x\) \/ 100\) \* pageW/)
  })

  it('samples a lasso freehand and closes a polygon three ways', () => {
    const text = source()
    // The freehand path: every sample that clears the spacing test is a vertex.
    expect(text).toMatch(/shouldStamp\(active\.points\.at\(-1\)/)
    // Click back on the first vertex…
    expect(text).toMatch(/Math\.hypot\(point\.x - first\.x, point\.y - first\.y\) <= CLOSE_WITHIN/)
    // …double-click…
    expect(text).toMatch(/ondblclick=\{\(\) => committable\(\) && commit\(\)\}/)
    // …or Enter, which `draftKeyIntent` reads as a commit.
    expect(text).toMatch(/intent\.kind === 'commit'/)
    // And three vertices before any of the three is allowed to finish it.
    expect(text).toMatch(/polygonal \? \(active\.points\?\.length \?\? 0\) >= 3 : true/)
  })

  // Escape is deliberately not handled here: abandoning a gesture is the
  // editor's own `cancelInteraction`, whose first rung is `clearDraft`, and
  // routing it through the surface as well would give it two owners.
  it('leaves Escape to the editor, which drops the draft first', () => {
    expect(source()).not.toMatch(/'Escape'/)
    const editorSource = readFileSync(
      resolve(import.meta.dirname, '../state/editor.svelte.js'),
      'utf-8',
    )
    expect(editorSource).toMatch(/export function cancelInteraction\(\)[\s\S]*?if \(clearDraft\(\)\) return true/)
  })
})
