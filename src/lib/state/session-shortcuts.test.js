import { describe, it, expect, afterEach, vi } from 'vitest'

import {
  adoptBackendSettings,
  backendSettingsPatch,
  clearShortcutBinding,
  resetAllShortcutBindings,
  resetShortcutBinding,
  sanitizeSession,
  session,
  setShortcutBinding,
} from './session.svelte.js'
import { matchShortcut, shortcuts, SHORTCUTS } from '../shortcuts.js'

const editor = { scope: /** @type {const} */ ('editor') }

/** @param {string} key @param {Object} [extra] */
function press(key, extra = {}) {
  return { key, shiftKey: false, metaKey: false, ctrlKey: false, altKey: false, ...extra }
}

/**
 * The session's shortcut overrides and the table in force are one thing stored
 * in two places on purpose - the session persists them, the table applies them.
 * These are the tests that they cannot part company.
 */
describe('the session’s shortcut overrides', () => {
  afterEach(() => resetAllShortcutBindings())

  it('starts empty, on the shipped table', () => {
    expect(session.shortcuts).toEqual({})
    expect(shortcuts()).toBe(SHORTCUTS)
  })

  it('drops on load what this build cannot honour', () => {
    const record = sanitizeSession({
      shortcuts: {
        'view.maskOverlay': { key: '9' }, //     kept, normalized
        'edit.redo': null, //                    kept: cleared on purpose
        'tool.retouch': { key: 'k' }, //         an id this build does not have
        'edit.undo': { key: 'Tab' }, //          a key that cannot carry a binding
        'app.cancel': { key: 'q' }, //           not the user's to change
        'zoom.fit': 'nonsense', //               not a chord at all
      },
    })
    expect(record.shortcuts).toEqual({
      'view.maskOverlay': { key: '9', shift: false, accel: false },
      'edit.redo': null,
    })
  })

  it('survives a stored record that is not one', () => {
    expect(sanitizeSession({}).shortcuts).toEqual({})
    expect(sanitizeSession({ shortcuts: null }).shortcuts).toEqual({})
    expect(sanitizeSession({ shortcuts: [1, 2] }).shortcuts).toEqual({})
    expect(sanitizeSession(null).shortcuts).toEqual({})
  })

  it('moves the table and the session together on a rebinding', () => {
    expect(setShortcutBinding('view.maskOverlay', { key: '9' })).toEqual({ ok: true })
    expect(session.shortcuts).toEqual({
      'view.maskOverlay': { key: '9', shift: false, accel: false },
    })
    expect(matchShortcut(press('9'), editor)?.id).toBe('view.maskOverlay')
  })

  it('leaves both untouched when a rebinding is refused', () => {
    const refused = setShortcutBinding('view.maskOverlay', { key: 'r' })
    expect(refused.ok).toBe(false)
    expect(session.shortcuts).toEqual({})
    expect(matchShortcut(press('m'), editor)?.id).toBe('view.maskOverlay')
  })

  it('clears and restores one binding', () => {
    expect(clearShortcutBinding('view.maskOverlay')).toEqual({ ok: true })
    expect(session.shortcuts).toEqual({ 'view.maskOverlay': null })
    expect(matchShortcut(press('m'), editor)).toBe(null)
    expect(resetShortcutBinding('view.maskOverlay')).toEqual({ ok: true })
    expect(session.shortcuts).toEqual({})
    expect(matchShortcut(press('m'), editor)?.id).toBe('view.maskOverlay')
  })

  it('sends the differences to the backend, and only those', () => {
    expect(backendSettingsPatch().shortcuts).toEqual({})
    setShortcutBinding('view.maskOverlay', { key: '9' })
    expect(backendSettingsPatch().shortcuts).toEqual({
      'view.maskOverlay': { key: '9', shift: false, accel: false },
    })
    // JSON is the only shape the seam carries, and the patch must survive it.
    const round = JSON.parse(JSON.stringify(backendSettingsPatch().shortcuts))
    expect(round).toEqual(backendSettingsPatch().shortcuts)
  })

  it('adopts the backend’s snapshot, dropping what it cannot honour', () => {
    adoptBackendSettings({
      shortcuts: { 'edit.undo': { key: '8' }, 'tool.retouch': { key: 'k' } },
    })
    expect(session.shortcuts).toEqual({ 'edit.undo': { key: '8', shift: false, accel: false } })
    expect(matchShortcut(press('8'), editor)?.id).toBe('edit.undo')
    expect(matchShortcut(press('u'), editor)).toBe(null)
  })

  it('leaves the bindings alone when the backend says nothing about them', () => {
    setShortcutBinding('view.maskOverlay', { key: '9' })
    adoptBackendSettings({ theme: 'dark' })
    expect(session.shortcuts).toEqual({
      'view.maskOverlay': { key: '9', shift: false, accel: false },
    })
  })
})

describe('windowGesture', () => {
  it('ignores non-primary button presses', async () => {
    const { windowGesture } = await import('../editor/windowgesture.js')
    const target = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() }
    const { onGestureStart } = windowGesture({ id: () => 'pages', measuredHeight: () => 200 })
    const preventDefault = vi.fn()
    onGestureStart(
      /** @type {any} */ ({ button: 2, pointerId: 1, currentTarget: target, preventDefault }),
      'move',
    )
    expect(preventDefault).not.toHaveBeenCalled()
    expect(target.setPointerCapture).not.toHaveBeenCalled()
  })

  it('guards against a second pointer while a drag is live', async () => {
    const { windowGesture } = await import('../editor/windowgesture.js')
    const target = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() }
    const { onGestureStart, onGestureMove, onGestureEnd } = windowGesture({
      id: () => 'pages',
      measuredHeight: () => 200,
    })
    session.windows.pages.x = 100
    session.windows.pages.y = 100

    // Pointer 1 starts drag
    onGestureStart(
      /** @type {any} */ ({
        button: 0,
        pointerId: 1,
        clientX: 100,
        clientY: 100,
        currentTarget: target,
        preventDefault: vi.fn(),
      }),
      'move',
    )
    expect(target.setPointerCapture).toHaveBeenCalledWith(1)

    // Pointer 2 attempts to start drag while pointer 1 is live
    const target2 = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() }
    onGestureStart(
      /** @type {any} */ ({
        button: 0,
        pointerId: 2,
        clientX: 500,
        clientY: 500,
        currentTarget: target2,
        preventDefault: vi.fn(),
      }),
      'move',
    )
    expect(target2.setPointerCapture).not.toHaveBeenCalled()

    // Pointer 2 moves - should be ignored
    onGestureMove(/** @type {any} */ ({ pointerId: 2, clientX: 600, clientY: 600 }))
    expect(session.windows.pages.x).toBe(100)

    // Pointer 1 moves - should move window
    onGestureMove(/** @type {any} */ ({ pointerId: 1, clientX: 120, clientY: 100 }))
    expect(session.windows.pages.x).toBe(120)

    // Pointer 2 releases - should be ignored
    onGestureEnd(/** @type {any} */ ({ pointerId: 2, currentTarget: target2 }))
    expect(target2.releasePointerCapture).not.toHaveBeenCalled()

    // Pointer 1 releases - ends gesture, releases capture on original target
    onGestureEnd(/** @type {any} */ ({ pointerId: 1, currentTarget: target }))
    expect(target.releasePointerCapture).toHaveBeenCalledWith(1)
  })

  it('releases capture, clears gesture and resets window on Escape', async () => {
    const { windowGesture } = await import('../editor/windowgesture.js')
    const target = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() }
    const { onGestureStart, onGestureKey } = windowGesture({
      id: () => 'pages',
      measuredHeight: () => 200,
    })
    session.windows.pages.x = 100
    onGestureStart(
      /** @type {any} */ ({
        button: 0,
        pointerId: 1,
        clientX: 100,
        clientY: 100,
        currentTarget: target,
        preventDefault: vi.fn(),
      }),
      'move',
    )
    onGestureKey(
      /** @type {any} */ ({
        key: 'Escape',
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      }),
      'move',
    )
    expect(target.releasePointerCapture).toHaveBeenCalledWith(1)
    expect(session.windows.pages.x).toBe(16)
  })

  it('cleans up in-flight gesture on destroy', async () => {
    const { windowGesture } = await import('../editor/windowgesture.js')
    const target = { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() }
    const gesture = windowGesture({ id: () => 'pages', measuredHeight: () => 200 })
    session.windows.pages.x = 100
    gesture.onGestureStart(
      /** @type {any} */ ({
        button: 0,
        pointerId: 1,
        clientX: 100,
        clientY: 100,
        currentTarget: target,
        preventDefault: vi.fn(),
      }),
      'move',
    )
    gesture.destroy?.()
    expect(target.releasePointerCapture).toHaveBeenCalledWith(1)
  })
})
