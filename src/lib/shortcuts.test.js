import { describe, it, expect, afterEach } from 'vitest'
import {
  SHORTCUTS,
  matchShortcut,
  normalizeKey,
  isTextEntry,
  handlesKeyNatively,
  HOLD_ORIGINAL_ID,
  HOLD_PAN_ID,
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
  extraButtonPress,
  isRebound,
  parseChord,
  rebindShortcut,
  releasesHold,
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
  it('duplicates with Cmd/Ctrl+D only in the editor, outside text fields and dialogs', () => {
    for (const modifier of ['metaKey', 'ctrlKey']) {
      const shortcut = matchShortcut(press('d', { [modifier]: true }), editor)
      expect(shortcut?.id).toBe('layers.duplicateSelected')
      let calls = 0
      shortcut.run({ duplicateSelectedRegion: () => calls++ })
      expect(calls).toBe(1)
      expect(matchShortcut(press('d', { [modifier]: true }), home)).toBeNull()
      expect(matchShortcut(press('d', { [modifier]: true, target: { tagName: 'INPUT', type: 'text' } }), editor)).toBeNull()
      expect(matchShortcut(press('d', { [modifier]: true }), { ...editor, modalOpen: true })).toBeNull()
      expect(matchShortcut(press('d', { [modifier]: true, shiftKey: true }), editor)).toBeNull()
    }
  })
  it('matches editor keys only in the editor', () => {
    expect(matchShortcut(press('1'), editor)?.id).toBe('tool.autoClean')
    expect(matchShortcut(press('1'), home)).toBe(null)
  })

  it('selects the six tools with 1 to 6, in rail order', () => {
    const ids = ['autoClean', 'brush', 'shapes', 'aiMaskBrush', 'cloneStamp', 'maskSelect']
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

  it('steps the shape on S and the mode on Q, back with Shift', () => {
    const calls = []
    const commands = /** @type {any} */ ({
      cycleShape: (direction) => calls.push(['shape', direction]),
      cycleMode: (direction) => calls.push(['mode', direction]),
    })
    matchShortcut(press('s'), editor)?.run(commands)
    matchShortcut(press('S', { shiftKey: true }), editor)?.run(commands)
    matchShortcut(press('q'), editor)?.run(commands)
    matchShortcut(press('Q', { shiftKey: true }), editor)?.run(commands)
    expect(calls).toEqual([['shape', 1], ['shape', -1], ['mode', 1], ['mode', -1]])
    expect(matchShortcut(press('s'), home)).toBe(null)
  })

  it('arms each selection mode on its own key, and the clone stamp on C', () => {
    const calls = []
    const commands = /** @type {any} */ ({
      setMaskSelectMode: (mode) => calls.push(mode),
      cycleCloneStamp: (key) => calls.push(`clone:${key}`),
    })
    for (const key of ['a', 'x', 'd']) matchShortcut(press(key), editor)?.run(commands)
    matchShortcut(press('c'), editor)?.run(commands)
    matchShortcut(press('C', { shiftKey: true }), editor)?.run(commands)
    expect(calls).toEqual(['add', 'remove', 'detect', 'clone:mode', 'clone:alignment'])
    // ⌘D is still Duplicate.
    expect(matchShortcut(press('d', { metaKey: true }), editor)?.id).toBe('layers.duplicateSelected')
  })

  it('steps the brush on the brackets, its hardness with Shift, its opacity on I, and the padding on ; and \'', () => {
    const calls = []
    const commands = /** @type {any} */ ({
      stepBrush: (key, direction) => calls.push([key, direction]),
      stepMaskPadding: (direction) => calls.push(['padding', direction]),
    })
    for (const event of [
      press('['), press(']'), press('{', { shiftKey: true }), press('}', { shiftKey: true }),
      press('i'), press('I', { shiftKey: true }), press(';'), press("'"),
    ]) matchShortcut(event, editor)?.run(commands)
    expect(calls).toEqual([
      ['size', -1], ['size', 1], ['hardness', -1], ['hardness', 1],
      ['opacity', -1], ['opacity', 1], ['padding', -1], ['padding', 1],
    ])
    for (const key of ['[', ']', ';', "'"]) expect(matchShortcut(press(key), editor)?.repeatable, key).toBe(true)
  })

  it('runs Text cleanup on ⌘↵ and never on a bare Enter', () => {
    expect(matchShortcut(press('Enter', { metaKey: true }), editor)?.id).toBe('tool.runCleanup')
    expect(matchShortcut(press('Enter', { ctrlKey: true }), editor)?.id).toBe('tool.runCleanup')
    expect(matchShortcut(press('Enter'), editor)).toBe(null)
    expect(matchShortcut({ ...press('Enter', { metaKey: true }), target: { tagName: 'INPUT', type: 'text' } }, editor)).toBe(null)
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

  // German and many other layouts type the brackets with Option (Mac) or
  // AltGr (Windows, reported as Ctrl and Alt).
  it('takes a bracket typed with Option or AltGr, and nothing else with Alt', () => {
    expect(matchShortcut(press('[', { altKey: true }), editor)?.id).toBe('brush.sizeDown')
    expect(matchShortcut(press(']', { altKey: true }), editor)?.id).toBe('brush.sizeUp')
    expect(matchShortcut(press('{', { altKey: true, ctrlKey: true }), editor)?.id).toBe('brush.hardnessDown')
    expect(matchShortcut(press('}', { altKey: true, ctrlKey: true }), editor)?.id).toBe('brush.hardnessUp')
    expect(matchShortcut(press('[', { altKey: true, metaKey: true }), editor)).toBe(null)
    expect(matchShortcut(press('i', { altKey: true }), editor)).toBe(null)
    expect(matchShortcut(press('u', { altKey: true, ctrlKey: true }), editor)).toBe(null)
  })

  it('binds the scope, the run\'s cancel and the selected layer\'s clean', () => {
    expect(matchShortcut(press('v'), editor)?.id).toBe('tool.cleanScope.next')
    expect(matchShortcut(press('V', { shiftKey: true }), editor)?.id).toBe('tool.cleanScope.prev')
    expect(matchShortcut(press('.', { metaKey: true }), editor)?.id).toBe('tool.cancelRun')
    expect(matchShortcut(press('.', { ctrlKey: true }), editor)?.id).toBe('tool.cancelRun')
    expect(matchShortcut(press('.'), editor)).toBe(null)
    expect(matchShortcut(press('b'), editor)?.id).toBe('layers.cleanSelected')
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
  })

  it('pages to the first and last page on Home and End', () => {
    expect(matchShortcut(press('Home'), editor)?.id).toBe('page.first')
    expect(matchShortcut(press('End'), editor)?.id).toBe('page.last')
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
    expect(matchShortcut(press('9'), editor)).toBe(null)
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

  describe('Space', () => {
    /** @param {Object} fields @param {{visible?: boolean, canvas?: boolean}} [state] */
    const control = (fields, { visible = true, canvas = false } = {}) => ({
      getAttribute: () => null,
      ...fields,
      matches: (selector) => selector === ':focus-visible' && visible,
      closest: (selector) => (selector === '[data-canvas-viewport]' && canvas ? {} : null),
    })

    it('is a keyboard-focused control\'s own press', () => {
      expect(handlesKeyNatively(control({ tagName: 'BUTTON' }), ' ')).toBe(true)
      expect(handlesKeyNatively(control({ tagName: 'INPUT', type: 'checkbox' }), ' ')).toBe(true)
      expect(handlesKeyNatively(control({ tagName: 'DIV', getAttribute: () => 'radio' }), ' ')).toBe(true)
    })

    it('pans over a control that was only clicked, and on the canvas', () => {
      expect(handlesKeyNatively(control({ tagName: 'BUTTON' }, { visible: false }), ' ')).toBe(false)
      expect(handlesKeyNatively(control({ tagName: 'BUTTON' }, { canvas: true }), ' ')).toBe(false)
      expect(handlesKeyNatively(control({ tagName: 'DIV' }), ' ')).toBe(false)
    })
  })

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
    expect(matchShortcut(press('5', { target: select }), editor)?.id).toBe('tool.cloneStamp')
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

    // The stamp was `tool.cloneHeal` when it could heal. A key chosen for it
    // then is still the key for it now.
    it('carries a rebinding across the clone stamp’s rename', () => {
      expect(sanitizeShortcutOverrides({ 'tool.cloneHeal': { key: 'k' } })).toEqual({
        'tool.cloneStamp': { key: 'k', shift: false, accel: false },
      })
      expect(sanitizeShortcutOverrides({ 'tool.cloneHeal': null })).toEqual({ 'tool.cloneStamp': null })
      expect(
        sanitizeShortcutOverrides({ 'tool.cloneHeal': { key: 'k' }, 'tool.cloneStamp': { key: 'j' } }),
      ).toEqual({ 'tool.cloneStamp': { key: 'j', shift: false, accel: false } })
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
      setShortcutOverrides({ 'view.maskOverlay': { key: '9', shift: false, accel: false } })
      expect(matchShortcut(press('9'), editor)?.id).toBe('view.maskOverlay')
      expect(matchShortcut(press('m'), editor)).toBe(null)
    })

    it('narrows a rebound entry to exactly the combination recorded', () => {
      setShortcutOverrides({ 'view.maskOverlay': { key: '9', shift: false, accel: false } })
      // The default `m` matched either Shift state; `k` was recorded Shift-up.
      expect(matchShortcut(press('9', { shiftKey: true }), editor)).toBe(null)
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
      setShortcutOverrides({ 'view.maskOverlay': { key: '9', shift: false, accel: false } })
      expect(shortcutConflicts()).toEqual([])
    })

    it('does not mutate the defaults', () => {
      setShortcutOverrides({ 'view.maskOverlay': { key: '9', shift: false, accel: false } })
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
      expect(rebindShortcut('view.maskOverlay', { key: '9' })).toEqual({ ok: true })
      expect(shortcutOverrides()).toEqual({
        'view.maskOverlay': { key: '9', shift: false, accel: false },
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
      expect(rebindShortcut('tool.retouch', { key: '9' }).reasonKey).toBe(
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
      expect(rebindShortcut('view.maskOverlay', { key: '9' }).ok).toBe(true)
      expect(rebindShortcut('view.reviewFilter', { key: 'm' }).ok).toBe(true)
      expect(matchShortcut(press('m'), editor)?.id).toBe('view.reviewFilter')
      expect(matchShortcut(press('9'), editor)?.id).toBe('view.maskOverlay')
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
      rebindShortcut('view.maskOverlay', { key: '9' })
      unbindShortcut('edit.redo')
      expect(Object.keys(shortcutOverrides())).toHaveLength(2)
      resetAllShortcuts()
      expect(shortcutOverrides()).toEqual({})
      expect(shortcuts()).toBe(SHORTCUTS)
    })

    it('moves the revision on every change, so a sheet knows to redraw', () => {
      const before = shortcutsRevision()
      rebindShortcut('view.maskOverlay', { key: '9' })
      expect(shortcutsRevision()).toBeGreaterThan(before)
    })
  })

  describe('releasesHold', () => {
    it('follows the entry, not the default key', () => {
      expect(releasesHold(press('o'), HOLD_ORIGINAL_ID)).toBe(true)
      expect(releasesHold(press('O'), HOLD_ORIGINAL_ID)).toBe(true)
      // Shift is not compared: a release must be honoured whatever the
      // modifier state has become.
      expect(releasesHold(press('o', { shiftKey: true }), HOLD_ORIGINAL_ID)).toBe(true)
      expect(releasesHold(press('o', { metaKey: true }), HOLD_ORIGINAL_ID)).toBe(false)
      expect(releasesHold(press('9'), HOLD_ORIGINAL_ID)).toBe(false)
    })

    it('moves with a rebinding and stops with an unbinding', () => {
      setShortcutOverrides({ 'view.holdOriginal': { key: '9', shift: false, accel: false } })
      expect(releasesHold(press('9'), HOLD_ORIGINAL_ID)).toBe(true)
      expect(releasesHold(press('o'), HOLD_ORIGINAL_ID)).toBe(false)
      setShortcutOverrides({ 'view.holdOriginal': null })
      expect(releasesHold(press('o'), HOLD_ORIGINAL_ID)).toBe(false)
      expect(releasesHold(press('9'), HOLD_ORIGINAL_ID)).toBe(false)
    })

    it('ends the pan on Space, or on the key it was moved to', () => {
      expect(releasesHold(press(' '), HOLD_PAN_ID)).toBe(true)
      expect(releasesHold(press('o'), HOLD_PAN_ID)).toBe(false)
      setShortcutOverrides({ [HOLD_PAN_ID]: { key: 'h', shift: true, accel: false } })
      expect(releasesHold(press('h'), HOLD_PAN_ID)).toBe(true)
      expect(releasesHold(press(' '), HOLD_PAN_ID)).toBe(false)
    })
  })

  describe('extra mouse buttons', () => {
    /** @param {string} type @param {number} button @param {Object} [extra] */
    function pointer(type, button, extra = {}) {
      return { type, button, buttons: 0, target: null, ...extra }
    }

    it('reads back and forward as keys, and nothing else', () => {
      expect(extraButtonPress(pointer('pointerdown', 3))).toMatchObject({ key: 'Mouse4', down: true })
      expect(extraButtonPress(pointer('pointerup', 4))).toMatchObject({ key: 'Mouse5', down: false })
      for (const button of [-1, 0, 1, 2]) expect(extraButtonPress(pointer('pointerdown', button))).toBe(null)
      expect(extraButtonPress(pointer('click', 3))).toBe(null)
    })

    it('reads a press made while another button is down', () => {
      expect(extraButtonPress(pointer('pointermove', 3, { buttons: 1 | 8 }))?.down).toBe(true)
      expect(extraButtonPress(pointer('pointermove', 3, { buttons: 1 }))?.down).toBe(false)
      expect(extraButtonPress(pointer('pointermove', -1, { buttons: 8 }))).toBe(null)
    })

    it('records, draws and matches like any key', () => {
      const press = /** @type {any} */ (extraButtonPress(pointer('pointerdown', 4, { shiftKey: true })))
      const { chord } = chordFromEvent(press)
      expect(chord).toEqual({ key: 'Mouse5', shift: true, accel: false })
      expect(parseChord(chord)).toEqual(chord)
      expect(chordCaps(chord)).toEqual(['Shift', 'Mouse 5'])
      expect(rebindShortcut('view.maskOverlay', chord).ok).toBe(true)
      expect(matchShortcut(press, { ...editor, textEntry: false })?.id).toBe('view.maskOverlay')
    })

    it('holds and lets go on the button', () => {
      setShortcutOverrides({ [HOLD_ORIGINAL_ID]: { key: 'Mouse4', shift: false, accel: false } })
      const down = /** @type {any} */ (extraButtonPress(pointer('pointerdown', 3)))
      const up = /** @type {any} */ (extraButtonPress(pointer('pointerup', 3)))
      expect(matchShortcut(down, editor)?.id).toBe(HOLD_ORIGINAL_ID)
      expect(releasesHold(up, HOLD_ORIGINAL_ID)).toBe(true)
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

// The two tools past the digits take letters, Photoshop's: J for the Patch
// tool (the healing group's key) and G for the Gradient.
describe('the Patch and Gradient keys', () => {
  it('arm slots 8 and 9 from J and G in the editor', () => {
    const slots = []
    const context = { selectToolSlot: (/** @type {number} */ slot) => slots.push(slot) }
    const patch = matchShortcut(press('j'), editor)
    const gradient = matchShortcut(press('g'), editor)
    expect(patch?.id).toBe('tool.patch')
    expect(gradient?.id).toBe('tool.gradient')
    patch?.run(/** @type {any} */ (context))
    gradient?.run(/** @type {any} */ (context))
    expect(slots).toEqual([8, 9])
    expect(shortcutChord(/** @type {any} */ (patch))).toEqual(['J'])
    expect(shortcutChord(/** @type {any} */ (gradient))).toEqual(['G'])
  })

  it('arm the Gradient with its Auto type from Shift+G, and G alone stays the tool', () => {
    const calls = []
    const context = { armAutoGradient: () => calls.push('auto'), selectToolSlot: (/** @type {number} */ slot) => calls.push(slot) }
    const auto = matchShortcut(press('G', { shiftKey: true }), editor)
    expect(auto?.id).toBe('tool.gradientAuto')
    auto?.run(/** @type {any} */ (context))
    matchShortcut(press('g'), editor)?.run(/** @type {any} */ (context))
    expect(calls).toEqual(['auto', 9])
    expect(shortcutChord(/** @type {any} */ (auto))).toEqual(['Shift', 'G'])
  })

  it('do nothing on the home screen or while typing', () => {
    expect(matchShortcut(press('j'), home)).toBeNull()
    expect(matchShortcut(press('g', { target: { tagName: 'INPUT', type: 'text' } }), editor)).toBeNull()
  })
})
