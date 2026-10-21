/**
 * `captureFocus` gives focus back without scrolling, on each of its three
 * ways back: the element itself, the fallback, and the old parent.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { captureFocus } from './focus.js'

afterEach(() => {
  document.body.replaceChildren()
  vi.restoreAllMocks()
})

/**
 * @param {HTMLElement} parent
 * @param {string} id
 */
function button(parent, id) {
  const el = document.createElement('button')
  el.id = id
  parent.appendChild(el)
  return el
}

/** The elements `focus` was called on, with the options each call got. */
function focusCalls() {
  const spy = vi.spyOn(HTMLElement.prototype, 'focus')
  return {
    spy,
    calls: () => spy.mock.calls.map((args, i) => ({ id: /** @type {HTMLElement} */ (spy.mock.contexts[i]).id, args })),
  }
}

describe('captureFocus restore', () => {
  it('refocuses the previous element without scrolling', () => {
    const previous = button(document.body, 'previous')
    previous.focus()
    const restore = captureFocus()
    button(document.body, 'other').focus()
    const { calls } = focusCalls()
    restore()
    expect(calls()).toEqual([{ id: 'previous', args: [{ preventScroll: true }] }])
    expect(document.activeElement).toBe(previous)
  })

  it('focuses the fallback\'s first focusable descendant without scrolling when the element is gone', () => {
    const container = document.createElement('div')
    document.body.appendChild(container)
    const previous = button(container, 'previous')
    previous.focus()
    const restore = captureFocus(() => container)
    previous.remove()
    const next = button(container, 'next')
    Object.defineProperty(next, 'offsetParent', { value: container, configurable: true })
    const { calls } = focusCalls()
    restore()
    expect(calls()).toEqual([{ id: 'next', args: [{ preventScroll: true }] }])
  })

  it('focuses a fallback with nothing focusable in it, itself, without scrolling', () => {
    const previous = button(document.body, 'previous')
    previous.focus()
    const target = document.createElement('div')
    target.id = 'target'
    target.tabIndex = -1
    document.body.appendChild(target)
    const restore = captureFocus(target)
    previous.remove()
    const { calls } = focusCalls()
    restore()
    expect(calls()).toEqual([{ id: 'target', args: [{ preventScroll: true }] }])
  })

  it('falls back to the old parent\'s first focusable child without scrolling', () => {
    const parent = document.createElement('div')
    document.body.appendChild(parent)
    const previous = button(parent, 'previous')
    previous.focus()
    const sibling = button(parent, 'sibling')
    Object.defineProperty(sibling, 'offsetParent', { value: parent, configurable: true })
    const restore = captureFocus()
    previous.remove()
    const { calls } = focusCalls()
    restore()
    expect(calls()).toEqual([{ id: 'sibling', args: [{ preventScroll: true }] }])
  })
})
