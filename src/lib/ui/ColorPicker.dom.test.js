import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { tick } from 'svelte'
import ColorPicker from './ColorPicker.svelte'

afterEach(() => cleanup())

it('places a portaled picker above a low trigger and keeps panel controls active', async () => {
  const onchange = vi.fn()
  const view = render(ColorPicker, { props: { value: '#ffffff', label: 'Fill colour', unclipped: true, onchange } })
  const trigger = view.getByRole('button', { name: 'Fill colour' })
  const anchor = trigger.parentElement
  const oldRect = anchor.getBoundingClientRect
  anchor.getBoundingClientRect = () => /** @type {DOMRect} */ ({
    left: 600, right: 626, top: 430, bottom: 456, width: 26, height: 26, x: 600, y: 430, toJSON() {},
  })
  const previous = { w: globalThis.innerWidth, h: globalThis.innerHeight }
  globalThis.innerWidth = 640
  globalThis.innerHeight = 480
  try {
    await fireEvent.click(trigger)
    await tick()
    const panel = screen.getByRole('dialog', { name: 'Fill colour' })
    expect(panel.parentElement).toBe(document.body)
    expect(panel.style.left).toBe('372px')
    expect(panel.style.top).toBe('62px')
    await fireEvent.pointerDown(panel)
    expect(screen.getByRole('dialog', { name: 'Fill colour' })).toBe(panel)
    await fireEvent.input(screen.getByRole('textbox', { name: 'Hex' }), { target: { value: '#123456' } })
    expect(onchange).toHaveBeenCalledWith('#123456')
    await view.rerender({ value: '#123456', label: 'Fill colour', unclipped: false, onchange })
    expect(anchor.contains(panel)).toBe(true)
    await view.rerender({ value: '#123456', label: 'Fill colour', unclipped: true, onchange })
    expect(panel.parentElement).toBe(document.body)
  } finally {
    anchor.getBoundingClientRect = oldRect
    globalThis.innerWidth = previous.w
    globalThis.innerHeight = previous.h
  }
})

it('samples the screen through the native command in the macOS app, where there is no EyeDropper', async () => {
  const invoke = vi.fn(async () => '#1a2b3c')
  const platform = Object.getOwnPropertyDescriptor(globalThis.navigator, 'platform')
  Object.defineProperty(globalThis.navigator, 'platform', { value: 'MacIntel', configurable: true })
  globalThis.__TAURI__ = { core: { invoke } }
  try {
    const onchange = vi.fn()
    const view = render(ColorPicker, { props: { value: '#ffffff', label: 'Fill colour', onchange } })
    await fireEvent.click(view.getByRole('button', { name: 'Fill colour' }))
    await fireEvent.click(screen.getByRole('button', { name: 'Pick a color from the screen' }))
    await tick()
    expect(invoke).toHaveBeenCalledWith('pick_screen_color')
    expect(onchange).toHaveBeenCalledWith('#1a2b3c')
  } finally {
    delete globalThis.__TAURI__
    if (platform) Object.defineProperty(globalThis.navigator, 'platform', platform)
    else delete (/** @type {any} */ (globalThis.navigator)).platform
  }
})

it('draws no eyedropper where the screen cannot be sampled', async () => {
  const view = render(ColorPicker, { props: { value: '#ffffff', label: 'Fill colour', onchange: vi.fn() } })
  await fireEvent.click(view.getByRole('button', { name: 'Fill colour' }))
  expect(screen.queryByRole('button', { name: 'Pick a color from the screen' })).toBeNull()
})
