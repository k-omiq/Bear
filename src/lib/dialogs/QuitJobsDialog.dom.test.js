/**
 * The quit guard's question, drawn beside the modal stack as `App.svelte`
 * draws it: how many jobs run and which, Keep running only with a tray,
 * Cancel first, each answer reaching its command, and Escape answering here
 * without reaching the dialog underneath.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { askQuit, jobs, registerJob, resetJobs } from '../state/jobs.svelte.js'
import { isDialogOutsideStackOpen } from '../shortcuts.js'
import QuitJobsDialog from './QuitJobsDialog.svelte'

function fakeBackend() {
  return {
    subscribe: () => () => {},
    onDenoiseProgress: async () => () => {},
    onQuitRequested: async () => () => {},
    listJobs: vi.fn(async () => []),
    listProjects: vi.fn(async () => []),
    confirmQuit: vi.fn(async () => {}),
    hideToTray: vi.fn(async () => true),
  }
}

/** @type {ReturnType<typeof fakeBackend>} */
let backend

beforeEach(() => {
  resetJobs()
  backend = fakeBackend()
  setBackend(/** @type {any} */ (backend))
  registerJob({ runId: 'run-1', kind: 'clean', chapterId: 'c1', projectId: 'p1', projectName: 'Tsuki to Hane', chapterName: 'Feather Weight', total: 4 })
  registerJob({ runId: 'den-1', kind: 'denoise', chapterId: 'c2', projectId: 'p1', projectName: 'Tsuki to Hane', chapterName: 'Small Hours', total: 4 })
})

afterEach(() => {
  cleanup()
  resetJobs()
  setBackend(null)
})

describe('the quit question', () => {
  it('names the running jobs and keeps them running in the background on request', async () => {
    askQuit({ jobs: 2, canHide: true })
    const { getByRole, container } = render(QuitJobsDialog)

    const dialog = getByRole('dialog')
    expect(dialog.textContent).toContain(t('jobs.quit.title'))
    expect(dialog.textContent).toContain(t('jobs.quit.body', { count: 2 }))
    expect([...container.querySelectorAll('.names li')].map((item) => item.textContent)).toEqual([
      t('jobs.item.title', { project: 'Tsuki to Hane', chapter: 'Small Hours' }),
      t('jobs.item.title', { project: 'Tsuki to Hane', chapter: 'Feather Weight' }),
    ])
    expect(dialog.textContent).toContain(t('jobs.quit.hideNote'))
    // Cancel is the first control, so it is where `Modal` puts the focus
    // (jsdom lays nothing out, so the focus move itself is not observable
    // here): a stray Return never lands on the answer that stops the jobs.
    expect([...dialog.querySelectorAll('button')].map((button) => button.textContent?.trim()))
      .toEqual([t('shell.action.cancel'), t('jobs.quit.keep'), t('jobs.quit.stop')])
    // The shortcut layer knows a dialog it cannot see is up.
    expect(isDialogOutsideStackOpen()).toBe(true)

    await fireEvent.click(getByRole('button', { name: t('jobs.quit.keep') }))
    await waitFor(() => expect(backend.hideToTray).toHaveBeenCalledTimes(1))
    expect(backend.confirmQuit).not.toHaveBeenCalled()
    expect(jobs.quit).toBeNull()
  })

  it('offers no Keep running without a tray, and stops the jobs to quit', async () => {
    askQuit({ jobs: 2, canHide: false })
    const { getByRole, queryByRole } = render(QuitJobsDialog)
    expect(queryByRole('button', { name: t('jobs.quit.keep') })).toBeNull()
    await fireEvent.click(getByRole('button', { name: t('jobs.quit.stop') }))
    await waitFor(() => expect(backend.confirmQuit).toHaveBeenCalledTimes(1))
  })

  it('answers Escape itself, and the dialog underneath never hears it', async () => {
    const underneath = vi.fn()
    window.addEventListener('keydown', underneath)
    try {
      askQuit({ jobs: 2, canHide: true })
      const view = render(QuitJobsDialog)
      // From the focused control, as a key in the app arrives: it is taken on
      // the way down and never bubbles back up to the window's listeners.
      await fireEvent.keyDown(document.body, { key: 'Escape' })
      expect(jobs.quit).toBeNull()
      expect(underneath).not.toHaveBeenCalled()
      view.unmount()
      expect(isDialogOutsideStackOpen()).toBe(false)
      expect(backend.confirmQuit).not.toHaveBeenCalled()
      expect(backend.hideToTray).not.toHaveBeenCalled()
    } finally {
      window.removeEventListener('keydown', underneath)
    }
  })
})
