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

it('shows the clone source only for Clone / heal on its sampled page', async () => {
  setTool('cloneHeal')
  setCloneSource('p1', { x: 25, y: 40 })
  const view = render(DraftPreview, { pageId: 'p1' })
  expect(view.container.querySelector('.source')).not.toBeNull()
  for (const tool of ['brush', 'shapes', 'maskSelect', 'autoClean']) {
    setTool(tool)
    await tick()
    expect(view.container.querySelector('.source')).toBeNull()
  }
  // Switching back preserves the sampled location for subsequent strokes.
  setTool('cloneHeal')
  await tick()
  expect(view.container.querySelector('.source circle').getAttribute('cx')).toBe('25')
  expect(draft.cloneSource).toEqual({ pageId: 'p1', x: 25, y: 40 })
  await view.rerender({ pageId: 'p2' })
  expect(view.container.querySelector('.source')).toBeNull()
})
