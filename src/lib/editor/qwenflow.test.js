import { afterEach, expect, it, vi } from 'vitest'
import { app, closeModal, pushModal } from '../state/app.svelte.js'
import { dismissQwenReview, reviewQwen, takeQwenRetry } from './qwenflow.js'
import { onAttemptEvent, stopCloud } from '../state/cloud.svelte.js'

afterEach(() => { app.modals.length = 0; app.notices.length = 0; stopCloud() })

const attemptId = 'att-' + 'a'.repeat(24)
const preview = { attemptId, before: 'data:image/png;base64,AA==', after: 'data:image/png;base64,AQ==', canRetry: true }

it('terminal attempts remove only their review without resolving a stale native request', () => {
  const backend = { resolveQwenReview: vi.fn() }
  reviewQwen(preview, backend)
  reviewQwen({ ...preview, attemptId: 'att-' + 'b'.repeat(24) }, backend)
  pushModal({ kind: 'qwenPrompt', props: {} })
  dismissQwenReview(attemptId)
  expect(app.modals.map((modal) => modal.kind)).toEqual(['qwenReview', 'qwenPrompt'])
  expect(app.modals[0].props.preview.attemptId).toBe('att-' + 'b'.repeat(24))
  expect(backend.resolveQwenReview).not.toHaveBeenCalled()
})

it('dismissing the terminal review leaves a requested retry available to the caller', async () => {
  const backend = { resolveQwenReview: vi.fn(async () => {}) }
  const edit = { target: 'sound_effect', description: 'Also remove the thin strokes' }
  reviewQwen(preview, backend)
  closeModal({ choice: 'retry', edit })
  dismissQwenReview(attemptId)
  expect(takeQwenRetry(attemptId)).toEqual(edit)
  expect(takeQwenRetry(attemptId)).toBeNull()
})

it('a native cancellation closes an expired review while preserving unrelated dialogs', () => {
  const backend = { resolveQwenReview: vi.fn() }
  reviewQwen(preview, backend)
  pushModal({ kind: 'qwenPrompt', props: {} })
  onAttemptEvent({ attemptId, phase: 'cancelled', errorCode: 'cancelled' })
  expect(app.modals.map((modal) => modal.kind)).toEqual(['qwenPrompt'])
  expect(backend.resolveQwenReview).not.toHaveBeenCalled()
})

it('a native review-closed event removes a queued or current recovered review', () => {
  const backend = { resolveQwenReview: vi.fn() }
  const otherAttempt = 'att-' + 'b'.repeat(24)
  reviewQwen(preview, backend)
  reviewQwen({ ...preview, attemptId: otherAttempt }, backend)
  reviewQwen({ attemptId, closed: true }, backend)
  expect(app.modals).toHaveLength(1)
  expect(app.modals[0].props.preview.attemptId).toBe(otherAttempt)
  reviewQwen({ attemptId: otherAttempt, closed: true }, backend)
  expect(app.modals).toHaveLength(0)
  expect(backend.resolveQwenReview).not.toHaveBeenCalled()
})

it('a review-closed event preserves a retry already chosen by the user', () => {
  const backend = { resolveQwenReview: vi.fn(async () => {}) }
  const edit = { target: 'sound_effect', description: 'Also remove the thin strokes' }
  reviewQwen(preview, backend)
  closeModal({ choice: 'retry', edit })
  reviewQwen({ attemptId, closed: true }, backend)
  expect(takeQwenRetry(attemptId)).toEqual(edit)
})
