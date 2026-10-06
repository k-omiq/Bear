import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { tick } from 'svelte'
import CanvasStage from './CanvasStage.svelte'
import KeyboardLayer from '../shell/KeyboardLayer.svelte'
import { editor } from '../state/editor.svelte.js'
import { app } from '../state/app.svelte.js'
import { session } from '../state/session.svelte.js'
import { setBackend } from '../api/backend.js'

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.project = null
  editor.stripScrollRequest = null
  session.readingDirection = 'rtl'
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

describe('repeated keyboard page turns', () => {
  it.each([
    ['single', 'rtl'], ['single', 'ltr'], ['longstrip', 'rtl'], ['longstrip', 'ltr'],
  ])('passes the second page in %s/%s mode and returns', async (mode, direction) => {
    editor.project = { id: 'p1', mode, readingDirection: 'rtl' }
    editor.chapter = {
      id: 'c1', review: [],
      pages: Array.from({ length: 5 }, (_, index) => ({
        id: `c1-p${index}`, chapterId: 'c1', index, number: index + 1,
        width: 800, height: 1600, status: 'unclean', regions: [], resident: true,
      })),
    }
    editor.loading = false
    editor.pageIndex = 0
    editor.fit = false
    editor.zoom = 1
    editor.stripScope = []
    app.route = { name: 'editor', projectId: 'p1', chapterId: 'c1' }
    app.modals.length = 0
    session.readingDirection = direction
    vi.stubGlobal('__TAURI_INTERNALS__', { convertFileSrc: () => 'tile://localhost/' })
    setBackend({ loadPages: vi.fn(async ({ indices }) => indices.map((i) => ({
      ...editor.chapter.pages[i], resident: true,
    }))) })

    // jsdom has no layout; emulate a real scroller whose column moves with it.
    const viewport = document.createElement('div')
    Object.defineProperties(viewport, {
      clientWidth: { value: 1000 }, clientHeight: { value: 600 },
    })
    document.body.append(viewport)
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function () {
      return { top: this.classList.contains('stage') ? -viewport.scrollTop : 0,
        left: 0, width: 800, height: 1600, right: 800, bottom: 1600 }
    })
    render(CanvasStage, { target: viewport })
    render(KeyboardLayer)
    await tick()

    let previousImage = viewport.querySelector('img')
    for (let index = 1; index < 5; index++) {
      await fireEvent.keyDown(window, { key: direction === 'rtl' ? 'ArrowLeft' : 'ArrowRight', repeat: index > 1 })
      await tick()
      expect(editor.pageIndex).toBe(index)
      if (mode === 'longstrip') expect(viewport.scrollTop).toBe(index * 1600)
      else {
        const image = viewport.querySelector('.paginated:not([hidden]) img')
        expect(image.src).toContain(`/c1/${index}/source/0?`)
        expect(image).not.toBe(previousImage)
        previousImage = image
      }
    }
    for (let index = 3; index >= 0; index--) {
      await fireEvent.keyDown(window, { key: direction === 'rtl' ? 'ArrowRight' : 'ArrowLeft' })
      await tick()
      expect(editor.pageIndex).toBe(index)
    }
    viewport.remove()
  })
})
