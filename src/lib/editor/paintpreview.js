/** One backend request in flight, with only the newest pending stroke retained. */
export function previewQueue(request, present, unavailable = () => {}) {
  let generation = 0
  let serial = 0
  let pending = null
  let running = false
  let waiters = []
  async function pump() {
    if (running) return
    running = true
    while (pending) {
      const spec = pending
      pending = null
      const current = generation
      try {
        const result = await request(spec)
        if (current === generation && !pending && result?.requestId === spec.requestId &&
            result.revision === spec.revision && result.chapterId === spec.chapterId && result.pageIndex === spec.pageIndex) {
          present(result)
        } else if (current === generation && !pending && !result) {
          unavailable()
        }
      } catch { if (current === generation && !pending) unavailable() }
    }
    running = false
    const done = waiters
    waiters = []
    done.forEach(resolve => resolve())
  }
  return {
    submit(spec) { pending = { ...spec, requestId: ++serial }; void pump() },
    flush() { return running || pending ? new Promise(resolve => waiters.push(resolve)) : Promise.resolve() },
    cancel() { generation++; pending = null },
  }
}

const flushers = new Map()
export function registerPaintPreview(pageId, flush) {
  flushers.set(pageId, flush)
  return () => { if (flushers.get(pageId) === flush) flushers.delete(pageId) }
}
export async function flushPaintPreview(pageId) { await flushers.get(pageId)?.() }
