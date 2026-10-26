import { describe, expect, it } from 'vitest'
import {
  NAVIGATION_PRESETS,
  defaultNavigation,
  navigationPreset,
  normalizeNavigation,
  panStart,
  wheelZooms,
} from './navigation.js'

const trackpad = NAVIGATION_PRESETS.trackpad
const mouse = NAVIGATION_PRESETS.mouse

describe('normalizeNavigation', () => {
  it('starts on the trackpad preset', () => {
    expect(defaultNavigation()).toEqual(trackpad)
    expect(normalizeNavigation(undefined)).toEqual(trackpad)
    expect(normalizeNavigation('mouse')).toEqual(trackpad)
  })

  it('keeps each good field and replaces each bad one on its own', () => {
    expect(normalizeNavigation({ wheel: 'zoom', middlePan: 'yes', emptyPan: true }))
      .toEqual({ wheel: 'zoom', middlePan: true, emptyPan: true })
    expect(normalizeNavigation({ wheel: 'spin', middlePan: false }))
      .toEqual({ wheel: 'scroll', middlePan: false, emptyPan: false })
  })
})

describe('navigationPreset', () => {
  it('names a preset only while every row matches it', () => {
    expect(navigationPreset(trackpad)).toBe('trackpad')
    expect(navigationPreset(mouse)).toBe('mouse')
    expect(navigationPreset({ ...mouse, middlePan: false })).toBeNull()
  })
})

describe('wheelZooms', () => {
  it('zooms on a pinch or a modifier turn whatever the wheel is set to', () => {
    for (const navigation of [trackpad, mouse]) {
      expect(wheelZooms({ deltaY: 4, ctrlKey: true }, navigation)).toBe(true)
      expect(wheelZooms({ deltaY: 4, metaKey: true }, navigation)).toBe(true)
    }
  })

  it('scrolls on a plain turn unless the wheel is set to zoom', () => {
    expect(wheelZooms({ deltaY: 100 }, trackpad)).toBe(false)
    expect(wheelZooms({ deltaY: 100 }, mouse)).toBe(true)
  })

  it('hands a zooming wheel back to the scroller with Shift or Alt, and never zooms sideways', () => {
    expect(wheelZooms({ deltaY: 100, shiftKey: true }, mouse)).toBe(false)
    expect(wheelZooms({ deltaY: 100, altKey: true }, mouse)).toBe(false)
    expect(wheelZooms({ deltaX: 100, deltaY: 0 }, mouse)).toBe(false)
    expect(wheelZooms({ deltaX: 100, deltaY: 20 }, mouse)).toBe(false)
  })
})

describe('panStart', () => {
  /** @param {boolean} control */
  const target = (control) => ({ closest: (selector) => (control && selector !== '[data-canvas-empty]' ? {} : null) })

  it('grabs a middle press while the middle button pans', () => {
    expect(panStart({ button: 1, target: target(true) }, { navigation: trackpad, held: false })).toBe('grab')
    expect(panStart({ button: 1 }, { navigation: { ...trackpad, middlePan: false }, held: false })).toBeNull()
  })

  it('grabs a left press on anything while the pan key is held', () => {
    expect(panStart({ button: 0, target: target(true) }, { navigation: trackpad, held: true })).toBe('grab')
  })

  it('pans a left drag on empty canvas only when asked to, and never one on a control', () => {
    expect(panStart({ button: 0, target: target(false) }, { navigation: trackpad, held: false })).toBeNull()
    expect(panStart({ button: 0, target: target(false) }, { navigation: mouse, held: false })).toBe('empty')
    expect(panStart({ button: 0, target: target(true) }, { navigation: mouse, held: false })).toBeNull()
  })

  it("pans from the page's own empty space, which is a button", () => {
    // Inside both: it is a button, and it is the page's empty space.
    const clear = { closest: () => ({}) }
    expect(panStart({ button: 0, target: clear }, { navigation: mouse, held: false })).toBe('empty')
  })

  it('leaves the right button to its menu', () => {
    expect(panStart({ button: 2, target: target(false) }, { navigation: mouse, held: true })).toBeNull()
  })
})
