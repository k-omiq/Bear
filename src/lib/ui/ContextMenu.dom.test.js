/**
 * The context menu takes and gives back focus without scrolling: a region's
 * menu is mounted inside the canvas scroller, which WebKit scrolled to it.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import ContextMenu from './ContextMenu.svelte'

/** @type {any} */
let menu = null

afterEach(() => {
  if (menu) unmount(menu)
  menu = null
  document.body.replaceChildren()
  vi.restoreAllMocks()
})

const SECTIONS = [{
  id: 'main',
  items: [
    { id: 'first', label: 'First' },
    { id: 'second', label: 'Second' },
  ],
}]

/** @param {Record<string, any>} [props] */
function show(props = {}) {
  const target = document.createElement('div')
  document.body.appendChild(target)
  menu = mount(ContextMenu, {
    target,
    props: { x: 10, y: 10, label: 'Region', sections: SECTIONS, onselect: () => {}, onclose: () => {}, ...props },
  })
  flushSync()
  return target
}

/** The elements `focus` was called on, with the options each call got. */
function focusCalls() {
  const spy = vi.spyOn(HTMLElement.prototype, 'focus')
  return () => spy.mock.calls.map((args, i) => ({
    label: /** @type {HTMLElement} */ (spy.mock.contexts[i]).textContent?.trim() || /** @type {HTMLElement} */ (spy.mock.contexts[i]).id,
    args,
  }))
}

describe('ContextMenu focus', () => {
  it('focuses the first item as it opens, without scrolling', () => {
    const calls = focusCalls()
    const target = show()
    expect(calls()).toEqual([{ label: 'First', args: [{ preventScroll: true }] }])
    expect(document.activeElement).toBe(target.querySelector('[role="menuitem"]'))
  })

  it('gives focus back to what had it, without scrolling, when an item is picked', () => {
    const opener = document.createElement('button')
    opener.id = 'opener'
    document.body.appendChild(opener)
    opener.focus()
    const onselect = vi.fn()
    const onclose = vi.fn()
    const target = show({ onselect, onclose })
    expect(document.activeElement).not.toBe(opener)

    const calls = focusCalls()
    ;/** @type {HTMLElement} */ (target.querySelectorAll('[role="menuitem"]')[1]).click()
    expect(calls()).toEqual([{ label: 'opener', args: [{ preventScroll: true }] }])
    expect(document.activeElement).toBe(opener)
    expect(onselect).toHaveBeenCalledWith('second')
    expect(onclose).toHaveBeenCalledTimes(1)
  })
})
