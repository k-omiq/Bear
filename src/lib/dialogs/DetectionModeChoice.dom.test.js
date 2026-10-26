/**
 * The Normal and Advanced cards as a radio group: arrow keys move focus and
 * the one tab stop, never the choice, because in Settings a choice starts
 * that mode's download. A click (or Space and Enter, which press the card)
 * chooses.
 */
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import DetectionModeChoice from './DetectionModeChoice.svelte'

afterEach(cleanup)

/** @param {ReturnType<typeof render>} rendered @param {'normal'|'advanced'} mode */
const card = (rendered, mode) => /** @type {HTMLElement} */ (rendered.container.querySelector(`[data-mode="${mode}"]`))

it('moves focus with the arrow keys and chooses only on a press', async () => {
  const onchange = vi.fn()
  const outside = document.body.appendChild(document.createElement('button'))
  const rendered = render(DetectionModeChoice, { value: 'normal', onchange, label: 'Detection' })
  expect(card(rendered, 'normal').tabIndex).toBe(0)
  expect(card(rendered, 'advanced').tabIndex).toBe(-1)

  card(rendered, 'normal').focus()
  await fireEvent.keyDown(card(rendered, 'normal'), { key: 'ArrowRight' })
  expect(document.activeElement).toBe(card(rendered, 'advanced'))
  // The tab stop goes with focus; the choice stays where it was.
  expect(card(rendered, 'advanced').tabIndex).toBe(0)
  expect(card(rendered, 'normal').tabIndex).toBe(-1)
  expect(card(rendered, 'normal').getAttribute('aria-checked')).toBe('true')
  await fireEvent.keyDown(card(rendered, 'advanced'), { key: 'ArrowLeft' })
  expect(document.activeElement).toBe(card(rendered, 'normal'))
  expect(onchange).not.toHaveBeenCalled()

  await fireEvent.keyDown(card(rendered, 'normal'), { key: 'End' })
  await fireEvent.click(card(rendered, 'advanced'))
  expect(onchange).toHaveBeenCalledTimes(1)
  expect(onchange).toHaveBeenCalledWith('advanced')

  // Leaving the group hands the tab stop back to the chosen card.
  outside.focus()
  await fireEvent.focusOut(card(rendered, 'advanced'), { relatedTarget: outside })
  expect(card(rendered, 'normal').tabIndex).toBe(0)
  expect(card(rendered, 'advanced').tabIndex).toBe(-1)
  outside.remove()
})
