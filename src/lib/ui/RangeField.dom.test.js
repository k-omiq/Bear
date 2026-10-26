import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import RangeField from './RangeField.svelte'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

/** @param {Record<string, unknown>} props */
function field(props) {
  const onchange = vi.fn()
  const view = render(RangeField, { props: { label: 'Size', boxLabel: 'Size value', value: 20, min: 1, max: 500, onchange, ...props } })
  const slider = /** @type {HTMLInputElement} */ (screen.getByRole('slider', { name: 'Size' }))
  const box = /** @type {HTMLInputElement} */ (screen.getByLabelText('Size value'))
  return { view, slider, box, onchange }
}

describe('a range field', () => {
  it('maps a log track so its middle is the geometric mean, and every whole size has a position', async () => {
    const { slider, onchange } = field({ scale: 'log' })
    expect(Number(slider.max)).toBeGreaterThanOrEqual(3200)
    await fireEvent.input(slider, { target: { value: String(Number(slider.max) / 2) } })
    await fireEvent.change(slider, { target: { value: String(Number(slider.max) / 2) } })
    expect(onchange).toHaveBeenLastCalledWith(Math.round(Math.sqrt(500)))
  })

  it('commits a typed value on Enter, clamped to the range, and puts it back on Escape', async () => {
    const { box, onchange } = field({})
    await fireEvent.input(box, { target: { value: '900' } })
    await fireEvent.keyDown(box, { key: 'Enter' })
    expect(onchange).toHaveBeenCalledWith(500)
    onchange.mockClear()
    await fireEvent.input(box, { target: { value: '7' } })
    await fireEvent.keyDown(box, { key: 'Escape' })
    await fireEvent.blur(box)
    expect(onchange).not.toHaveBeenCalled()
    expect(box.value).toBe('20')
  })

  it('settles a run of arrow steps once, after the presses stop', async () => {
    vi.useFakeTimers()
    const { slider, onchange } = field({ min: 0, max: 100 })
    await fireEvent.keyDown(slider, { key: 'ArrowRight' })
    await fireEvent.keyDown(slider, { key: 'ArrowRight' })
    await fireEvent.keyDown(slider, { key: 'ArrowRight', shiftKey: true })
    expect(onchange).not.toHaveBeenCalled()
    vi.advanceTimersByTime(300)
    expect(onchange).toHaveBeenCalledTimes(1)
    expect(onchange).toHaveBeenCalledWith(27)
  })

  it('commits a draft the parent followed live, so a live parent still hears the release', async () => {
    vi.useFakeTimers()
    const oninput = vi.fn()
    const { view, slider, onchange } = field({ min: 0, max: 100, oninput })
    await fireEvent.keyDown(slider, { key: 'ArrowRight' })
    expect(oninput).toHaveBeenCalledWith(21)
    await view.rerender({ value: 21 })
    vi.advanceTimersByTime(300)
    expect(onchange).toHaveBeenCalledWith(21)
  })

  it('drops a live run of steps when the value is changed from outside, so it cannot settle onto the next owner', async () => {
    vi.useFakeTimers()
    const oninput = vi.fn()
    const { view, slider, onchange } = field({ min: 0, max: 100, oninput })
    await fireEvent.keyDown(slider, { key: 'ArrowRight' })
    // A tool switch that kept this control: the next tool's size is 80.
    await view.rerender({ value: 80 })
    vi.advanceTimersByTime(300)
    expect(onchange).not.toHaveBeenCalled()
    expect(slider.value).toBe('80')
  })

  it('settles a run of steps still waiting when it goes away', async () => {
    vi.useFakeTimers()
    const { view, slider, onchange } = field({ min: 0, max: 100 })
    await fireEvent.keyDown(slider, { key: 'ArrowLeft' })
    view.unmount()
    expect(onchange).toHaveBeenCalledWith(19)
  })

  it('leaves Up and Down in its box to a menu it is an item of', async () => {
    const { box, onchange } = field({ listItem: true })
    const down = new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true })
    box.dispatchEvent(down)
    expect(down.defaultPrevented).toBe(false)
    expect(onchange).not.toHaveBeenCalled()
  })

  it('steps on a wheel notch only while focused inside, when asked to', async () => {
    vi.useFakeTimers()
    const { view, slider, onchange } = field({ min: 0, max: 100, wheel: 'focused' })
    const root = /** @type {HTMLElement} */ (view.container.querySelector('.field'))
    root.dispatchEvent(new WheelEvent('wheel', { deltaY: -1, deltaMode: 1, bubbles: true, cancelable: true }))
    vi.advanceTimersByTime(300)
    expect(onchange).not.toHaveBeenCalled()
    slider.focus()
    root.dispatchEvent(new WheelEvent('wheel', { deltaY: -1, deltaMode: 1, bubbles: true, cancelable: true }))
    vi.advanceTimersByTime(300)
    expect(onchange).toHaveBeenCalledWith(21)
  })
})

describe('a range field, typed and stepped together', () => {
  it('steps from a typed value on Up in the box', async () => {
    const { box, onchange } = field({ min: 0, max: 100 })
    vi.useFakeTimers()
    await fireEvent.input(box, { target: { value: '50' } })
    await fireEvent.keyDown(box, { key: 'ArrowUp' })
    vi.advanceTimersByTime(300)
    expect(onchange).toHaveBeenLastCalledWith(51)
  })

  it('settles a waiting run of steps when the box takes the focus, so typing is not overwritten', async () => {
    vi.useFakeTimers()
    const { slider, box, onchange } = field({ min: 0, max: 100 })
    await fireEvent.keyDown(slider, { key: 'ArrowRight' })
    await fireEvent.focus(box)
    expect(onchange).toHaveBeenCalledWith(21)
    await fireEvent.input(box, { target: { value: '70' } })
    vi.advanceTimersByTime(300)
    expect(onchange).toHaveBeenCalledTimes(1)
    expect(box.value).toBe('70')
  })

  it('drops a waiting run of steps when the value is changed from outside, live parent or not', async () => {
    vi.useFakeTimers()
    const { view, slider, onchange } = field({ min: 0, max: 100 })
    await fireEvent.keyDown(slider, { key: 'ArrowRight' })
    await view.rerender({ value: 0 })
    vi.advanceTimersByTime(300)
    expect(onchange).not.toHaveBeenCalled()
  })

  it('commits a typed value when it goes away before the box lost the focus', async () => {
    const { view, box, onchange } = field({ min: 0, max: 100 })
    await fireEvent.input(box, { target: { value: '15' } })
    view.unmount()
    expect(onchange).toHaveBeenCalledWith(15)
  })
})
