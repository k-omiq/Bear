import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { app } from '../state/app.svelte.js'
import { t } from '../i18n/index.js'
import QwenPromptDialog from './QwenPromptDialog.svelte'

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

it('Klein asks what the lettering covers, in its own words', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'sound_effect', description: '' }, flux: true })
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  expect(screen.getByText(t('qwen.prompt.preserveFlux'))).toBeTruthy()
  expect(screen.queryByText(t('qwen.prompt.preserve'))).toBeNull()
  await fireEvent.input(screen.getByLabelText(t('qwen.prompt.descriptionFlux')), { target: { value: 'A brick wall' } })
  await fireEvent.click(screen.getByText(t('qwen.prompt.cleanFlux')))
  expect(modal.onresolve).toHaveBeenCalledWith({ target: 'sound_effect', description: 'A brick wall' })
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

it('a single-region rerun visibly preserves Automatic guidance saved by a batch', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'auto', description: 'Small lettering' } })
  const screen = render(QwenPromptDialog, { props: { spec: modal } })
  expect(screen.getByLabelText(t('qwen.prompt.auto')).checked).toBe(true)
  await fireEvent.click(screen.getByText(t('qwen.prompt.clean')))
  expect(modal.onresolve).toHaveBeenCalledWith({ target: 'auto', description: 'Small lettering' })
})

it('a prompt draft survives being hidden by another dialog', async () => {
  const modal = spec('qwenPrompt', { initial: { target: 'sound_effect', description: '' } })
  const first = render(QwenPromptDialog, { props: { spec: modal } })
  await fireEvent.click(first.getByLabelText(t('qwen.prompt.dialogue')))
  await fireEvent.input(first.getByLabelText(t('qwen.prompt.description')), { target: { value: 'Keep the drawing beside the bubble' } })
  first.unmount()
  const restored = render(QwenPromptDialog, { props: { spec: modal } })
  expect(restored.getByLabelText(t('qwen.prompt.dialogue')).checked).toBe(true)
  expect(restored.getByLabelText(t('qwen.prompt.description')).value).toBe('Keep the drawing beside the bubble')
})
