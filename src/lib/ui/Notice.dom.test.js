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

it('dismisses itself after its duration', () => {
  vi.useFakeTimers()
  try {
    const onclose = vi.fn()
    render(Notice, { props: { text: 'Saved', dismissLabel: 'Dismiss', duration: 1000, onclose } })
    vi.advanceTimersByTime(999)
    expect(onclose).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(onclose).toHaveBeenCalledOnce()
  } finally {
    vi.useRealTimers()
  }
})

// The low-memory warning a run raises (`App.svelte`) stays until it is
// closed or its Stop run is pressed: setTimeout would read Infinity as 0.
it('stays for an endless duration until it is closed or its action is pressed', async () => {
  vi.useFakeTimers()
  try {
    const onclose = vi.fn()
    const onaction = vi.fn()
    const view = render(Notice, { props: {
      text: 'Low memory', dismissLabel: 'Dismiss', tone: 'warn', duration: Infinity,
      actionLabel: 'Stop run', onaction, onclose,
    } })
    vi.advanceTimersByTime(24 * 60 * 60 * 1000)
    expect(onclose).not.toHaveBeenCalled()
    await fireEvent.click(view.getByRole('button', { name: 'Stop run' }))
    expect(onaction).toHaveBeenCalledOnce()
    expect(onclose).not.toHaveBeenCalled()
    await fireEvent.click(view.getByRole('button', { name: 'Dismiss' }))
    expect(onclose).toHaveBeenCalledOnce()
  } finally {
    vi.useRealTimers()
  }
})
