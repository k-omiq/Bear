/**
 * Clean on Nano Banana: the Run button starts a Nano Banana batch for a page
 * or a chapter, and nothing else. Detect & clean and Project are refused with
 * a notice, never run on this computer instead, and a run shortcut that asks
 * for Solid bubbles only is a clean here whatever Clean on says.
 *
 * The batch itself is `nanobananarun.svelte.test.js`'s; here it is a stub, so
 * these tests see only the routing.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { app } from '../state/app.svelte.js'
import { setCleanTarget } from '../state/session.svelte.js'
import { startCleanRun } from './cloudrun.js'
import { startNanoBananaRun } from './nanobananarun.svelte.js'
import { startRun } from '../state/editor.svelte.js'

vi.mock('./nanobananarun.svelte.js', () => ({ startNanoBananaRun: vi.fn(async () => 'nb-run') }))
vi.mock('../state/editor.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  startRun: vi.fn(async () => 'run-1'),
}))

function backend() {
  return /** @type {any} */ ({})
}

/** @param {string} key */
const noticed = (key) => app.notices.some((notice) => notice.key === key)

afterEach(() => {
  setCleanTarget('local')
  app.notices.length = 0
  vi.clearAllMocks()
})

describe('Clean on Nano Banana', () => {
  it('starts the batch for a page or a chapter, and no local run', async () => {
    setCleanTarget('nanoBanana')
    for (const scope of /** @type {const} */ (['page', 'chapter'])) {
      const api = backend()
      expect(await startCleanRun(scope, api, { step: 'clean' })).toBe('nb-run')
      expect(startRun).not.toHaveBeenCalled()
      expect(vi.mocked(startNanoBananaRun).mock.lastCall?.slice(0, 2)).toEqual([scope, api])
    }
  })

  it('refuses Detect & clean and a project, and runs nothing', async () => {
    setCleanTarget('nanoBanana')
    const api = backend()
    expect(await startCleanRun('chapter', api, { step: 'auto' })).toBeNull()
    expect(noticed('nanoBanana.target.cleanStepOnly')).toBe(true)
    expect(await startCleanRun('project', api, { step: 'clean' })).toBeNull()
    expect(noticed('nanoBanana.target.scopeProject')).toBe(true)
    expect(startNanoBananaRun).not.toHaveBeenCalled()
    expect(startRun).not.toHaveBeenCalled()
  })

  it('leaves Detect alone, and a Solid bubbles only shortcut cleans here', async () => {
    setCleanTarget('nanoBanana')
    const api = backend()
    expect(await startCleanRun('page', api, { step: 'detect' })).toBe('run-1')
    expect(vi.mocked(startRun).mock.calls[0][1]).toMatchObject({ mode: 'detect' })
    expect(await startCleanRun('page', api, { step: 'auto', solidOnly: true })).toBe('run-1')
    expect(vi.mocked(startRun).mock.calls[1][1]).toMatchObject({ mode: 'auto', solidOnly: true })
    expect(startNanoBananaRun).not.toHaveBeenCalled()
  })
})
