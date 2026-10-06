import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, render, fireEvent } from '@testing-library/svelte'
import { tick } from 'svelte'
import EditorScreen from './EditorScreen.svelte'
import KeyboardLayer from '../shell/KeyboardLayer.svelte'
import { editor } from '../state/editor.svelte.js'
import { app } from '../state/app.svelte.js'
import { session } from '../state/session.svelte.js'
import { setBackend } from '../api/backend.js'

const region = (pageId, id, mask = null) => ({ id, pageId, bbox: { x: 20, y: 30, w: 40, h: 20 }, detected: true,
  source: 'auto', outcome: mask ? 'cleaned' : 'declined', mask })
const cloud = { id: 'legacy-patch-m1', regionId: 'legacy-patch', sequence: 1, fillMode: 'reconstruct', elapsedMs: 0,
  fittingReconstructed: false, cloudOutcome: null, provenance: { engine: 'flux', engine_version: '1.0',
  mask_sha256: 'm', source_sha256: 's', created: '2026-10-02', execution_provider: 'cloud',
  params_snapshot: {}, cloud: { provider: 'modal', model: 'legacy-model', request_id: 'request', cost: null, tier: null } } }
afterEach(() => { cleanup(); setBackend(null); editor.chapter = null; editor.project = null; vi.restoreAllMocks(); vi.unstubAllGlobals() })
it('updates canvas, layers and page readout together when returning to a legacy cloud patch', async () => {
  const loaded = Array.from({ length: 5 }, (_, index) => {
    const id = `c-p${index}`
    return { id, chapterId: 'c', index, number: index + 1, file: `${index}.png`, width: 1125, height: 1600,
      sourceSha: `sha-${index}`, status: 'cleaned', resident: true,
      regions: index === 0 ? [region(id, 'legacy-patch', cloud), ...Array.from({length:4},(_,i)=>region(id,`p0-d${i}`))]
        : Array.from({length:3},(_,i)=>region(id,`p${index}-d${i}`)), regionCount: index===0?5:3, doneCount:index===0?1:0, reviewCount:3 }
  })
  editor.project = { id: 'p', mode: 'single', readingDirection: 'rtl', chapters: [] }
  editor.chapter = { id: 'c', title: 'Chapter', review: [], pages: loaded.map(p=>({...p, regions: [...p.regions]})) }
  editor.loading = false; editor.pageIndex = 0; editor.fit = false; editor.zoom = 1; editor.stripScope = []
  app.route = { name: 'editor', projectId: 'p', chapterId: 'c' }; app.modals.length = 0
  session.readingDirection = 'rtl'; session.windows.pages.open = true; session.windows.layers.open = true
  vi.stubGlobal('__TAURI_INTERNALS__', { convertFileSrc: () => 'tile://localhost/' })
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ top:0,left:0,right:1125,bottom:1600,width:1125,height:1600 })
  HTMLElement.prototype.scrollTo = vi.fn()
  setBackend({ loadPages: vi.fn(async ({indices}) => {await Promise.resolve(); return indices.map(i=>structuredClone(loaded[i]))}) })
  const { container } = render(EditorScreen); render(KeyboardLayer); await tick()
  for (const key of ['ArrowLeft','ArrowLeft','ArrowLeft','ArrowRight','ArrowRight','ArrowRight']) {
    await fireEvent.keyDown(window, { key }); await Promise.resolve(); await tick()
    const current = editor.pageIndex
    expect(container.querySelector(`[title="Page ${current+1} of 5"]`)).not.toBeNull()
    expect(container.querySelector('.paginated:not([hidden]) [data-artwork="source"] img').src).toContain(`/c/${current}/source/`)
  }
  expect(editor.pageIndex).toBe(0)
  expect(container.querySelectorAll('[data-mask-row]').length).toBe(5)
  expect(container.querySelector('[data-mask-row="legacy-patch"]')).not.toBeNull()
})

it('measures a saved scroll from the page at rest, and restores it only once the pages are there', async () => {
  const pages = Array.from({ length: 2 }, (_, index) => ({ id: `c-p${index}`, chapterId: 'c', index, number: index + 1,
    file: `${index}.png`, width: 1125, height: 1600, sourceSha: `sha-${index}`, status: 'unclean', resident: true,
    regions: [], regionCount: 0, doneCount: 0, reviewCount: 0 }))
  editor.project = { id: 'p', mode: 'single', readingDirection: 'rtl', chapters: [] }
  editor.chapter = { id: 'c', title: 'Chapter', review: [], pages }
  editor.loading = true; editor.pageIndex = 0; editor.fit = false; editor.zoom = 1; editor.stripScope = []
  editor.scroll = { top: 250, left: 40 }
  app.route = { name: 'editor', projectId: 'p', chapterId: 'c' }; app.modals.length = 0
  const scrollTo = HTMLElement.prototype.scrollTo = vi.fn()
  setBackend({ loadPages: vi.fn(async ({ indices }) => indices.map((i) => structuredClone(pages[i]))) })
  const { container } = render(EditorScreen); await tick()
  const viewport = /** @type {HTMLElement} */ (container.querySelector('.viewport'))

  // The placeholder is neither scrolled to the saved place nor recorded.
  expect(scrollTo).not.toHaveBeenCalled()
  viewport.scrollTop = 7
  await fireEvent.scroll(viewport)
  expect(editor.scroll).toEqual({ top: 250, left: 40 })

  editor.scrollRoom = 300
  editor.loading = false
  await tick()
  expect(scrollTo).toHaveBeenCalledWith({ top: 550, left: 40 })

  // Pulled down into the room above the page: above rest is negative.
  viewport.scrollTop = 100
  await fireEvent.scroll(viewport)
  expect(editor.scroll.top).toBe(-200)
})
