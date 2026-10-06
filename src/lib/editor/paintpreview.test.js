import { describe, expect, it, vi } from 'vitest'
import { previewQueue } from './paintpreview.js'
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r }); return { promise, resolve } }
const spec = { chapterId: 'chapter', pageIndex: 2, revision: 'a' }

describe('authoritative paint preview queue', () => {
  it('coalesces while busy and waits for the final authoritative frame', async () => {
    const first = deferred(), last = deferred()
    const request = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(last.promise)
    const present = vi.fn(), queue = previewQueue(request, present)
    queue.submit(spec); queue.submit({ ...spec, points: [1] }); queue.submit({ ...spec, points: [2] })
    let flushed = false
    const flush = queue.flush().then(() => { flushed = true })
    first.resolve({ ...request.mock.calls[0][0], png: [] })
    await Promise.resolve()
    expect(present).not.toHaveBeenCalled()
    expect(request).toHaveBeenCalledTimes(2)
    expect(request.mock.calls[1][0].points).toEqual([2])
    expect(flushed).toBe(false)
    last.resolve({ ...request.mock.calls[1][0], png: [] })
    await flush
    expect(present).toHaveBeenCalledTimes(1)
  })
  it('removes a previous preview when the final request is unavailable', async () => {
    const unavailable = vi.fn(), present = vi.fn()
    const request = vi.fn().mockImplementationOnce(async s => s).mockResolvedValueOnce(null)
    const queue = previewQueue(request, present, unavailable)
    queue.submit(spec); await queue.flush()
    queue.submit(spec); await queue.flush()
    expect(present).toHaveBeenCalledTimes(1)
    expect(unavailable).toHaveBeenCalledTimes(1)
  })
  it.each(['chapterId', 'pageIndex', 'revision', 'requestId'])('rejects a mismatched %s', async (field) => {
    const present = vi.fn()
    const queue = previewQueue(async spec => ({ ...spec, [field]: 'wrong' }), present)
    queue.submit(spec); await queue.flush()
    expect(present).not.toHaveBeenCalled()
  })
  it('discards canceled in-flight work even after a new stroke starts', async () => {
    const first = deferred(), request = vi.fn().mockReturnValueOnce(first.promise).mockImplementation(async value => value)
    const present = vi.fn(), queue = previewQueue(request, present)
    queue.submit(spec); queue.cancel(); queue.submit({ ...spec, revision: 'b' })
    first.resolve(request.mock.calls[0][0]); await queue.flush()
    expect(present).toHaveBeenCalledTimes(1)
    expect(present.mock.calls[0][0].revision).toBe('b')
  })
})
