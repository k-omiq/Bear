import { describe, it, expect, afterEach } from 'vitest'
import {
  SHORTCUTS,
  matchShortcut,
  normalizeKey,
  isTextEntry,
  handlesKeyNatively,
  shortcutLabelKey,
  shortcutChord,
  shortcutConflicts,
  groupedShortcuts,
  SHORTCUT_GROUPS,
  DEFAULT_POINTER_MODIFIER,
  POINTER_MODIFIERS,
  chordCaps,
  chordConflict,
  modifierCap,
  modifierHeld,
  pointerModifiersFor,
  normalizePointerModifier,
  chordFromEvent,
  isRebound,
  parseChord,
  rebindShortcut,
  releasesHoldOriginal,
  resetAllShortcuts,
  resetShortcut,
  sanitizeShortcutOverrides,
  setShortcutOverrides,
  shortcutOverrides,
  shortcuts,
  setDialogOutsideStack,
  isDialogOutsideStackOpen,
  shortcutsRevision,
  unbindShortcut,
} from './shortcuts.js'

/** @param {string} key @param {Object} [extra] */
function press(key, extra = {}) {
  return {
    key,
    shiftKey: false,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    target: null,
    ...extra,
  }
}

const editor = { scope: /** @type {const} */ ('editor') }
const home = { scope: /** @type {const} */ ('home') }

describe('the table', () => {
  it('has no key claimed twice in overlapping scopes', () => {
    expect(shortcutConflicts()).toEqual([])
  })

  it('gives every entry a label key, a chord and a known group', () => {
    for (const shortcut of SHORTCUTS) {
      expect(shortcut.labelKey, shortcut.id).toMatch(/^[a-z]+(\.[a-zA-Z]+)+$/)
      expect(shortcut.chord.length, shortcut.id).toBeGreaterThan(0)
      expect(shortcut.keys.length, shortcut.id).toBeGreaterThan(0)
      expect(SHORTCUT_GROUPS, shortcut.id).toContain(shortcut.group)
      expect(typeof shortcut.run, shortcut.id).toBe('function')
    }
  })

  it('has unique ids', () => {
    const ids = SHORTCUTS.map((s) => s.id)
    expect(new Set(ids).size).toBe(ids.length)
  })
})

describe('matchShortcut', () => {
  it('matches editor keys only in the editor', () => {
    expect(matchShortcut(press('1'), editor)?.id).toBe('tool.autoClean')
    expect(matchShortcut(press('1'), home)).toBe(null)
  })

  it('selects the six tools with 1 to 6, in rail order', () => {
    const ids = ['autoClean', 'brush', 'shapes', 'aiMaskBrush', 'cloneHeal', 'maskSelect']
    for (const [index, id] of ids.entries()) {
      const shortcut = matchShortcut(press(String(index + 1)), editor)
      expect(shortcut?.id).toBe(`tool.${id}`)
      expect(shortcut?.labelKey).toBe(`tools.name.${id}`)
      const slots = []
      shortcut?.run(/** @type {any} */ ({ selectToolSlot: (slot) => slots.push(slot) }))
      expect(slots).toEqual([index + 1])
    }
    expect(matchShortcut(press('6'), home)).toBe(null)
  })

  it('gives the selection tool S for its shape and X for add or remove', () => {
    const calls = []
    const commands = /** @type {any} */ ({
      cycleMaskSelectShape: () => calls.push('shape'),
      toggleMaskSelectMode: () => calls.push('mode'),
    })
    matchShortcut(press('s'), editor)?.run(commands)
    matchShortcut(press('x'), editor)?.run(commands)
    expect(calls).toEqual(['shape', 'mode'])
    expect(matchShortcut(press('s'), home)).toBe(null)
  })

  it('separates N by scope - new project at home, next issue in the editor', () => {
    expect(matchShortcut(press('n'), home)?.id).toBe('app.newProject')
    expect(matchShortcut(press('n'), editor)?.id).toBe('review.next')
  })

  it('treats shifted letters as their own bindings', () => {
    expect(matchShortcut(press('u'), editor)?.id).toBe('edit.undo')
    expect(matchShortcut(press('U', { shiftKey: true }), editor)?.id).toBe('edit.redo')
    expect(matchShortcut(press('o'), editor)?.id).toBe('view.holdOriginal')
    expect(matchShortcut(press('O', { shiftKey: true }), editor)?.id).toBe('view.pinOriginal')
  })

  it('decides Shift by the modifier, not by letter case - Caps Lock must not swap a binding', () => {
    // Caps Lock on, no Shift: the browser reports 'O' but Shift is up.
    expect(matchShortcut(press('O'), editor)?.id).toBe('view.holdOriginal')
    // Caps Lock on, Shift held: the browser reports 'o' but Shift is down.
    expect(matchShortcut(press('o', { shiftKey: true }), editor)?.id).toBe('view.pinOriginal')
    expect(matchShortcut(press('U'), editor)?.id).toBe('edit.undo')
    expect(matchShortcut(press('u', { shiftKey: true }), editor)?.id).toBe('edit.redo')
  })

  it('matches unshifted letter bindings whatever the Shift state', () => {
    expect(matchShortcut(press('m'), editor)?.id).toBe('view.maskOverlay')
    expect(matchShortcut(press('M', { shiftKey: true }), editor)?.id).toBe('view.maskOverlay')
    expect(matchShortcut(press('N', { shiftKey: true }), editor)?.id).toBe('review.next')
  })

  it('leaves modifier chords to the platform', () => {
    expect(matchShortcut(press('e', { metaKey: true }), editor)).toBe(null)
    expect(matchShortcut(press('e', { ctrlKey: true }), editor)).toBe(null)
    expect(matchShortcut(press('e', { altKey: true }), editor)).toBe(null)
    expect(matchShortcut(press('Escape', { metaKey: true }), editor)).toBe(null)
  })

  it('fires nothing but Escape while a dialog is open', () => {
    const open = { ...editor, modalOpen: true }
    expect(matchShortcut(press('1'), open)).toBe(null)
    expect(matchShortcut(press(','), open)).toBe(null)
    expect(matchShortcut(press('Escape'), open)?.id).toBe('app.cancel')
  })

  it('fires nothing at all while a dialog beside the modal stack is open', () => {
    // The first-launch offer is mounted outside `app.modals`, so
    // `isModalOpen()` is false while it fills the screen - and `,` opened a
    // Settings dialog *underneath* its backdrop, `N` and `H` navigated the
    // screen behind it, and the App-level guard then unmounted the offer with
    // a download still running.
    setDialogOutsideStack(true)
    expect(isDialogOutsideStackOpen()).toBe(true)
    expect(matchShortcut(press('1'), editor)).toBe(null)
    expect(matchShortcut(press(','), editor)).toBe(null)
    expect(matchShortcut(press('n'), home)).toBe(null)
    expect(matchShortcut(press('h'), editor)).toBe(null)
    expect(matchShortcut(press('o', { metaKey: true }), home)).toBe(null)
    // Escape too, and this is the one that differs from a stacked dialog: the
    // layer defers to a mounted `Modal` by asking `isModalOpen()`, which
    // cannot see this one, so letting Escape through would cancel an editor
    // interaction behind a dialog that is closing itself on the same press.
    expect(matchShortcut(press('Escape'), editor)).toBe(null)

    setDialogOutsideStack(false)
    expect(isDialogOutsideStackOpen()).toBe(false)
    expect(matchShortcut(press(','), editor)?.id).toBe('app.settings')
  })

  it('fires nothing but Escape while a text field has focus', () => {
    const input = press('m', { target: { tagName: 'INPUT', type: 'text' } })
    expect(matchShortcut(input, editor)).toBe(null)
    const escape = press('Escape', { target: { tagName: 'INPUT', type: 'text' } })
    expect(matchShortcut(escape, editor)?.id).toBe('app.cancel')
  })

  it('accepts the alternate keys the prototype bound', () => {
    expect(matchShortcut(press('='), editor)?.id).toBe('zoom.in')
    expect(matchShortcut(press('['), editor)?.id).toBe('page.left')
    expect(matchShortcut(press(']'), editor)?.id).toBe('page.right')
  })

  it('runs global entries in both scopes', () => {
    expect(matchShortcut(press(','), home)?.id).toBe('app.settings')
    expect(matchShortcut(press(','), editor)?.id).toBe('app.settings')
    expect(matchShortcut(press('?'), home)?.id).toBe('app.shortcutSheet')
  })

  it('matches ⌘O and Ctrl+O at home, and only there', () => {
    expect(matchShortcut(press('o', { metaKey: true }), home)?.id).toBe('app.openProject')
    expect(matchShortcut(press('o', { ctrlKey: true }), home)?.id).toBe('app.openProject')
    expect(matchShortcut(press('O', { metaKey: true }), home)?.id).toBe('app.openProject')
    expect(matchShortcut(press('o', { metaKey: true }), editor)).toBe(null)
  })

  it('keeps the chord and the bare key apart', () => {
    // `O` in the editor is hold-original and must not be reachable by chord;
    // `⌘O` at home must not be reachable without the modifier.
    expect(matchShortcut(press('o'), home)).toBe(null)
    expect(matchShortcut(press('o', { metaKey: true, altKey: true }), home)).toBe(null)
    expect(matchShortcut(press('o'), editor)?.id).toBe('view.holdOriginal')
  })

  it('lets a chord through a text field, which a bare letter never is', () => {
    const field = { tagName: 'INPUT', type: 'text' }
    expect(matchShortcut(press('o', { metaKey: true, target: field }), home)?.id).toBe(
      'app.openProject'
    )
    expect(matchShortcut(press('o', { target: field }), home)).toBe(null)
  })

  it('ignores a chord while a dialog is open', () => {
    expect(matchShortcut(press('o', { metaKey: true }), { ...home, modalOpen: true })).toBe(null)
  })

  it('matches ⌘⌫ and Ctrl+⌫ in the editor, and only there', () => {
    expect(matchShortcut(press('Backspace', { metaKey: true }), editor)?.id).toBe(
      'layers.deleteSelected'
    )
    expect(matchShortcut(press('Backspace', { ctrlKey: true }), editor)?.id).toBe(
      'layers.deleteSelected'
    )
    expect(matchShortcut(press('Backspace', { metaKey: true }), home)).toBe(null)
  })

  it('leaves a bare Backspace to whatever has the focus', () => {
    // The Layers list answers it on a focused row (`MaskList`), and nothing
    // else in the editor may claim it: a destructive act on an unmodified key
    // is one a fumbled keystroke performs.
    expect(matchShortcut(press('Backspace'), editor)).toBe(null)
    expect(matchShortcut(press('Delete'), editor)).toBe(null)
  })

  it('keeps ⌘⌫ out of a text field, where the platform already spends it', () => {
    // The one exception to "a chord always gets through a field": ⌘⌫ there is
    // delete-to-start-of-line, and eating it would be eating the user's text.
    const field = { tagName: 'INPUT', type: 'text' }
    expect(matchShortcut(press('Backspace', { metaKey: true, target: field }), editor)).toBe(null)
    expect(
      matchShortcut(press('Backspace', { metaKey: true }), { ...editor, textEntry: true })
    ).toBe(null)
    // And it is the one entry that opts out - ⌘O still reaches a field.
    expect(matchShortcut(press('o', { metaKey: true, target: field }), home)?.id).toBe(
      'app.openProject'
    )
  })

  it('returns null for an unbound key and for a malformed event', () => {
    expect(matchShortcut(press('q'), editor)).toBe(null)
    expect(matchShortcut(null, editor)).toBe(null)
    expect(matchShortcut({}, editor)).toBe(null)
  })
})

describe('normalizeKey', () => {
  it('lower-cases single characters and leaves named keys alone', () => {
    expect(normalizeKey('O')).toBe('o')
    expect(normalizeKey('?')).toBe('?')
    expect(normalizeKey('ArrowLeft')).toBe('ArrowLeft')
    expect(normalizeKey('Escape')).toBe('Escape')
  })
})

describe('isTextEntry', () => {
  it('recognises the places a keystroke belongs to the user', () => {
    expect(isTextEntry({ tagName: 'INPUT', type: 'text' })).toBe(true)
    expect(isTextEntry({ tagName: 'input', type: 'search' })).toBe(true)
    expect(isTextEntry({ tagName: 'TEXTAREA' })).toBe(true)
    expect(isTextEntry({ isContentEditable: true, tagName: 'DIV' })).toBe(true)
  })

  it('does not treat buttons, checkboxes or ranges as text entry', () => {
    expect(isTextEntry({ tagName: 'BUTTON' })).toBe(false)
    expect(isTextEntry({ tagName: 'INPUT', type: 'checkbox' })).toBe(false)
    expect(isTextEntry({ tagName: 'INPUT', type: 'range' })).toBe(false)
    expect(isTextEntry(null)).toBe(false)
  })

  // A picker holds no text, so it can never eat a character the user meant to
  // type; what it does own is `handlesKeyNatively`'s narrower list.
  it('no longer counts a picker as somewhere the user is typing', () => {
    expect(isTextEntry({ tagName: 'SELECT' })).toBe(false)
  })
})

describe('handlesKeyNatively', () => {
  const range = { tagName: 'INPUT', type: 'range' }

  it('gives a range input its own arrows, Home and End', () => {
    expect(handlesKeyNatively(range, 'ArrowLeft')).toBe(true)
    expect(handlesKeyNatively(range, 'ArrowRight')).toBe(true)
    expect(handlesKeyNatively(range, 'Home')).toBe(true)
    expect(handlesKeyNatively(range, 'End')).toBe(true)
  })

  it('leaves every other key to the table', () => {
    expect(handlesKeyNatively(range, 'm')).toBe(false)
    expect(handlesKeyNatively(range, 'Escape')).toBe(false)
    expect(handlesKeyNatively({ tagName: 'BUTTON' }, 'ArrowLeft')).toBe(false)
    expect(handlesKeyNatively(null, 'ArrowLeft')).toBe(false)
  })

  it('gives a picker its own arrows, its ends, Enter, Space and its type-ahead', () => {
    const select = { tagName: 'SELECT' }
    for (const key of ['ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown', 'Enter', ' ']) {
      expect(handlesKeyNatively(select, key)).toBe(true)
    }
    expect(handlesKeyNatively(select, 'm')).toBe(true)
    expect(handlesKeyNatively(select, 'f')).toBe(true)
  })

  it("leaves a picker's digits to the table, and asks nothing of a disabled one", () => {
    const select = { tagName: 'SELECT' }
    for (const key of ['1', '2', '3', '4', '5', '6']) {
      expect(handlesKeyNatively(select, key)).toBe(false)
    }
    expect(handlesKeyNatively({ tagName: 'SELECT', disabled: true }, 'ArrowDown')).toBe(false)
  })

  // A picker held the keyboard where the chips it replaced did not: the tool
  // window's engine rows were the case that found it, and a Layers row's
  // picker is the one that remains.
  it('lets 1-6 through a focused picker and keeps the keys the picker answers', () => {
    const select = { tagName: 'SELECT' }
    expect(matchShortcut(press('1', { target: select }), editor)?.id).toBe('tool.autoClean')
    expect(matchShortcut(press('5', { target: select }), editor)?.id).toBe('tool.cloneHeal')
    expect(matchShortcut(press('6', { target: select }), editor)?.id).toBe('tool.maskSelect')
    expect(matchShortcut(press('ArrowLeft', { target: select }), editor)).toBeNull()
    expect(matchShortcut(press('m', { target: select }), editor)).toBeNull()
  })

  // The wipe slider and every tool slider used to page the chapter instead of
  // moving: the layer called `preventDefault` before the input ever saw it.
  it('keeps the shortcut layer off a focused slider’s arrows, and only those', () => {
    expect(matchShortcut(press('ArrowLeft', { target: range }), editor)).toBeNull()
    expect(matchShortcut(press('m', { target: range }), editor)?.id).toBe('view.maskOverlay')
    expect(matchShortcut(press('ArrowLeft'), editor)?.id).toBe('page.left')
  })
})

describe('shortcutLabelKey', () => {
  it('resolves the arrow keys against the reading direction', () => {
    const left = SHORTCUTS.find((s) => s.id === 'page.left')
    const right = SHORTCUTS.find((s) => s.id === 'page.right')
    expect(shortcutLabelKey(left, { readingDirection: 'rtl' })).toBe('paging.action.next')
    expect(shortcutLabelKey(right, { readingDirection: 'rtl' })).toBe('paging.action.prev')
    expect(shortcutLabelKey(left, { readingDirection: 'ltr' })).toBe('paging.action.prev')
    expect(shortcutLabelKey(right, { readingDirection: 'ltr' })).toBe('paging.action.next')
  })

  it('falls back to the static label for everything else', () => {
    const undo = SHORTCUTS.find((s) => s.id === 'edit.undo')
    expect(shortcutLabelKey(undo)).toBe('shortcuts.edit.undo')
  })
})

describe('shortcutChord', () => {
  it('names the command modifier per platform, and leaves every other entry alone', () => {
    const open = SHORTCUTS.find((s) => s.id === 'app.openProject')
    expect(shortcutChord(open, { apple: true })).toEqual(['⌘', 'O'])
    expect(shortcutChord(open, { apple: false })).toEqual(['Ctrl', 'O'])
    const undo = SHORTCUTS.find((s) => s.id === 'edit.undo')
    expect(shortcutChord(undo, { apple: true })).toEqual(['U'])
  })

  it('draws the delete-layer chord with the erase keycap on both platforms', () => {
    const del = SHORTCUTS.find((s) => s.id === 'layers.deleteSelected')
    expect(shortcutChord(del, { apple: true })).toEqual(['⌘', '⌫'])
    expect(shortcutChord(del, { apple: false })).toEqual(['Ctrl', '⌫'])
  })
})

describe('groupedShortcuts', () => {
  it('covers every entry exactly once and keeps group order', () => {
    const sections = groupedShortcuts()
    const ids = sections.flatMap((section) => section.shortcuts.map((s) => s.id))
    expect(ids.length).toBe(SHORTCUTS.length)
    expect(new Set(ids).size).toBe(SHORTCUTS.length)
    const order = sections.map((section) => section.group)
    expect(order).toEqual([...order].sort((a, b) => SHORTCUT_GROUPS.indexOf(a) - SHORTCUT_GROUPS.indexOf(b)))
  })

  it('filters to a scope, keeping global entries', () => {
    const homeIds = groupedShortcuts({ scope: 'home' }).flatMap((s) => s.shortcuts.map((x) => x.id))
    expect(homeIds).toContain('app.newProject')
    expect(homeIds).toContain('app.settings')
    expect(homeIds).not.toContain('tool.brush')
  })
})

/* ------------------------------------------------------------------ */
/* Rebinding                                                           */
/* ------------------------------------------------------------------ */

describe('rebinding', () => {
  // The override layer is module-level state: every test here starts from the
  // shipped defaults, whatever the one before it bound.
  afterEach(() => resetAllShortcuts())

  describe('parseChord', () => {
    it('accepts a chord and normalizes its key', () => {
      expect(parseChord({ key: 'M', shift: false, accel: false })).toEqual({
        key: 'm',
        shift: false,
        accel: false,
      })
      expect(parseChord({ key: 'ArrowUp', shift: true, accel: true })).toEqual({
        key: 'ArrowUp',
        shift: true,
        accel: true,
      })
    })

    it('defaults the two modifiers rather than trusting them', () => {
      expect(parseChord({ key: 'k' })).toEqual({ key: 'k', shift: false, accel: false })
      expect(parseChord({ key: 'k', shift: 'yes', accel: 1 })).toEqual({
        key: 'k',
        shift: false,
        accel: false,
      })
    })

    it('refuses anything that is not a key a user can press again', () => {
      expect(parseChord(null)).toBe(null)
      expect(parseChord('m')).toBe(null)
      expect(parseChord([])).toBe(null)
      expect(parseChord({})).toBe(null)
      expect(parseChord({ key: '' })).toBe(null)
      expect(parseChord({ key: 42 })).toBe(null)
      expect(parseChord({ key: 'Escape' })).toBe(null)
      expect(parseChord({ key: 'Tab' })).toBe(null)
      expect(parseChord({ key: 'Backspace' })).toBe(null)
      expect(parseChord({ key: 'Delete' })).toBe(null)
      expect(parseChord({ key: 'Shift' })).toBe(null)
    })

    it('round-trips through JSON unchanged', () => {
      const chord = { key: 'j', shift: true, accel: true }
      expect(parseChord(JSON.parse(JSON.stringify(chord)))).toEqual(chord)
    })
  })

  describe('chordFromEvent', () => {
    it('records the key, the shift state and the platform modifier', () => {
      expect(chordFromEvent(press('K')).chord).toEqual({ key: 'k', shift: false, accel: false })
      expect(chordFromEvent(press('k', { shiftKey: true })).chord).toEqual({
        key: 'k',
        shift: true,
        accel: false,
      })
      expect(chordFromEvent(press('k', { metaKey: true })).chord).toEqual({
        key: 'k',
        shift: false,
        accel: true,
      })
      expect(chordFromEvent(press('k', { ctrlKey: true })).chord?.accel).toBe(true)
    })

    it('keeps listening through a modifier pressed on its own', () => {
      expect(chordFromEvent(press('Shift', { shiftKey: true }))).toEqual({
        chord: null,
        reasonKey: null,
      })
      expect(chordFromEvent(press('Meta', { metaKey: true })).reasonKey).toBe(null)
    })

    it('refuses an Alt chord, which the table can never match', () => {
      expect(chordFromEvent(press('k', { altKey: true }))).toEqual({
        chord: null,
        reasonKey: 'shortcuts.rebind.refusedAlt',
      })
    })

    it('refuses a key that cannot carry a binding', () => {
      expect(chordFromEvent(press('Escape')).reasonKey).toBe('shortcuts.rebind.refusedKey')
      expect(chordFromEvent(press('Tab')).reasonKey).toBe('shortcuts.rebind.refusedKey')
    })
  })

  describe('chordCaps', () => {
    it('names the command modifier per platform', () => {
      const chord = { key: 'k', shift: true, accel: true }
      expect(chordCaps(chord, { apple: true })).toEqual(['⌘', 'Shift', 'K'])
      expect(chordCaps(chord, { apple: false })).toEqual(['Ctrl', 'Shift', 'K'])
    })

    it('draws the named keys as their caps', () => {
      expect(chordCaps({ key: 'ArrowLeft', shift: false, accel: false })).toEqual(['←'])
      expect(chordCaps({ key: 'F1', shift: false, accel: false })).toEqual(['F1'])
      expect(chordCaps(null)).toEqual([])
    })
  })

  describe('sanitizeShortcutOverrides', () => {
    it('keeps a real rebinding and a cleared one', () => {
      const kept = sanitizeShortcutOverrides({
        'view.maskOverlay': { key: 'K', shift: false, accel: false },
        'edit.redo': null,
      })
      expect(kept).toEqual({
        'view.maskOverlay': { key: 'k', shift: false, accel: false },
        'edit.redo': null,
      })
    })

    it('drops an id this build no longer has', () => {
      expect(sanitizeShortcutOverrides({ 'tool.retouch': { key: 'k' } })).toEqual({})
    })

    it('drops a malformed chord rather than the whole record', () => {
      expect(
        sanitizeShortcutOverrides({
          'view.maskOverlay': { key: 'Tab' },
          'edit.undo': { key: 'k' },
        })
      ).toEqual({ 'edit.undo': { key: 'k', shift: false, accel: false } })
      expect(sanitizeShortcutOverrides('nonsense')).toEqual({})
      expect(sanitizeShortcutOverrides(null)).toEqual({})
      expect(sanitizeShortcutOverrides([{ key: 'k' }])).toEqual({})
    })

    it('drops an entry whose chord is only its default written out', () => {
      expect(sanitizeShortcutOverrides({ 'view.maskOverlay': { key: 'm' } })).toEqual({})
      expect(sanitizeShortcutOverrides({ 'edit.redo': { key: 'u', shift: true } })).toEqual({})
      // Not the default: `edit.redo` wants Shift.
      expect(sanitizeShortcutOverrides({ 'edit.redo': { key: 'u' } })).not.toEqual({})
    })

    it('refuses to touch the one entry that is not the user’s to change', () => {
      expect(sanitizeShortcutOverrides({ 'app.cancel': { key: 'q' } })).toEqual({})
      expect(sanitizeShortcutOverrides({ 'app.cancel': null })).toEqual({})
    })
  })

  describe('the table in force', () => {
    it('is the default table until something is bound', () => {
      expect(shortcuts()).toBe(SHORTCUTS)
      expect(shortcutOverrides()).toEqual({})
    })

    it('answers the new chord and no longer the old one', () => {
      setShortcutOverrides({ 'view.maskOverlay': { key: 'k', shift: false, accel: false } })
      expect(matchShortcut(press('k'), editor)?.id).toBe('view.maskOverlay')
      expect(matchShortcut(press('m'), editor)).toBe(null)
    })

    it('narrows a rebound entry to exactly the combination recorded', () => {
      setShortcutOverrides({ 'view.maskOverlay': { key: 'k', shift: false, accel: false } })
      // The default `m` matched either Shift state; `k` was recorded Shift-up.
      expect(matchShortcut(press('K', { shiftKey: true }), editor)).toBe(null)
    })

    it('carries an accel chord through', () => {
      setShortcutOverrides({ 'edit.undo': { key: 'z', shift: false, accel: true } })
      expect(matchShortcut(press('z', { metaKey: true }), editor)?.id).toBe('edit.undo')
      expect(matchShortcut(press('z'), editor)?.id).toBe('zoom.actual')
    })

    it('matches nothing for an unbound entry, and keeps its row in the sheet', () => {
      setShortcutOverrides({ 'view.maskOverlay': null })
      expect(matchShortcut(press('m'), editor)).toBe(null)
      const ids = groupedShortcuts().flatMap((s) => s.shortcuts.map((x) => x.id))
      expect(ids).toContain('view.maskOverlay')
      const row = shortcuts().find((s) => s.id === 'view.maskOverlay')
      expect(row.unbound).toBe(true)
      expect(shortcutChord(row, { apple: true })).toEqual([])
    })

    it('renders a rebound entry’s chord from the chord itself', () => {
      setShortcutOverrides({ 'app.openProject': { key: 'p', shift: true, accel: true } })
      const row = shortcuts().find((s) => s.id === 'app.openProject')
      expect(shortcutChord(row, { apple: true })).toEqual(['⌘', 'Shift', 'P'])
      expect(shortcutChord(row, { apple: false })).toEqual(['Ctrl', 'Shift', 'P'])
    })

    it('leaves the table sound, and says so', () => {
      setShortcutOverrides({ 'view.maskOverlay': { key: 'k', shift: false, accel: false } })
      expect(shortcutConflicts()).toEqual([])
    })

    it('does not mutate the defaults', () => {
      setShortcutOverrides({ 'view.maskOverlay': { key: 'k', shift: false, accel: false } })
      expect(SHORTCUTS.find((s) => s.id === 'view.maskOverlay').keys).toEqual(['m'])
    })
  })

  describe('chordConflict', () => {
    it('names the entry already holding the combination', () => {
      const taken = chordConflict('view.maskOverlay', { key: 'r', shift: false, accel: false })
      expect(taken?.id).toBe('view.reviewFilter')
    })

    it('lets two scopes that never overlap share a key', () => {
      // `app.newProject` is home-only; `review.next` is editor-only.
      expect(chordConflict('app.newProject', { key: 'p', shift: false, accel: false })).toBe(null)
    })

    it('counts a global entry as overlapping both scopes', () => {
      const taken = chordConflict('review.next', { key: ',', shift: false, accel: false })
      expect(taken?.id).toBe('app.settings')
    })

    it('keeps a chord and a bare key apart', () => {
      // `O` in the editor is hold-original; `⌘O` is a different press.
      expect(chordConflict('edit.undo', { key: 'o', shift: false, accel: true })).toBe(null)
      expect(chordConflict('edit.undo', { key: 'o', shift: false, accel: false })?.id).toBe(
        'view.holdOriginal'
      )
    })

    it('ignores the entry being rebound, and every unbound one', () => {
      expect(chordConflict('view.maskOverlay', { key: 'm', shift: false, accel: false })).toBe(null)
      setShortcutOverrides({ 'view.reviewFilter': null })
      expect(chordConflict('view.maskOverlay', { key: 'r', shift: false, accel: false })).toBe(null)
    })
  })

  describe('rebindShortcut', () => {
    it('binds, and reports the differences and nothing else', () => {
      expect(rebindShortcut('view.maskOverlay', { key: 'K' })).toEqual({ ok: true })
      expect(shortcutOverrides()).toEqual({
        'view.maskOverlay': { key: 'k', shift: false, accel: false },
      })
      expect(isRebound('view.maskOverlay')).toBe(true)
      expect(isRebound('edit.undo')).toBe(false)
    })

    it('refuses a collision and names what it collided with', () => {
      expect(rebindShortcut('view.maskOverlay', { key: 'r' })).toEqual({
        ok: false,
        reasonKey: 'shortcuts.rebind.refusedConflict',
        conflictId: 'view.reviewFilter',
      })
      expect(shortcutOverrides()).toEqual({})
    })

    it('refuses the fixed entry, and refuses to unbind it', () => {
      expect(rebindShortcut('app.cancel', { key: 'q' }).reasonKey).toBe(
        'shortcuts.rebind.refusedFixed'
      )
      expect(unbindShortcut('app.cancel').reasonKey).toBe('shortcuts.rebind.refusedFixed')
      expect(matchShortcut(press('Escape'), editor)?.id).toBe('app.cancel')
    })

    it('refuses an id this build does not have, and a chord it cannot use', () => {
      expect(rebindShortcut('tool.retouch', { key: 'k' }).reasonKey).toBe(
        'shortcuts.rebind.refusedUnknown'
      )
      expect(rebindShortcut('edit.undo', { key: 'Tab' }).reasonKey).toBe(
        'shortcuts.rebind.refusedKey'
      )
    })

    it('stores nothing when the chord recorded is the default', () => {
      expect(rebindShortcut('view.maskOverlay', { key: 'm' })).toEqual({ ok: true })
      expect(shortcutOverrides()).toEqual({})
    })

    it('frees the key it moved off, so two entries can be swapped by hand', () => {
      expect(rebindShortcut('view.maskOverlay', { key: 'k' }).ok).toBe(true)
      expect(rebindShortcut('view.reviewFilter', { key: 'm' }).ok).toBe(true)
      expect(matchShortcut(press('m'), editor)?.id).toBe('view.reviewFilter')
      expect(matchShortcut(press('k'), editor)?.id).toBe('view.maskOverlay')
      expect(matchShortcut(press('r'), editor)).toBe(null)
    })
  })

  describe('unbind and reset', () => {
    it('clears one entry and gives it back', () => {
      expect(unbindShortcut('view.maskOverlay')).toEqual({ ok: true })
      expect(shortcutOverrides()).toEqual({ 'view.maskOverlay': null })
      expect(matchShortcut(press('m'), editor)).toBe(null)
      expect(resetShortcut('view.maskOverlay')).toEqual({ ok: true })
      expect(shortcutOverrides()).toEqual({})
      expect(matchShortcut(press('m'), editor)?.id).toBe('view.maskOverlay')
    })

    it('gives every entry back at once', () => {
      rebindShortcut('view.maskOverlay', { key: 'k' })
      unbindShortcut('edit.redo')
      expect(Object.keys(shortcutOverrides())).toHaveLength(2)
      resetAllShortcuts()
      expect(shortcutOverrides()).toEqual({})
      expect(shortcuts()).toBe(SHORTCUTS)
    })

    it('moves the revision on every change, so a sheet knows to redraw', () => {
      const before = shortcutsRevision()
      rebindShortcut('view.maskOverlay', { key: 'k' })
      expect(shortcutsRevision()).toBeGreaterThan(before)
    })
  })

  describe('releasesHoldOriginal', () => {
    it('follows the entry, not the default key', () => {
      expect(releasesHoldOriginal(press('o'))).toBe(true)
      expect(releasesHoldOriginal(press('O'))).toBe(true)
      // Shift is not compared: a release must be honoured whatever the
      // modifier state has become.
      expect(releasesHoldOriginal(press('o', { shiftKey: true }))).toBe(true)
      expect(releasesHoldOriginal(press('o', { metaKey: true }))).toBe(false)
      expect(releasesHoldOriginal(press('k'))).toBe(false)
    })

    it('moves with a rebinding and stops with an unbinding', () => {
      setShortcutOverrides({ 'view.holdOriginal': { key: 'k', shift: false, accel: false } })
      expect(releasesHoldOriginal(press('k'))).toBe(true)
      expect(releasesHoldOriginal(press('o'))).toBe(false)
      setShortcutOverrides({ 'view.holdOriginal': null })
      expect(releasesHoldOriginal(press('o'))).toBe(false)
      expect(releasesHoldOriginal(press('k'))).toBe(false)
    })
  })
})

/* ------------------------------------------------------------------ */
/* The modifier a pointer gesture carries                              */
/* ------------------------------------------------------------------ */

describe('the modifier a pointer gesture carries', () => {
  /** A pointer event with the named modifiers down and no others. */
  function click(...down) {
    return {
      altKey: down.includes('alt'),
      metaKey: down.includes('meta'),
      ctrlKey: down.includes('ctrl'),
      shiftKey: down.includes('shift'),
    }
  }

  it('matches exactly the one asked for, and no other', () => {
    expect(modifierHeld(click('alt'), 'alt')).toBe(true)
    expect(modifierHeld(click('meta'), 'meta')).toBe(true)
    expect(modifierHeld(click('ctrl'), 'control')).toBe(true)
    expect(modifierHeld(click('shift'), 'shift')).toBe(true)

    // The one the old code read is not special: with the setting on ⌘, an
    // Alt-click is an ordinary drag again, which is the whole point of making
    // it a setting.
    expect(modifierHeld(click('alt'), 'meta')).toBe(false)
    expect(modifierHeld(click('meta'), 'alt')).toBe(false)
    expect(modifierHeld(click('shift'), 'control')).toBe(false)
  })

  it('is satisfied by a chord that includes it, because a hand holds two keys', () => {
    // ⌥⇧-click carries Alt, and refusing it would mean a stray Shift silently
    // turned a source pick into a stroke.
    expect(modifierHeld(click('alt', 'shift'), 'alt')).toBe(true)
    expect(modifierHeld(click('alt', 'shift'), 'shift')).toBe(true)
  })

  it('does not offer Control on an Apple keyboard, where it is the secondary click', () => {
    // macOS delivers ⌃-click as `button === 2`, so it never reaches the
    // primary-button handler in `DrawLayer#onpointerdown` at all. A chip for it
    // would do nothing on the one platform whose missing `Alt` key is the
    // reason this setting exists.
    expect(pointerModifiersFor({ apple: true })).toEqual(['alt', 'meta', 'shift'])
    expect(pointerModifiersFor({ apple: false })).toEqual(['alt', 'meta', 'control', 'shift'])
    // No context is the non-Apple answer, which is what every caller that has
    // not asked the platform should get.
    expect(pointerModifiersFor()).toContain('control')
  })

  it('is false for a bare click, and for no event at all', () => {
    expect(modifierHeld(click(), 'alt')).toBe(false)
    expect(modifierHeld(click(), 'meta')).toBe(false)
    expect(modifierHeld(null, 'alt')).toBe(false)
    expect(modifierHeld(undefined, 'alt')).toBe(false)
  })

  it('falls back to the modifier the gesture has always used, whatever it is handed', () => {
    // A hand-edited settings file, a value from a build that offered a fifth
    // modifier, a number: all of them are Alt, which is what the gesture did
    // before it was a setting.
    for (const junk of ['option', '', 'Alt', 0, null, undefined, {}, ['alt']]) {
      expect(normalizePointerModifier(junk)).toBe(DEFAULT_POINTER_MODIFIER)
      expect(modifierHeld(click('alt'), junk)).toBe(true)
      expect(modifierHeld(click('meta'), junk)).toBe(false)
    }
  })

  it('is named by the key the platform prints, not by one name for both', () => {
    // The user's complaint, in one assertion: there is no key called Alt on
    // the machine this is being read on.
    expect(modifierCap('alt', { apple: true })).toBe('⌥')
    expect(modifierCap('alt', { apple: false })).toBe('Alt')
    expect(modifierCap('meta', { apple: true })).toBe('⌘')
    expect(modifierCap('control', { apple: true })).toBe('⌃')
    expect(modifierCap('shift', { apple: true })).toBe('⇧')
    expect(modifierCap('control', { apple: false })).toBe('Ctrl')
  })

  it('has a cap for every modifier it offers, and offers four', () => {
    expect([...POINTER_MODIFIERS]).toEqual(['alt', 'meta', 'control', 'shift'])
    for (const id of POINTER_MODIFIERS) {
      expect(modifierCap(id, { apple: true })).toBeTruthy()
      expect(modifierCap(id, { apple: false })).toBeTruthy()
    }
  })
})
