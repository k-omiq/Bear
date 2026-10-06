/**
 * The top-right cluster, mounted, for its one conditional control: the
 * text-shaped review is the optional all-text mode, so its button is drawn
 * only when the session's text policy asks for it.
 */
import { afterEach, expect, it } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { app } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { session } from '../state/session.svelte.js'
import { setBackend } from '../api/backend.js'
import { cloud, stopCloud } from '../state/cloud.svelte.js'
import { t } from '../i18n/index.js'
import ModalHost from '../shell/ModalHost.svelte'
import ClusterRight from './ClusterRight.svelte'

const policy = session.textPolicy
const route = app.route

afterEach(() => {
  cleanup()
  session.textPolicy = policy
  app.route = route
  app.modals.length = 0
  editor.chapter = null
  editor.loading = false
  editor.pageIndex = 0
  setBackend(null)
})

it('hides the review under the legacy text policy', () => {
  session.textPolicy = 'legacy_gate'
  const screen = render(ClusterRight)
  expect(screen.queryByRole('button', { name: t('editor.action.textShapeReview') })).toBeNull()
  expect(screen.getByRole('button', { name: new RegExp(t('editor.action.settings')) })).toBeTruthy()
})

// Cloud analysis lives in the review and does not depend on the text policy,
// so a usable cloud GPU brings the entry back as the cloud action.
it('offers the review as the cloud entry under the legacy policy while the cloud GPU is usable', async () => {
  session.textPolicy = 'legacy_gate'
  editor.chapter = /** @type {any} */ ({ id: 'ch-1', pages: [], review: [] })
  const allowed = session.cloudAllowed
  session.cloudAllowed = true
  cloud.readiness = /** @type {any} */ ({ ...cloud.readiness, configured: true })
  try {
    const screen = render(ClusterRight)
    await fireEvent.click(screen.getByRole('button', { name: t('cloud.analysis.entry.action') }))
    expect(app.modals.at(-1)).toMatchObject({ kind: 'workflowReview', props: { chapterId: 'ch-1', cloud: true } })
  } finally {
    session.cloudAllowed = allowed
    stopCloud()
  }
})

it('offers the review under the all-text policy and opens it for the open chapter', async () => {
  session.textPolicy = 'all_text'
  app.route = { name: 'editor', projectId: 'p1', chapterId: 'ch-1' }
  editor.chapter = /** @type {any} */ ({ id: 'ch-1', pages: [], review: [] })
  editor.pageIndex = 2
  const screen = render(ClusterRight)
  await fireEvent.click(screen.getByRole('button', { name: t('editor.action.textShapeReview') }))
  expect(app.modals.at(-1)).toMatchObject({ kind: 'workflowReview', props: { chapterId: 'ch-1', pageIndex: 2 } })
})

// The route names the chapter asked for; the editor holds the one whose pages
// are loaded. They differ while a switch is loading and after an open that
// failed, and the review lists its pages from the editor's chapter, so it
// opens on that one - the chapter on screen - like the other two ways in.
it('opens the review on the chapter the editor holds, not the one the route asks for', async () => {
  session.textPolicy = 'all_text'
  app.route = { name: 'editor', projectId: 'p1', chapterId: 'ch-2' }
  editor.chapter = /** @type {any} */ ({ id: 'ch-1', pages: [], review: [] })
  editor.pageIndex = 4
  const screen = render(ClusterRight)
  await fireEvent.click(screen.getByRole('button', { name: t('editor.action.textShapeReview') }))
  expect(app.modals.at(-1)).toMatchObject({ kind: 'workflowReview', props: { chapterId: 'ch-1', pageIndex: 4 } })
})

it('holds the review while no chapter is loaded', async () => {
  session.textPolicy = 'all_text'
  app.route = { name: 'editor', projectId: 'p1', chapterId: 'ch-2' }
  editor.loading = true
  const screen = render(ClusterRight)
  const review = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('editor.action.textShapeReview') }))
  expect(review.disabled).toBe(true)
  await fireEvent.click(review)
  expect(app.modals).toHaveLength(0)
})

// The dialog draws its own title, so the spec's default `titleKey`
// (`modal.title.workflowReview`, which the catalogue has no entry for) is
// never read: the dialog is named by what it shows.
it('names the review dialog by its own title', async () => {
  session.textPolicy = 'all_text'
  editor.chapter = /** @type {any} */ ({ id: 'ch-1', pages: [], review: [] })
  setBackend(/** @type {any} */ (new Proxy({}, { get: (_, key) => key === 'subscribe'
    ? () => () => {}
    : async () => { throw new Error('not in this test') } })))
  const screen = render(ClusterRight)
  const host = render(ModalHost)
  await fireEvent.click(screen.getByRole('button', { name: t('editor.action.textShapeReview') }))
  const dialog = await host.findByRole('dialog')
  expect(dialog.getAttribute('aria-modal')).toBe('true')
  const title = document.getElementById(/** @type {string} */ (dialog.getAttribute('aria-labelledby')))
  expect(title?.textContent?.trim()).toBe(t('workflow.title.review'))
  expect(host.getByRole('dialog', { name: t('workflow.title.review') })).toBe(dialog)
})
