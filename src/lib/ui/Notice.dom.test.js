import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import Notice from './Notice.svelte'

afterEach(() => cleanup())

it('keeps an optional notice action separate from dismissal', async () => {
  const onaction = vi.fn()
  const onclose = vi.fn()
  const view = render(Notice, { props: {
    text: 'A model is missing', dismissLabel: 'Dismiss', actionLabel: 'Choose models',
    onaction, onclose,
  } })
  await fireEvent.click(view.getByRole('button', { name: 'Choose models' }))
  expect(onaction).toHaveBeenCalledOnce()
  expect(onclose).not.toHaveBeenCalled()
})
