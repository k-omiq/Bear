// @vitest-environment jsdom
import { it, expect, vi } from 'vitest'
import { installReloadGuard } from './reload-guard.js'
it('blocks reload shortcuts and browser menus while preserving custom menu handlers', () => {
  const off = installReloadGuard()
  const custom = vi.fn()
  document.body.addEventListener('contextmenu', custom)
  const menu = new MouseEvent('contextmenu', { bubbles: true, cancelable: true })
  document.body.dispatchEvent(menu)
  expect(custom).toHaveBeenCalledOnce()
  expect(menu.defaultPrevented).toBe(true)
  for (const options of [{ key: 'F5' }, { key: 'r', metaKey: true }, { key: 'R', ctrlKey: true, shiftKey: true }]) {
    const event = new KeyboardEvent('keydown', { ...options, cancelable: true, bubbles: true })
    document.body.dispatchEvent(event)
    expect(event.defaultPrevented).toBe(true)
  }
  const regular = new KeyboardEvent('keydown', { key: 'r', cancelable: true })
  window.dispatchEvent(regular)
  expect(regular.defaultPrevented).toBe(false)
  off()
  document.body.removeEventListener('contextmenu', custom)
  const released = new KeyboardEvent('keydown', { key: 'F5', cancelable: true })
  window.dispatchEvent(released)
  expect(released.defaultPrevented).toBe(false)
})
