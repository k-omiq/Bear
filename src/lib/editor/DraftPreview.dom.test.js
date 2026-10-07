import { afterEach, expect, it } from 'vitest'
import { cleanup, render } from '@testing-library/svelte'
import { tick } from 'svelte'
import DraftPreview from './DraftPreview.svelte'
import { editor, setTool } from '../state/editor.svelte.js'
import { draft, resetDraftState, setCloneSource } from './draft.svelte.js'

afterEach(() => {
  cleanup()
  resetDraftState()
  editor.tool = 'autoClean'
})

// The stamp's marks are `CloneSource.svelte`'s. The draft preview draws the
// mask a gesture is about to make, and a clone stroke previews as pixels.
it('draws no mark for the clone source, and keeps the source across tools', async () => {
  setTool('cloneStamp')
  setCloneSource('p1', { x: 25, y: 40 })
  const view = render(DraftPreview, { pageId: 'p1' })
  expect(view.container.querySelector('.source')).toBeNull()
  for (const tool of ['brush', 'shapes', 'maskSelect', 'autoClean', 'cloneStamp']) {
    setTool(tool)
    await tick()
  }
  // Switching away and back keeps the sampled location for later strokes.
  expect(draft.cloneSource).toEqual({ pageId: 'p1', x: 25, y: 40 })
})
