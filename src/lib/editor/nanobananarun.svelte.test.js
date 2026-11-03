import { beforeEach, describe, expect, it, vi } from 'vitest'

const stores = vi.hoisted(() => ({
  cleanAnyway: vi.fn(),
  notify: vi.fn(),
  modals: [],
  claim: vi.fn(() => ({})),
  release: vi.fn(),
}))

vi.mock('./maskactions.svelte.js', () => ({ cleanAnyway: stores.cleanAnyway }))
vi.mock('../state/app.svelte.js', () => ({
  notify: stores.notify,
  pushModal: (spec) => stores.modals.push(spec),
}))
vi.mock('../state/editor.svelte.js', () => {
  const editor = {
    chapter: { id: 'chapter' },
    pageIndex: 0,
    pages: [
      { index: 0, status: 'detected' },
      { index: 1, status: 'detected' },
      { index: 2, status: 'pending' },
    ],
  }
  return { editor, pages: () => editor.pages, claimRunStart: stores.claim, releaseRunStart: stores.release }
})

import { nanoBananaRun, startNanoBananaRun, stopNanoBananaRun } from './nanobananarun.svelte.js'
import { confirmNanoBanana } from './nanobananaask.svelte.js'

const region = (id, outcome = 'detected') => ({ id, outcome })

function backendWith(pagesByIndex) {
  return {
    readSettings: vi.fn(async () => ({ nanoBananaPrompt: 'keep the hatching' })),
    writeSettings: vi.fn(async () => ({})),
    loadPages: vi.fn(async ({ indices }) => indices.map((index) => ({ index, regions: pagesByIndex[index] ?? [] }))),
  }
}

/** Answer the question as soon as it is asked. A string stands for the button pressed, with the box as it was. */
async function answerWhenAsked(answer) {
  await vi.waitFor(() => expect(stores.modals.length).toBeGreaterThan(0))
  const spec = stores.modals.pop()
  spec.onresolve(typeof answer === 'string' ? { id: answer, prompt: spec.props.prompt } : answer)
  return spec
}

beforeEach(() => {
  vi.clearAllMocks()
  stores.modals.length = 0
  stores.cleanAnyway.mockResolvedValue(true)
  stores.claim.mockReturnValue({})
})

describe('a Nano Banana batch', () => {
  it('sends nothing and asks nothing when nothing is detected', async () => {
    const backend = backendWith({})
    expect(await startNanoBananaRun('chapter', backend)).toBe(0)
    expect(stores.modals).toEqual([])
    expect(stores.cleanAnyway).not.toHaveBeenCalled()
    expect(stores.notify).toHaveBeenCalledWith(expect.objectContaining({ key: 'notice.run.nothingDetected' }))
  })

  it('reads the chapter from the backend, because the editor holds only nearby pages', async () => {
    const backend = backendWith({ 0: [region('a'), region('kept', 'cleaned')], 1: [region('b')] })
    const started = startNanoBananaRun('chapter', backend)
    const spec = await answerWhenAsked('confirm')
    expect(spec.props).toEqual({ count: 2, size: '512', model: 'gemini-3.1-flash-image', prompt: 'keep the hatching' })
    expect(await started).toBe(2)
    expect(backend.loadPages).toHaveBeenCalledWith({ chapterId: 'chapter', indices: [0, 1] })
    expect(stores.cleanAnyway.mock.calls.map(([target, engine]) => [target.id, engine])).toEqual([
      ['a', 'nanobanana'],
      ['b', 'nanobanana'],
    ])
  })

  it('holds a page batch to the page on screen', async () => {
    const backend = backendWith({ 0: [region('a')], 1: [region('b')] })
    const started = startNanoBananaRun('page', backend)
    await answerWhenAsked('confirm')
    await started
    expect(stores.cleanAnyway.mock.calls.map(([target]) => target.id)).toEqual(['a'])
  })

  it('sends nothing on Cancel, and frees the run slot', async () => {
    const backend = backendWith({ 0: [region('a')] })
    const started = startNanoBananaRun('chapter', backend)
    await answerWhenAsked('cancel')
    expect(await started).toBe(0)
    expect(stores.cleanAnyway).not.toHaveBeenCalled()
    expect(stores.release).toHaveBeenCalledTimes(1)
  })

  it('does not start beside a run', async () => {
    stores.claim.mockReturnValue(null)
    const backend = backendWith({ 0: [region('a')] })
    expect(await startNanoBananaRun('chapter', backend)).toBe(0)
    expect(stores.modals).toEqual([])
  })

  it('stops after three failures in a row, so a bad key cannot fail a chapter', async () => {
    stores.cleanAnyway.mockResolvedValue(false)
    const backend = backendWith({ 0: ['a', 'b', 'c', 'd', 'e'].map((id) => region(id)) })
    const started = startNanoBananaRun('chapter', backend)
    await answerWhenAsked('confirm')
    expect(await started).toBe(0)
    expect(stores.cleanAnyway).toHaveBeenCalledTimes(3)
    expect(stores.notify).toHaveBeenCalledWith(expect.objectContaining({ key: 'nanoBanana.run.stoppedFailing' }))
    expect(nanoBananaRun.active).toBe(false)
  })

  it('counts a success as the end of a failing streak', async () => {
    stores.cleanAnyway
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true)
    const backend = backendWith({ 0: ['a', 'b', 'c', 'd', 'e'].map((id) => region(id)) })
    const started = startNanoBananaRun('chapter', backend)
    await answerWhenAsked('confirm')
    expect(await started).toBe(2)
    expect(stores.cleanAnyway).toHaveBeenCalledTimes(5)
  })

  it('finishes the region in flight on Stop and starts no other', async () => {
    stores.cleanAnyway.mockImplementation(async () => {
      stopNanoBananaRun()
      return true
    })
    const backend = backendWith({ 0: ['a', 'b', 'c'].map((id) => region(id)) })
    const started = startNanoBananaRun('chapter', backend)
    await answerWhenAsked('confirm')
    expect(await started).toBe(1)
    expect(stores.cleanAnyway).toHaveBeenCalledTimes(1)
  })
})

describe('the window in front of a Nano Banana request', () => {
  it('opens with the model, the size the model answers at, and the saved instruction', async () => {
    const backend = backendWith({})
    backend.readSettings.mockResolvedValue({ nanoBananaModel: 'gemini-nano-banana-2.1', nanoBananaPrompt: 'a' })
    const asked = confirmNanoBanana(1, backend)
    const spec = await answerWhenAsked('cancel')
    expect(spec.props).toEqual({ count: 1, model: 'gemini-nano-banana-2.1', size: '1K', prompt: 'a' })
    expect(await asked).toBe(false)
    expect(backend.writeSettings).not.toHaveBeenCalled()
  })

  it('keeps what the person wrote, trimmed and cut, before anything is sent', async () => {
    const backend = backendWith({})
    const asked = confirmNanoBanana(1, backend)
    await answerWhenAsked({ id: 'confirm', prompt: `  ${'x'.repeat(900)}  ` })
    expect(await asked).toBe(true)
    expect(backend.writeSettings).toHaveBeenCalledWith({ nanoBananaPrompt: 'x'.repeat(600) })
  })

  it('sends nothing when the instruction could not be saved', async () => {
    const backend = backendWith({})
    backend.writeSettings.mockRejectedValue(new Error('disk'))
    const asked = confirmNanoBanana(1, backend)
    await answerWhenAsked('confirm')
    expect(await asked).toBe(false)
    expect(stores.notify).toHaveBeenCalledWith(expect.objectContaining({ key: 'nanoBanana.run.promptNotSaved' }))
  })

  it('treats Escape and the backdrop as Cancel', async () => {
    const backend = backendWith({})
    const asked = confirmNanoBanana(1, backend)
    await answerWhenAsked(null)
    expect(await asked).toBe(false)
  })
})
