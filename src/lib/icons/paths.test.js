import { describe, expect, it } from 'vitest'
import { glyph, iconNames, icons } from './paths.js'

/**
 * The set Task 1 is contracted to ship, and every glyph drawn since. A name
 * added to `paths.js` and not to this list fails the first test below, which is
 * the point: the set is a closed list rather than whatever happens to be in the
 * file.
 */
const REQUIRED = [
  'home', 'pages', 'layers', 'tools', 'eye', 'eye-off', 'pin', 'mask-overlay',
  'settings', 'export', 'sparkle', 'brush', 'shapes', 'wand', 'droplet',
  'stamp', 'zoom-fit', 'zoom-in', 'zoom-out', 'undo', 'redo', 'chevron-left',
  'chevron-right', 'chevron-up', 'chevron-down', 'chevrons-left',
  'chevrons-right', 'close', 'warning-triangle', 'check', 'dot', 'info',
  'trash', 'refresh', 'arrow-up', 'arrow-down', 'folder', 'file', 'plus',
  'search', 'keyboard', 'drag-handle', 'resize-corner', 'more-horizontal',
  'external-link', 'lock', 'cloud', 'cpu', 'help',
  // Added for the colour row's Pick a colour button.
  'eyedropper',
  // The tool bar's glyphs: shape and mode choices drawn as icons, and the
  // bar's own controls.
  'shape-rect', 'shape-ellipse', 'shape-line', 'shape-lasso', 'shape-polygon',
  'book', 'sliders', 'play', 'stop', 'link',
  'link-off', 'bandage',
  // Onboarding and Settings: ratings, and a download's pause.
  'star', 'star-filled', 'pause', 'download',
  // The selection tool on the rail, and its add and remove modes.
  'selection', 'mask-add', 'mask-remove',
  // The language picker.
  'globe',
]

describe('icon set', () => {
  it('ships every required name and nothing extra', () => {
    expect([...iconNames].sort()).toEqual([...REQUIRED].sort())
  })

  it.each(REQUIRED)('%s has at least one drawn sub-path', (name) => {
    const g = glyph(name)
    expect(g.paths.length + g.filled.length).toBeGreaterThan(0)
  })

  it.each(REQUIRED)('%s uses only well-formed path data', (name) => {
    const g = glyph(name)
    for (const d of [...g.paths, ...g.filled]) {
      expect(d, `${name}: sub-path must start with a moveto`).toMatch(/^M/)
      expect(d, `${name}: unexpected command letter`).toMatch(
        /^[MmLlHhVvCcSsQqTtAaZz0-9.,\-\s]+$/
      )
      // Every coordinate lives inside the 16x16 grid, with room for the
      // 0.75 half-stroke. Values outside that range mean a clipped glyph.
      for (const n of d.match(/-?\d*\.?\d+/g) ?? []) {
        expect(Number(n), `${name}: ${d}`).toBeGreaterThanOrEqual(-16)
        expect(Number(n), `${name}: ${d}`).toBeLessThanOrEqual(16)
      }
    }
  })

  it('returns an empty glyph rather than throwing for an unknown name', () => {
    expect(glyph('no-such-icon')).toEqual({ paths: [], filled: [] })
  })

  it('exposes filled sub-paths only where a solid form is wanted', () => {
    const withFill = Object.keys(icons).filter((n) => glyph(n).filled.length > 0)
    expect(withFill.sort()).toEqual(
      ['dot', 'eye', 'eye-off', 'mask-overlay', 'more-horizontal', 'star-filled'].sort()
    )
  })
})
