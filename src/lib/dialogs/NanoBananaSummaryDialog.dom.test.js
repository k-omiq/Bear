/**
 * The summary of a Nano Banana batch: what it cleaned, why it stopped early,
 * the regions it did not clean counted by reason with their pages, and Send
 * again only while the batch's chapter is open in the editor.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { registerJob, resetJobs } from '../state/jobs.svelte.js'
import NanoBananaSummaryDialog from './NanoBananaSummaryDialog.svelte'

const retryNanoBanana = vi.fn(async () => null)
vi.mock('../editor/nanobananarun.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  retryNanoBanana: (/** @type {string} */ runId) => retryNanoBanana(runId),
}))

const BLOCKED = 'decline.reason.nanoBananaBlocked'
const BAD = 'decline.reason.nanoBananaBadAnswer'

function fakeBackend() {
  const handlers = new Set()
  return {
    emit: (/** @type {any} */ event) => [...handlers].forEach((handler) => handler(event)),
    subscribe: (/** @type {any} */ handler) => {
      handlers.add(handler)
      return () => handlers.delete(handler)
    },
    onDenoiseProgress: async () => () => {},
    onQuitRequested: async () => () => {},
    listJobs: vi.fn(async () => []),
    listProjects: vi.fn(async () => []),
  }
}

/** @type {ReturnType<typeof fakeBackend>} */
let backend

/** A finished batch with this report. @param {Record<string, any>} report */
function finished(report) {
  registerJob({ runId: 'run-1', kind: 'nanoBanana', chapterId: 'c1', total: 3, projectId: 'p1', projectName: 'P', chapterName: 'C' })
  backend.emit({ type: 'nano-banana-report', runId: 'run-1', chapterId: 'c1', report })
  backend.emit({ type: 'run-finished', runId: 'run-1', chapterId: 'c1', reason: report.stopped ? 'failed' : 'completed', pagesQueued: 3, pagesCleaned: 3, regionsCleaned: report.cleaned })
  return render(NanoBananaSummaryDialog, { props: { spec: { props: { runId: 'run-1' } } } })
}

beforeEach(() => {
  resetJobs()
  backend = fakeBackend()
  setBackend(/** @type {any} */ (backend))
  app.modals.length = 0
  editor.chapter = /** @type {any} */ ({ id: 'c1', review: [], pages: [] })
  editor.run.active = false
})

afterEach(() => {
  cleanup()
  resetJobs()
  setBackend(null)
  editor.chapter = null
  retryNanoBanana.mockClear()
})

describe('the Nano Banana summary', () => {
  it('counts the regions not cleaned by reason, with their pages, and sends them again', async () => {
    const { container, getByRole } = finished({
      total: 5, cleaned: 2, gone: 1, left: [], byUser: false,
      failed: [
        { regionId: 'a', pageIndex: 2, reason: BLOCKED },
        { regionId: 'b', pageIndex: 0, reason: BLOCKED },
        { regionId: 'c', pageIndex: 2, reason: BAD },
      ],
    })
    expect(container.textContent).toContain(t('nanoBanana.job.summaryCleaned', { cleaned: 2, total: 5 }))
    const blocked = /** @type {HTMLElement} */ (container.querySelector(`[data-reason="${BLOCKED}"]`))
    expect(blocked.textContent).toContain(t('nanoBanana.job.summaryReason', { count: 2, reasonKey: BLOCKED }))
    expect(blocked.textContent).toContain(t('nanoBanana.job.summaryPages', { pages: '1, 3' }))
    expect(container.textContent).toContain(t('nanoBanana.job.summaryGone', { count: 1 }))
    await fireEvent.click(getByRole('button', { name: t('nanoBanana.job.retry', { count: 3 }) }))
    expect(retryNanoBanana).toHaveBeenCalledWith('run-1')
  })

  it('says why it stopped early, and counts what it did not reach', () => {
    const { container } = finished({
      total: 4, cleaned: 1, gone: 0, failed: [], byUser: false,
      stopped: 'decline.reason.nanoBananaBilling',
      left: [{ regionId: 'b', pageIndex: 0 }, { regionId: 'c', pageIndex: 1 }, { regionId: 'd', pageIndex: 1 }],
    })
    expect(container.textContent).toContain(t('nanoBanana.job.summaryHalted', { reasonKey: 'decline.reason.nanoBananaBilling' }))
    expect(container.textContent).toContain(t('nanoBanana.job.summaryLeft', { count: 3 }))
  })

  it('offers no Send again away from the chapter, and says where to go', () => {
    editor.chapter = /** @type {any} */ ({ id: 'other', review: [], pages: [] })
    const { container, queryByRole } = finished({
      total: 2, cleaned: 1, gone: 0, left: [], byUser: false, failed: [{ regionId: 'a', pageIndex: 0, reason: BLOCKED }],
    })
    expect(queryByRole('button', { name: t('nanoBanana.job.retry', { count: 1 }) })).toBeNull()
    expect(container.textContent).toContain(t('nanoBanana.job.retryHint'))
  })

  it('offers nothing to send again when all was cleaned', () => {
    const { container, queryByRole } = finished({ total: 2, cleaned: 2, gone: 0, left: [], failed: [], byUser: false })
    expect(container.querySelector('[data-action="retry"]')).toBeNull()
    expect(queryByRole('button', { name: t('shell.action.close') })).not.toBeNull()
  })
})
