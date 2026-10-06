import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { app } from '../state/app.svelte.js'
import { t } from '../i18n/index.js'
import QwenPromptDialog from './QwenPromptDialog.svelte'
import QwenReviewDialog from './QwenReviewDialog.svelte'

function spec(kind, props) {
  const modal = { id: 'qwen-test', kind, props, blocking: true, onresolve: vi.fn() }
  app.modals.push(modal)
  return modal
}
afterEach(() => { cleanup(); app.modals.length = 0 })

it('a blank description works with a chosen type and shows the preservation instructions', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'sound_effect', description: '' } })
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  expect(screen.getByLabelText(t('qwen.prompt.soundEffect')).checked).toBe(true)
  expect(screen.getByText(t('qwen.prompt.preserve'))).toBeTruthy()
  await fireEvent.click(screen.getByText(t('qwen.prompt.clean')))
  expect(modal.onresolve).toHaveBeenCalledWith({ target: 'sound_effect', description: '' })
})

it('takes ordinary words and a type, without exposing a raw prompt', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'sound_effect', description: '' } })
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  await fireEvent.click(screen.getByLabelText(t('qwen.prompt.dialogue')))
  await fireEvent.input(screen.getByLabelText(t('qwen.prompt.description')), { target: { value: 'Small words in the grey bubble' } })
  await fireEvent.click(screen.getByText(t('qwen.prompt.clean')))
  expect(modal.onresolve).toHaveBeenCalledWith({ target: 'dialogue', description: 'Small words in the grey bubble' })
})

it('Cancel leaves no request options', async () => {
  const modal = spec('qwenPrompt', {})
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  await fireEvent.click(screen.getByText(t('qwen.prompt.cancel')))
  expect(modal.onresolve).toHaveBeenCalledWith(null)
})

it('batch guidance defaults to Automatic and states the shared scope', () => {
  const modal = spec('qwenPrompt', { batch: true })
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  expect(screen.getByLabelText(t('qwen.prompt.auto')).checked).toBe(true)
  expect(screen.getByText(t('qwen.prompt.batch'))).toBeTruthy()
})

const preview = { before: 'data:image/png;base64,AA==', after: 'data:image/png;base64,AQ==', canRetry: true,
  edit: { target: 'sound_effect', description: 'Letters beside the hand' } }

it('shows the two candidates and Use result explicitly accepts', async () => {
  const modal = spec('qwenReview', { preview })
  const screen = render(QwenReviewDialog, { props: { spec: modal } })
  expect(screen.getByAltText(t('qwen.review.before')).src).toBe(preview.before)
  expect(screen.getByAltText(t('qwen.review.after')).src).toBe(preview.after)
  await fireEvent.click(screen.getByText(t('qwen.review.use')))
  expect(modal.onresolve).toHaveBeenCalledWith({ choice: 'use' })
})

it('retry keeps the type, takes the revised description, and explains the additional charge', async () => {
  const modal = spec('qwenReview', { preview })
  const screen = render(QwenReviewDialog, { props: { spec: modal } })
  expect(screen.getByText(t('qwen.review.retryCost'))).toBeTruthy()
  await fireEvent.input(screen.getByLabelText(t('qwen.prompt.description')), { target: { value: 'Also the black strokes at the top edge' } })
  await fireEvent.click(screen.getByText(t('qwen.review.retry')))
  expect(modal.onresolve).toHaveBeenCalledWith({ choice: 'retry', edit: { target: 'sound_effect', description: 'Also the black strokes at the top edge' } })
})

it('batch and recovered candidates can be discarded without claiming a retry has started', async () => {
  const modal = spec('qwenReview', { preview: { ...preview, canRetry: false } })
  const screen = render(QwenReviewDialog, { props: { spec: modal } })
  expect(screen.queryByText(t('qwen.review.retry'))).toBeNull()
  await fireEvent.click(screen.getByText(t('qwen.review.discard')))
  expect(modal.onresolve).toHaveBeenCalledWith({ choice: 'discard' })
})

it('retry is disabled until the description changes, avoiding the same paid request twice', () => {
  const modal = spec('qwenReview', { preview })
  const screen = render(QwenReviewDialog, { props: { spec: modal } })
  expect(screen.getByText(t('qwen.review.retry')).disabled).toBe(true)
})

it('a single-region rerun visibly preserves Automatic guidance saved by a batch', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'auto', description: 'Small lettering' } })
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  expect(screen.getByLabelText(t('qwen.prompt.auto')).checked).toBe(true)
  await fireEvent.click(screen.getByText(t('qwen.prompt.clean')))
  expect(modal.onresolve).toHaveBeenCalledWith({ target: 'auto', description: 'Small lettering' })
})

it('a prompt draft survives being hidden by an arriving review', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'sound_effect', description: '' } })
  const first = render(QwenPromptDialog, { props: { spec: modal } })
  await fireEvent.click(first.getByLabelText(t('qwen.prompt.dialogue')))
  await fireEvent.input(first.getByLabelText(t('qwen.prompt.description')), { target: { value: 'Keep the drawing beside the bubble' } })
  first.unmount()
  const restored = render(QwenPromptDialog, { props: { spec: modal } })
  expect(restored.getByLabelText(t('qwen.prompt.dialogue')).checked).toBe(true)
  expect(restored.getByLabelText(t('qwen.prompt.description')).value).toBe('Keep the drawing beside the bubble')
})

it('a revised review description survives another review appearing above it', async () => {
  const modal = spec('qwenReview', { preview })
  const first = render(QwenReviewDialog, { props: { spec: modal } })
  await fireEvent.input(first.getByLabelText(t('qwen.prompt.description')), { target: { value: 'Also the strokes above the hand' } })
  first.unmount()
  const restored = render(QwenReviewDialog, { props: { spec: modal } })
  expect(restored.getByLabelText(t('qwen.prompt.description')).value).toBe('Also the strokes above the hand')
  expect(restored.getByText(t('qwen.review.retry')).disabled).toBe(false)
})
