/**
 * The shortcut table - one table, two consumers.
 *
 * 1. `src/lib/shell/KeyboardLayer.svelte` matches a keydown against it and
 *    runs the entry's `run(commands)`.
 * 2. Task 11's shortcut sheet in Settings renders `labelKey` + `chord`,
 *    grouped by `group`.
 *
 * Nothing else may define a keybinding. Adding a shortcut means adding a row
 * here, and the sheet documents it automatically.
 *
 * This module is pure: no Svelte, no DOM state, no imports from
 * `src/lib/state`. An entry's `run` is handed a `ShortcutCommands` object -
 * the shell implements it - so the table can be tested and rendered without
 * booting the app.
 *
 * Modifier chords are deliberately *not* intercepted: `⌘`/`Ctrl`/`Alt`
 * combinations belong to the platform (⌘Q, ⌘W, ⌘, on macOS), so a keydown
 * carrying one never matches - unless an entry asks for one explicitly with
 * `accel`, which is how a desktop app claims the handful of chords that are
 * genuinely its own (⌘O is the platform's *convention* for Open, not one of
 * the platform's own bindings). `accel` matches ⌘ or Ctrl, so one entry covers
 * both platforms; `Alt` is never part of it.
 *
 * Shift is different - `⇧O` and `⇧U` are bindings of their own. They are
 * matched as a *lower-cased key plus an explicit shift requirement*, not as
 * the uppercase `event.key`: with Caps Lock on, a bare `o` arrives as `'O'`,
 * and matching the letter case alone would silently swap hold-original for
 * pin-original. `shift: undefined` means the entry does not care.
 *
 * ## The user's own bindings
 *
 * `SHORTCUTS` is the *default* table. The user may rebind an entry, and the
 * differences are held here too - `setShortcutOverrides()` installs them,
 * `shortcuts()` is the table actually in force, and `matchShortcut()`,
 * `groupedShortcuts()` and `shortcutConflicts()` all read that one table. An
 * override is keyed by the entry's `id`, so it survives a table edit that
 * changes a default chord, and an id the table no longer has is dropped on
 * load rather than kept as a binding nothing can run.
 *
 * The invariant at the top of this file is unchanged by that: a *binding* is
 * still declared only here. What the override layer supplies is which keys an
 * existing entry answers to - never a new entry, never a new command.
 */

import { pageNavControls } from './model/paging.js'

/**
 * What the shell must provide. Every method is optional to *call* - the layer
 * supplies all of them - but this list is the contract, and each entry below
 * uses exactly one of them.
 *
 * @typedef {Object} ShortcutCommands
 * @property {(slot: number) => void} selectToolSlot - 1..6
 * @property {() => void} cycleMaskSelectShape
 * @property {() => void} toggleMaskSelectMode
 * @property {(held: boolean) => void} holdOriginal
 * @property {() => void} togglePinOriginal
 * @property {() => void} toggleMaskOverlay
 * @property {() => void} toggleReviewFilter
 * @property {() => void} undo
 * @property {() => void} redo
 * @property {() => void} deleteSelectedLayer
 * @property {() => void} zoomFit
 * @property {() => void} zoomIn
 * @property {() => void} zoomOut
 * @property {() => void} zoomActual
 * @property {(side: 'left'|'right') => void} pageByArrow
 * @property {(direction: 'next'|'prev') => void} stepReview
 * @property {(panel: 'pages'|'masks'|'tools') => void} togglePanel
 * @property {(kind: string) => void} openModal
 * @property {() => void} goHome
 * @property {() => void} cancel
 */

/**
 * @typedef {Object} Shortcut
 * @property {string} id - stable, unique
 * @property {string} group - one of `SHORTCUT_GROUPS`
 * @property {string} labelKey - i18n key for the sheet. Never English.
 * @property {(ctx: {readingDirection?: 'rtl'|'ltr'}) => string} [labelKeyFor] - direction-aware label, wins over `labelKey`
 * @property {string[]} keys - `event.key` values that match, letters lower-cased (see `normalizeKey`)
 * @property {boolean} [shift] - require Shift down (true) or up (false); omitted means either
 * @property {boolean} [accel] - require the platform's command modifier (⌘ or Ctrl). Without it, an entry never matches a modifier chord
 * @property {string[]} chord - how the sheet renders it, one `<kbd>` per element
 * @property {(ctx: {apple?: boolean}) => string[]} [chordFor] - platform-aware chord, wins over `chord`
 * @property {'global'|'home'|'editor'} scope
 * @property {boolean} [allowInModal] - fires even with a dialog open
 * @property {boolean} [skipInTextEntry] - never fires while a text field has focus, chord or not
 * @property {boolean} [repeatable] - may fire on auto-repeat while the key is held
 * @property {boolean} [fixed] - the chord is not the user's to change (Escape)
 * @property {boolean} [rebound] - set by `applyShortcutOverrides`: this entry carries an override
 * @property {boolean} [unbound] - set by `applyShortcutOverrides`: the user cleared its chord
 * @property {(commands: ShortcutCommands) => void} run
 */

/**
 * One recorded key combination, and the whole of what an override stores.
 *
 * The same three fields `matchShortcut` compares an event against, in the same
 * vocabulary: a normalized `event.key`, an explicit Shift state, and `accel`
 * for the platform's command modifier (⌘ on Apple, Ctrl elsewhere). Alt is
 * absent by construction - the table never matches an Alt chord, so one can
 * never be recorded. Plain JSON, so it round-trips through the settings store
 * unchanged.
 *
 * @typedef {Object} Chord
 * @property {string} key
 * @property {boolean} shift
 * @property {boolean} accel
 */

/** Sheet sections, in display order. Each has an i18n key of the same name. */
export const SHORTCUT_GROUPS = /** @type {const} */ ([
  'tools',
  'view',
  'edit',
  'zoom',
  'navigation',
  'panels',
  'app',
])

/** Releasing this key (normalized) ends a held original view - the default. */
export const HOLD_ORIGINAL_KEY = 'o'

/** The entry whose key, whatever it is bound to, shows the original while held. */
export const HOLD_ORIGINAL_ID = 'view.holdOriginal'

/** @type {Shortcut[]} */
export const SHORTCUTS = [
  /* ---- tools: 1–6, in tool-rail order, then the selection tool's own -- */
  {
    id: 'tool.autoClean',
    group: 'tools',
    labelKey: 'tools.name.autoClean',
    keys: ['1'],
    chord: ['1'],
    scope: 'editor',
    run: (c) => c.selectToolSlot(1),
  },
  {
    id: 'tool.brush',
    group: 'tools',
    labelKey: 'tools.name.brush',
    keys: ['2'],
    chord: ['2'],
    scope: 'editor',
    run: (c) => c.selectToolSlot(2),
  },
  {
    id: 'tool.shapes',
    group: 'tools',
    labelKey: 'tools.name.shapes',
    keys: ['3'],
    chord: ['3'],
    scope: 'editor',
    run: (c) => c.selectToolSlot(3),
  },
  {
    id: 'tool.aiMaskBrush',
    group: 'tools',
    labelKey: 'tools.name.aiMaskBrush',
    keys: ['4'],
    chord: ['4'],
    scope: 'editor',
    run: (c) => c.selectToolSlot(4),
  },
  {
    id: 'tool.cloneHeal',
    group: 'tools',
    labelKey: 'tools.name.cloneHeal',
    keys: ['5'],
    chord: ['5'],
    scope: 'editor',
    run: (c) => c.selectToolSlot(5),
  },
  {
    id: 'tool.maskSelect',
    group: 'tools',
    labelKey: 'tools.name.maskSelect',
    keys: ['6'],
    chord: ['6'],
    scope: 'editor',
    run: (c) => c.selectToolSlot(6),
  },
  {
    // The selection tool's own two keys. Each arms the tool first; once it is
    // armed, `S` steps its shape (brush, lasso, rectangle) and `X` swaps add
    // for remove (`cycleMaskSelectShape` in the editor state). Not Photoshop's
    // `L` and `M`: those are the Layers panel and the mask overlay here.
    id: 'tool.maskSelect.shape',
    group: 'tools',
    labelKey: 'shortcuts.tool.maskSelectShape',
    keys: ['s'],
    chord: ['S'],
    scope: 'editor',
    run: (c) => c.cycleMaskSelectShape(),
  },
  {
    id: 'tool.maskSelect.mode',
    group: 'tools',
    labelKey: 'shortcuts.tool.maskSelectMode',
    keys: ['x'],
    chord: ['X'],
    scope: 'editor',
    run: (c) => c.toggleMaskSelectMode(),
  },

  /* ---- view ------------------------------------------------------------ */
  {
    // Held: down shows the original, up hides it again (see HOLD_ORIGINAL_KEY).
    id: 'view.holdOriginal',
    group: 'view',
    labelKey: 'shortcuts.view.holdOriginal',
    keys: ['o'],
    shift: false,
    chord: ['O'],
    scope: 'editor',
    run: (c) => c.holdOriginal(true),
  },
  {
    // The sticky-toggle equivalent of the held key.
    id: 'view.pinOriginal',
    group: 'view',
    labelKey: 'shortcuts.view.pinOriginal',
    keys: ['o'],
    shift: true,
    chord: ['Shift', 'O'],
    scope: 'editor',
    run: (c) => c.togglePinOriginal(),
  },
  {
    id: 'view.maskOverlay',
    group: 'view',
    labelKey: 'shortcuts.view.maskOverlay',
    keys: ['m'],
    chord: ['M'],
    scope: 'editor',
    run: (c) => c.toggleMaskOverlay(),
  },
  {
    id: 'view.reviewFilter',
    group: 'view',
    labelKey: 'shortcuts.view.reviewFilter',
    keys: ['r'],
    chord: ['R'],
    scope: 'editor',
    run: (c) => c.toggleReviewFilter(),
  },

  /* ---- edit ------------------------------------------------------------ */
  {
    id: 'edit.undo',
    group: 'edit',
    labelKey: 'shortcuts.edit.undo',
    keys: ['u'],
    shift: false,
    chord: ['U'],
    scope: 'editor',
    run: (c) => c.undo(),
  },
  {
    id: 'edit.redo',
    group: 'edit',
    labelKey: 'shortcuts.edit.redo',
    keys: ['u'],
    shift: true,
    chord: ['Shift', 'U'],
    scope: 'editor',
    run: (c) => c.redo(),
  },
  {
    // The selected layer, from **anywhere** in the editor.
    //
    // The Layers list already answers a bare `Delete` or `Backspace` on a
    // focused row, and keeps doing so; that key belongs to the list and needs
    // the list to have the focus. This is the other half: a region is usually
    // selected on the *page*, and until this there was no way to delete it
    // without tabbing into a panel to find its row.
    //
    // A chord rather than a bare `Backspace`, because a destructive act that a
    // single unmodified key performs from anywhere on the screen is one a
    // fumbled keystroke performs too - and ⌘⌫ is already the platform's
    // spelling of "delete the selected thing". `skipInTextEntry` because a
    // chord otherwise reaches a focused text field (rule 3 in
    // `matchShortcut`), where ⌘⌫ is the platform's *delete to start of line*.
    //
    // It is undoable, like every other region edit.
    id: 'layers.deleteSelected',
    group: 'edit',
    labelKey: 'shortcuts.edit.deleteLayer',
    keys: ['Backspace'],
    accel: true,
    skipInTextEntry: true,
    chord: ['Cmd', 'Backspace'],
    chordFor: ({ apple }) => (apple ? ['⌘', '⌫'] : ['Ctrl', '⌫']),
    scope: 'editor',
    run: (c) => c.deleteSelectedLayer(),
  },

  /* ---- zoom ------------------------------------------------------------ */
  {
    id: 'zoom.fit',
    group: 'zoom',
    labelKey: 'shortcuts.zoom.fit',
    keys: ['0'],
    chord: ['0'],
    scope: 'editor',
    run: (c) => c.zoomFit(),
  },
  {
    id: 'zoom.in',
    group: 'zoom',
    labelKey: 'shortcuts.zoom.in',
    keys: ['+', '='],
    chord: ['+'],
    scope: 'editor',
    repeatable: true,
    run: (c) => c.zoomIn(),
  },
  {
    id: 'zoom.out',
    group: 'zoom',
    labelKey: 'shortcuts.zoom.out',
    keys: ['-'],
    chord: ['-'],
    scope: 'editor',
    repeatable: true,
    run: (c) => c.zoomOut(),
  },
  {
    // The brief lists a "1:1" zoom without naming a key. `Z` is free, is not a
    // platform chord on its own, and survives every keyboard layout - unlike
    // ⇧0, which is `)` on US layouts and something else again elsewhere.
    id: 'zoom.actual',
    group: 'zoom',
    labelKey: 'shortcuts.zoom.actualSize',
    keys: ['z'],
    chord: ['Z'],
    scope: 'editor',
    run: (c) => c.zoomActual(),
  },

  /* ---- navigation ------------------------------------------------------ */
  {
    // Reading-direction aware: in RTL (the default) ← is *next*.
    id: 'page.left',
    group: 'navigation',
    labelKey: 'shortcuts.page.left',
    labelKeyFor: (ctx) => pageNavControls(ctx.readingDirection ?? 'rtl').left.tooltipKey,
    keys: ['ArrowLeft', '['],
    chord: ['←'],
    scope: 'editor',
    repeatable: true,
    run: (c) => c.pageByArrow('left'),
  },
  {
    id: 'page.right',
    group: 'navigation',
    labelKey: 'shortcuts.page.right',
    labelKeyFor: (ctx) => pageNavControls(ctx.readingDirection ?? 'rtl').right.tooltipKey,
    keys: ['ArrowRight', ']'],
    chord: ['→'],
    scope: 'editor',
    repeatable: true,
    run: (c) => c.pageByArrow('right'),
  },
  {
    id: 'review.next',
    group: 'navigation',
    labelKey: 'shortcuts.review.next',
    keys: ['n'],
    chord: ['N'],
    scope: 'editor',
    repeatable: true,
    run: (c) => c.stepReview('next'),
  },
  {
    id: 'review.prev',
    group: 'navigation',
    labelKey: 'shortcuts.review.prev',
    keys: ['p'],
    chord: ['P'],
    scope: 'editor',
    repeatable: true,
    run: (c) => c.stepReview('prev'),
  },

  /* ---- panels ---------------------------------------------------------- */
  {
    id: 'panel.pages',
    group: 'panels',
    labelKey: 'shortcuts.panel.pages',
    keys: ['f'],
    chord: ['F'],
    scope: 'editor',
    run: (c) => c.togglePanel('pages'),
  },
  {
    id: 'panel.masks',
    group: 'panels',
    labelKey: 'shortcuts.panel.masks',
    keys: ['l'],
    chord: ['L'],
    scope: 'editor',
    run: (c) => c.togglePanel('masks'),
  },
  {
    id: 'panel.tools',
    group: 'panels',
    labelKey: 'shortcuts.panel.tools',
    keys: ['t'],
    chord: ['T'],
    scope: 'editor',
    run: (c) => c.togglePanel('tools'),
  },

  /* ---- app ------------------------------------------------------------- */
  {
    id: 'app.export',
    group: 'app',
    labelKey: 'shortcuts.app.export',
    keys: ['e'],
    chord: ['E'],
    scope: 'editor',
    run: (c) => c.openModal('export'),
  },
  {
    // Home only: `N` in the editor is "next region needing review".
    id: 'app.newProject',
    group: 'app',
    labelKey: 'shortcuts.app.newProject',
    keys: ['n'],
    chord: ['N'],
    scope: 'home',
    run: (c) => c.openModal('newProject'),
  },
  {
    // Home only, and the one chord in the table. The Open project dialog has no
    // pointer route: the design file's Home header is a wordmark and a Settings
    // button, so this binding is the way in. `⌘O` rather than a bare letter
    // because that is what a desktop user reaches for, and because it can then
    // be the same key the editor already spends on hold-original.
    id: 'app.openProject',
    group: 'app',
    labelKey: 'shortcuts.app.openProject',
    keys: ['o'],
    accel: true,
    chord: ['Cmd', 'O'],
    chordFor: ({ apple }) => (apple ? ['⌘', 'O'] : ['Ctrl', 'O']),
    scope: 'home',
    run: (c) => c.openModal('openProject'),
  },
  {
    id: 'app.settings',
    group: 'app',
    labelKey: 'shortcuts.app.settings',
    keys: [','],
    chord: [','],
    scope: 'global',
    run: (c) => c.openModal('settings'),
  },
  {
    id: 'app.shortcutSheet',
    group: 'app',
    labelKey: 'shortcuts.app.shortcutSheet',
    keys: ['?'],
    chord: ['?'],
    scope: 'global',
    run: (c) => c.openModal('shortcuts'),
  },
  {
    // The library root. The editor's Back control is the other route out, and
    // it goes one level up, to the project's chapter list.
    id: 'app.home',
    group: 'app',
    labelKey: 'shortcuts.app.home',
    keys: ['h'],
    chord: ['H'],
    scope: 'global',
    run: (c) => c.goHome(),
  },
  {
    // `fixed`, and the only entry that is. Escape is not one binding among
    // thirty: it is written into the matcher's own gates - the one key that
    // survives an open dialog and a focused text field - and it is how the
    // rebinding recorder is cancelled. Rebinding it would leave the gates
    // naming a key nothing runs, and unbinding it would leave a user in a
    // recorder with no way out.
    id: 'app.cancel',
    group: 'app',
    labelKey: 'shortcuts.app.cancel',
    keys: ['Escape'],
    chord: ['Esc'],
    scope: 'global',
    allowInModal: true,
    fixed: true,
    run: (c) => c.cancel(),
  },
]

/** Defaults by id - the lookup every override path needs. */
const DEFAULTS_BY_ID = new Map(SHORTCUTS.map((shortcut) => [shortcut.id, shortcut]))

/**
 * The entry a stable id names, in the *default* table.
 *
 * @param {string} id
 * @returns {Shortcut|null}
 */
export function defaultShortcut(id) {
  return DEFAULTS_BY_ID.get(id) ?? null
}

/* ------------------------------------------------------------------ */
/* Matching                                                            */
/* ------------------------------------------------------------------ */

const TEXT_INPUT_TYPES = new Set([
  'text',
  'search',
  'email',
  'url',
  'tel',
  'password',
  'number',
  'date',
  'time',
  'datetime-local',
  'month',
  'week',
])

/**
 * Keys a range input answers itself. A slider is not text entry - a letter
 * pressed over one is still the letter's shortcut - but its own keys are its
 * own, and `ArrowLeft` on a wipe slider must move the wipe rather than page
 * the chapter.
 */
const RANGE_KEYS = new Set([
  'ArrowLeft',
  'ArrowRight',
  'ArrowUp',
  'ArrowDown',
  'Home',
  'End',
  'PageUp',
  'PageDown',
])

/**
 * Keys a `<select>` answers itself.
 *
 * A picker is **not** text entry, which it used to be counted as: every bare
 * key belonged to it, so `1`–`6` stopped changing tool the moment one had
 * focus. The tool bar draws menus rather than pickers now, but a Layers row,
 * Settings and Export all still draw one. What a native
 * select actually answers is this list, plus a letter, which is its type-ahead
 * and the reason the broad rule was the safe one to start from.
 *
 * **Digits are not on it.** A select's type-ahead does match them, and the
 * cost of leaving them to it is the six tool shortcuts; the cost of claiming
 * them is a user who cannot type `2` to reach an option whose label starts
 * with it. No option in this application does. `Escape` is not on it either,
 * for the same reason a range input's is not: the layer hands `Escape` to
 * `app.cancel` before this function is consulted.
 */
const SELECT_KEYS = new Set([
  'ArrowLeft',
  'ArrowRight',
  'ArrowUp',
  'ArrowDown',
  'Home',
  'End',
  'PageUp',
  'PageDown',
  'Enter',
  ' ',
])

/**
 * Is the event target somewhere the user is typing? A shortcut must never eat
 * a character (`m` in a project name, `,` in a path).
 *
 * A `<select>` is deliberately **not** here - it holds no text, and the keys
 * it does own are `handlesKeyNatively`'s narrower answer.
 *
 * Duck-typed rather than `instanceof`, so it works against a plain object in a
 * test and against a real element in the browser.
 *
 * @param {any} target
 * @returns {boolean}
 */
export function isTextEntry(target) {
  if (!target) return false
  if (target.isContentEditable) return true
  const tag = typeof target.tagName === 'string' ? target.tagName.toUpperCase() : ''
  if (tag === 'TEXTAREA') return true
  if (tag === 'INPUT') {
    const type = typeof target.type === 'string' ? target.type.toLowerCase() : 'text'
    return TEXT_INPUT_TYPES.has(type)
  }
  return false
}

/**
 * Does the focused control answer this key itself? Text entry is the broad
 * case - every bare key belongs to the field - and this is the narrow one: a
 * control that owns a handful of keys and nothing else.
 *
 * A control that runs its own handler and calls `preventDefault` is covered
 * separately, by the layer skipping an already-defaulted event; this covers
 * the native behaviours, which set no such flag.
 *
 * Two controls have an answer here: a range input, whose arrows move the value
 * rather than the page, and a `<select>`, whose list is `SELECT_KEYS` plus a
 * letter of type-ahead.
 *
 * Duck-typed, like `isTextEntry`.
 *
 * @param {any} target
 * @param {string} key - already through `normalizeKey`
 * @returns {boolean}
 */
export function handlesKeyNatively(target, key) {
  if (!target) return false
  if (target.disabled === true) return false
  const tag = typeof target.tagName === 'string' ? target.tagName.toUpperCase() : ''
  if (tag === 'SELECT') return SELECT_KEYS.has(key) || /^[a-z]$/.test(key)
  if (tag !== 'INPUT') return false
  const type = typeof target.type === 'string' ? target.type.toLowerCase() : 'text'
  return type === 'range' && RANGE_KEYS.has(key)
}

/**
 * Fold a raw `event.key` into the form the table stores: single characters
 * lower-cased, named keys (`ArrowLeft`, `Escape`) untouched. Shift is then
 * carried by the event's `shiftKey`, not by the letter's case, so Caps Lock
 * cannot silently swap a binding for its shifted sibling.
 *
 * @param {string} key
 * @returns {string}
 */
export function normalizeKey(key) {
  return typeof key === 'string' && key.length === 1 ? key.toLowerCase() : key
}

/* ------------------------------------------------------------------ */
/* Chords - recording, validating, rendering                           */
/* ------------------------------------------------------------------ */

/**
 * Named keys a chord may use, beside any single character.
 *
 * A short list rather than "anything `event.key` reports", because most of
 * what it reports cannot be a shortcut and letting it through would store a
 * binding the user can never press again. Four are absent on purpose:
 *
 * - `Escape` belongs to `app.cancel` and cancels the recorder;
 * - `Tab` is how a keyboard user leaves the recorder;
 * - `Backspace` and `Delete` are how a binding is *cleared*, so neither can be
 *   recorded - and accepting one from a hand-edited settings file would show a
 *   chord the interface cannot reproduce.
 */
export const NAMED_CHORD_KEYS = new Set([
  'ArrowLeft',
  'ArrowRight',
  'ArrowUp',
  'ArrowDown',
  'Home',
  'End',
  'PageUp',
  'PageDown',
  'Enter',
  'Insert',
  ...Array.from({ length: 12 }, (_, i) => `F${i + 1}`),
])

/** Keys that are a modifier and nothing else: pressing one records nothing. */
const MODIFIER_KEYS = new Set([
  'Shift',
  'Control',
  'Meta',
  'Alt',
  'AltGraph',
  'CapsLock',
  'NumLock',
  'ScrollLock',
  'Fn',
  'FnLock',
  'Hyper',
  'Super',
  'OS',
  'Dead',
  'Unidentified',
])

/**
 * May this key (already through `normalizeKey`) carry a binding?
 *
 * @param {unknown} key
 * @returns {boolean}
 */
export function isChordKey(key) {
  if (typeof key !== 'string' || key === '') return false
  if (key.length === 1) return true
  return NAMED_CHORD_KEYS.has(key)
}

/**
 * Coerce anything at all - a hand-edited settings file, a stale record from an
 * older build - into a `Chord`, or `null`.
 *
 * @param {unknown} raw
 * @returns {Chord|null}
 */
export function parseChord(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null
  const record = /** @type {Record<string, unknown>} */ (raw)
  const key = normalizeKey(/** @type {any} */ (record.key))
  if (!isChordKey(key)) return null
  return { key, shift: record.shift === true, accel: record.accel === true }
}

/**
 * The chord a keydown records, or the reason it cannot be one.
 *
 * Three outcomes, and the recorder needs all three apart:
 *
 * - `{chord}` - a usable combination;
 * - `{chord: null, reasonKey: null}` - a modifier pressed on its own, which is
 *   the first half of every chord: keep listening;
 * - `{chord: null, reasonKey}` - refused, and the recorder says why.
 *
 * @param {{key?: unknown, shiftKey?: boolean, metaKey?: boolean, ctrlKey?: boolean, altKey?: boolean}} event
 * @returns {{chord: Chord|null, reasonKey: string|null}}
 */
export function chordFromEvent(event) {
  const raw = event?.key
  if (typeof raw !== 'string' || raw === '') return { chord: null, reasonKey: null }
  if (MODIFIER_KEYS.has(raw)) return { chord: null, reasonKey: null }
  // Alt chords are never matched by this table (see the module note), so
  // recording one would store a binding that could not fire.
  if (event.altKey === true) return { chord: null, reasonKey: 'shortcuts.rebind.refusedAlt' }
  const key = normalizeKey(raw)
  if (!isChordKey(key)) return { chord: null, reasonKey: 'shortcuts.rebind.refusedKey' }
  return {
    chord: {
      key,
      shift: event.shiftKey === true,
      accel: event.metaKey === true || event.ctrlKey === true,
    },
    reasonKey: null,
  }
}

/** Named keys whose keycap is not their `event.key`. */
const KEY_CAPS = /** @type {Record<string, string>} */ ({
  ArrowLeft: '←',
  ArrowRight: '→',
  ArrowUp: '↑',
  ArrowDown: '↓',
  PageUp: 'PgUp',
  PageDown: 'PgDn',
  ' ': 'Space',
  Escape: 'Esc',
})

/**
 * A chord as keycaps, one `<kbd>` each - the same shape the table's hand-written
 * `chord` arrays use, so `KeyHint` renders a rebound entry and a default one
 * through one path.
 *
 * @param {Chord|null} chord
 * @param {{apple?: boolean}} [context]
 * @returns {string[]}
 */
export function chordCaps(chord, { apple = false } = {}) {
  if (!chord) return []
  const caps = []
  if (chord.accel) caps.push(apple ? '⌘' : 'Ctrl')
  if (chord.shift) caps.push('Shift')
  caps.push(KEY_CAPS[chord.key] ?? (chord.key.length === 1 ? chord.key.toUpperCase() : chord.key))
  return caps
}

/* ------------------------------------------------------------------ */
/* Modifiers a pointer gesture can carry                               */
/* ------------------------------------------------------------------ */

/**
 * Whether this platform names its command modifier `⌘`.
 *
 * `userAgentData` where it exists, `platform` behind it, and neither on a
 * server - in which case `Ctrl` is the safer guess, since it is the name on
 * every platform that is not Apple's. Lives here rather than in the sheet that
 * used to hold it because two callers now ask the same question, and two
 * detectors would eventually answer it differently.
 *
 * @returns {boolean}
 */
export function isApplePlatform() {
  const nav = /** @type {any} */ (globalThis.navigator)
  if (!nav) return false
  const platform = nav.userAgentData?.platform ?? nav.platform ?? ''
  return /mac|iphone|ipad|ipod/i.test(String(platform))
}

/**
 * The modifiers a **pointer** gesture can be qualified by, in the order a
 * picker offers them.
 *
 * These are not chords and cannot be: a chord is a key plus modifiers, and a
 * pointer gesture is a modifier plus a click. `Chord` has no way to say "Alt
 * and nothing else" - `accel` deliberately merges ⌘ and Ctrl, and Alt is
 * absent from it by construction, because the keyboard table never matches an
 * Alt chord. So this is its own small vocabulary beside that one, sharing the
 * keycap idea and nothing else.
 *
 * `meta` is the platform's own command key: ⌘ on Apple, the Windows key
 * elsewhere. It is offered on both because the modifier a pointer carries is
 * the user's choice and refusing one on their platform would be this module
 * guessing at their keyboard.
 */
export const POINTER_MODIFIERS = /** @type {const} */ (['alt', 'meta', 'control', 'shift'])

/** The modifier Clone / heal's source pick has always used, and still starts on. */
export const DEFAULT_POINTER_MODIFIER = 'alt'

/**
 * The keycap for each, per platform.
 *
 * `KEY_CAPS` above cannot carry these: it maps an `event.key` to one cap
 * regardless of platform, and every one of these four is named differently on
 * an Apple keyboard than on any other. The glyphs are the ones printed on the
 * keys themselves, which is what the user is looking for when they read the
 * row.
 */
const MODIFIER_CAPS = /** @type {Record<string, {apple: string, other: string}>} */ ({
  alt: { apple: '⌥', other: 'Alt' },
  meta: { apple: '⌘', other: 'Win' },
  control: { apple: '⌃', other: 'Ctrl' },
  shift: { apple: '⇧', other: 'Shift' },
})

/**
 * Coerce anything at all - a hand-edited settings file, a stale record - into
 * one of `POINTER_MODIFIERS`.
 *
 * @param {unknown} modifier
 * @returns {'alt'|'meta'|'control'|'shift'}
 */
export function normalizePointerModifier(modifier) {
  return /** @type {any} */ (
    typeof modifier === 'string' && /** @type {readonly string[]} */ (POINTER_MODIFIERS).includes(modifier)
      ? modifier
      : DEFAULT_POINTER_MODIFIER
  )
}

/**
 * One pointer modifier as its keycap - `⌥` on an Apple keyboard, `Alt`
 * anywhere else.
 *
 * A keycap is data rather than copy, exactly as the `chord` arrays in the
 * table above are: `⌘` is not translated into anything, and neither is `Ctrl`.
 *
 * @param {unknown} modifier
 * @param {{apple?: boolean}} [context]
 * @returns {string}
 */
export function modifierCap(modifier, { apple = false } = {}) {
  const caps = MODIFIER_CAPS[normalizePointerModifier(modifier)]
  return apple ? caps.apple : caps.other
}

/**
 * Does this pointer event carry the modifier asked for?
 *
 * The whole of the decision, and pure, so that "which modifier a pointer event
 * satisfies" is asserted without a canvas, a page or a draft - everything
 * around it in `DrawLayer` is a hit test and a call.
 *
 * @param {{altKey?: boolean, metaKey?: boolean, ctrlKey?: boolean, shiftKey?: boolean}|null|undefined} event
 * @param {unknown} modifier
 * @returns {boolean}
 */
export function modifierHeld(event, modifier) {
  if (!event) return false
  switch (normalizePointerModifier(modifier)) {
    case 'meta':
      return event.metaKey === true
    case 'control':
      return event.ctrlKey === true
    case 'shift':
      return event.shiftKey === true
    default:
      return event.altKey === true
  }
}

/**
 * The modifiers this platform may actually offer for a pointer gesture.
 *
 * `control` is **not offered on an Apple keyboard**, and that is not a taste
 * decision: macOS delivers `⌃`-click as the secondary click, so the press
 * arrives with `button === 2` and never reaches a primary-button handler at
 * all. Offering it there would be a chip that silently did nothing on the one
 * platform whose missing `Alt` key is the reason this setting exists.
 *
 * @param {{apple?: boolean}} [context]
 * @returns {ReadonlyArray<'alt'|'meta'|'control'|'shift'>}
 */
export function pointerModifiersFor({ apple = false } = {}) {
  return apple ? POINTER_MODIFIERS.filter((id) => id !== 'control') : POINTER_MODIFIERS
}

/**
 * @param {Chord|null} a
 * @param {Chord|null} b
 * @returns {boolean}
 */
export function chordsEqual(a, b) {
  if (a === null || b === null) return a === b
  return a.key === b.key && a.shift === b.shift && a.accel === b.accel
}

/**
 * Is this chord the entry's own default, expressed as a chord?
 *
 * Only entries with a single key can be, and an entry whose `shift` is
 * `undefined` - "either state" - counts a recorded Shift-up press as its
 * default, because that is the press the sheet already draws for it. Storing
 * the override would be storing a difference that is not one.
 *
 * @param {Shortcut} entry
 * @param {Chord} chord
 * @returns {boolean}
 */
export function isDefaultChord(entry, chord) {
  if (entry.keys.length !== 1 || entry.keys[0] !== chord.key) return false
  if ((entry.shift ?? false) !== chord.shift) return false
  return (entry.accel === true) === chord.accel
}

/* ------------------------------------------------------------------ */
/* Overrides, and the table actually in force                          */
/* ------------------------------------------------------------------ */

/**
 * The user's differences from `SHORTCUTS`, by id. A `null` value is a binding
 * the user cleared: the entry keeps its row in the sheet and matches nothing.
 *
 * @type {Record<string, Chord|null>}
 */
let overrides = {}

/** `SHORTCUTS` with `overrides` applied. Rebuilt on every change, never read stale. */
let effective = SHORTCUTS

/** Bumped on every change, so a UI can depend on "the bindings" without diffing them. */
let revision = 0

/**
 * Drop everything that is not a binding this build can honour: an id the table
 * no longer has, a chord that will not parse, an entry whose chord is not the
 * user's to change, and a chord that is only the default written out.
 *
 * @param {unknown} raw
 * @returns {Record<string, Chord|null>}
 */
export function sanitizeShortcutOverrides(raw) {
  /** @type {Record<string, Chord|null>} */
  const kept = {}
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return kept
  for (const [id, value] of Object.entries(/** @type {Record<string, unknown>} */ (raw))) {
    const entry = DEFAULTS_BY_ID.get(id)
    if (!entry || entry.fixed) continue
    if (value === null) {
      kept[id] = null
      continue
    }
    const chord = parseChord(value)
    if (!chord || isDefaultChord(entry, chord)) continue
    kept[id] = chord
  }
  return kept
}

/**
 * A table with the overrides applied. Pure - the live one is `shortcuts()`.
 *
 * A rebound entry answers exactly the chord that was recorded: one key, an
 * explicit Shift state, an explicit `accel`. A default that accepted two keys
 * (`+` and `=`) or either Shift state is *narrowed* by being rebound, which is
 * the honest reading of "the user chose this combination".
 *
 * @param {Record<string, Chord|null>} map
 * @param {Shortcut[]} [table]
 * @returns {Shortcut[]}
 */
export function applyShortcutOverrides(map, table = SHORTCUTS) {
  const byId = new Map(Object.entries(map ?? {}))
  if (byId.size === 0) return table
  return table.map((entry) => {
    if (!byId.has(entry.id)) return entry
    const chord = byId.get(entry.id) ?? null
    if (chord === null) {
      return {
        ...entry,
        keys: [],
        shift: undefined,
        accel: undefined,
        chord: [],
        chordFor: undefined,
        rebound: true,
        unbound: true,
      }
    }
    return {
      ...entry,
      keys: [chord.key],
      shift: chord.shift,
      accel: chord.accel,
      chord: chordCaps(chord),
      chordFor: (context) => chordCaps(chord, context),
      rebound: true,
      unbound: false,
    }
  })
}

/**
 * @param {Record<string, Chord|null>} next
 */
function commit(next) {
  overrides = next
  effective = applyShortcutOverrides(overrides)
  revision += 1
}

/**
 * Install the user's overrides - at boot, and again whenever the settings
 * store hands over a snapshot. Anything unrecognised is dropped.
 *
 * @param {unknown} raw
 * @returns {Record<string, Chord|null>} what was kept
 */
export function setShortcutOverrides(raw) {
  commit(sanitizeShortcutOverrides(raw))
  return shortcutOverrides()
}

/**
 * The differences from the defaults, and only those - what goes to the
 * settings store. A deep copy, so a caller cannot reach back in.
 *
 * @returns {Record<string, Chord|null>}
 */
export function shortcutOverrides() {
  /** @type {Record<string, Chord|null>} */
  const copy = {}
  for (const [id, chord] of Object.entries(overrides)) {
    copy[id] = chord === null ? null : { ...chord }
  }
  return copy
}

/** The table in force: `SHORTCUTS` plus the user's overrides. */
export function shortcuts() {
  return effective
}

/** Increments on every binding change. A UI reads it to know it must redraw. */
export function shortcutsRevision() {
  return revision
}

/**
 * Does this id carry an override? - the per-row `Reset` control's condition.
 *
 * @param {string} id
 * @returns {boolean}
 */
export function isRebound(id) {
  return Object.hasOwn(overrides, id)
}

/**
 * @typedef {{ok: true}|{ok: false, reasonKey: string, conflictId?: string}} RebindResult
 */

/**
 * Bind an entry to a recorded chord.
 *
 * Refused - never swapped - when the chord already belongs to another entry
 * whose scope overlaps this one's. A swap would move a binding the user did
 * not name, in a dialog whose whole job is to say what each key does.
 *
 * @param {string} id
 * @param {unknown} raw - a `Chord`, or anything at all
 * @returns {RebindResult}
 */
export function rebindShortcut(id, raw) {
  const entry = DEFAULTS_BY_ID.get(id)
  if (!entry) return { ok: false, reasonKey: 'shortcuts.rebind.refusedUnknown' }
  if (entry.fixed) return { ok: false, reasonKey: 'shortcuts.rebind.refusedFixed' }
  const chord = parseChord(raw)
  if (!chord) return { ok: false, reasonKey: 'shortcuts.rebind.refusedKey' }
  const taken = chordConflict(id, chord)
  if (taken) {
    return { ok: false, reasonKey: 'shortcuts.rebind.refusedConflict', conflictId: taken.id }
  }
  const next = { ...overrides }
  if (isDefaultChord(entry, chord)) delete next[id]
  else next[id] = chord
  commit(next)
  return { ok: true }
}

/**
 * Clear an entry's chord. The row stays in the sheet, reading `Unbound`, and
 * the command keeps whatever pointer route it has.
 *
 * @param {string} id
 * @returns {RebindResult}
 */
export function unbindShortcut(id) {
  const entry = DEFAULTS_BY_ID.get(id)
  if (!entry) return { ok: false, reasonKey: 'shortcuts.rebind.refusedUnknown' }
  if (entry.fixed) return { ok: false, reasonKey: 'shortcuts.rebind.refusedFixed' }
  commit({ ...overrides, [id]: null })
  return { ok: true }
}

/**
 * Give one entry its default back.
 *
 * @param {string} id
 * @returns {RebindResult}
 */
export function resetShortcut(id) {
  if (!DEFAULTS_BY_ID.has(id)) return { ok: false, reasonKey: 'shortcuts.rebind.refusedUnknown' }
  const next = { ...overrides }
  delete next[id]
  commit(next)
  return { ok: true }
}

/** Give every entry its default back. */
export function resetAllShortcuts() {
  commit({})
}

/**
 * Whether a dialog that is **not on the modal stack** is on screen.
 *
 * There is exactly one - the first-launch download offer, which belongs to the
 * launch rather than to a screen and is therefore mounted beside the stack
 * rather than pushed onto it. `isModalOpen()` counts `app.modals` and cannot
 * see it, so without this the keyboard layer
 * would answer `,` with a Settings dialog raised *underneath* a modal
 * backdrop, and `N`, `O` and `H` would navigate the screen behind it.
 *
 * A module-level boolean rather than a store import: this file is the shortcut
 * table and is read by node tests that mount nothing, and one flag written by
 * one dialog is not worth reaching into application state for.
 */
let dialogOutsideStack = false

/**
 * Say whether such a dialog is up. Called by the dialog's own store as it
 * opens and closes.
 *
 * @param {boolean} open
 */
export function setDialogOutsideStack(open) {
  dialogOutsideStack = open === true
}

/** @returns {boolean} */
export function isDialogOutsideStackOpen() {
  return dialogOutsideStack
}

/**
 * Find the shortcut a keydown should run, or `null`. Read against the table in
 * force - `shortcuts()`, defaults plus the user's overrides - never against
 * `SHORTCUTS` directly.
 *
 * The rules, in order:
 *
 * 1. `Alt` chords are the platform's and are never matched. `⌘`/`Ctrl` chords
 *    match only an entry that asks for one with `accel`, and an `accel` entry
 *    matches nothing else.
 * 2. While a dialog is open, only `allowInModal` entries match - Escape. A
 *    dialog beside the stack (`setDialogOutsideStack`) takes Escape too,
 *    because it answers that key itself and the layer cannot tell it is
 *    there.
 * 3. While a text field has focus, only Escape and `accel` entries match: a
 *    chord cannot be eaten by a text field the way a bare letter can - unless
 *    the entry says `skipInTextEntry`, which is for the handful of chords the
 *    *platform* spends inside a field (⌘⌫ deletes to the start of the line).
 * 4. A key the focused control answers itself - an arrow over a range input -
 *    belongs to that control. The rest of the table still matches, so `M` over
 *    a slider is still the mask overlay.
 * 5. An entry with a `shift` requirement only matches when `shiftKey` agrees.
 *
 * @param {{key: string, shiftKey?: boolean, metaKey?: boolean, ctrlKey?: boolean, altKey?: boolean, target?: any}} event
 * @param {{scope: 'home'|'editor', modalOpen?: boolean, textEntry?: boolean}} context
 * @returns {Shortcut|null}
 */
export function matchShortcut(event, context) {
  if (!event || typeof event.key !== 'string') return null
  if (event.altKey) return null

  const accel = event.metaKey === true || event.ctrlKey === true
  const escape = event.key === 'Escape'
  // A dialog beside the stack takes the whole table, **Escape included**. The
  // stack's own dialogs let Escape through because the layer defers to the
  // mounted `Modal` by asking `isModalOpen()` before it acts; a dialog the
  // stack does not know about would fail that test, so the layer would cancel
  // an editor interaction behind a dialog that is closing itself on the same
  // press. `Modal` still answers Escape on `window` - the offer closes, and it
  // is the only thing that happens.
  if (isDialogOutsideStackOpen()) return null
  if (context.modalOpen && !escape) return null
  const textEntry = context.textEntry ?? isTextEntry(event.target)
  if (textEntry && !escape && !accel) return null
  if (!escape && !accel && handlesKeyNatively(event.target, normalizeKey(event.key))) return null

  const key = normalizeKey(event.key)
  const shift = event.shiftKey === true
  for (const shortcut of shortcuts()) {
    if (!shortcut.keys.includes(key)) continue
    if ((shortcut.accel === true) !== accel) continue
    if (textEntry && shortcut.skipInTextEntry === true) continue
    if (shortcut.shift !== undefined && shortcut.shift !== shift) continue
    if (shortcut.scope !== 'global' && shortcut.scope !== context.scope) continue
    if (context.modalOpen && !shortcut.allowInModal) continue
    return shortcut
  }
  return null
}

/**
 * The i18n key the shortcut sheet should render for an entry. Direction-aware
 * entries (the arrow keys) resolve against the open project's reading
 * direction, so the sheet says what the key actually does.
 *
 * @param {Shortcut} shortcut
 * @param {{readingDirection?: 'rtl'|'ltr'}} [context]
 * @returns {string}
 */
export function shortcutLabelKey(shortcut, context = {}) {
  return shortcut.labelKeyFor ? shortcut.labelKeyFor(context) : shortcut.labelKey
}

/**
 * The keys the shortcut sheet should render for an entry, one `<kbd>` each.
 * Chord entries name the platform's command modifier, which is the one key
 * whose *name* differs between platforms rather than its meaning.
 *
 * @param {Shortcut} shortcut
 * @param {{apple?: boolean}} [context]
 * @returns {string[]}
 */
export function shortcutChord(shortcut, context = {}) {
  return shortcut.chordFor ? shortcut.chordFor(context) : shortcut.chord
}

/**
 * Do two entries claim a key as the same press?
 *
 * Two entries overlap when their scopes are equal or either is `global`.
 * Entries that require opposite Shift states can share a key, and so can a
 * chord and a bare key: `⌘O` and `O` are two different presses. An unbound
 * entry has no keys and therefore clashes with nothing.
 *
 * @param {Shortcut} a
 * @param {Shortcut} b
 * @returns {boolean}
 */
function clash(a, b) {
  const overlap = a.scope === b.scope || a.scope === 'global' || b.scope === 'global'
  if (!overlap) return false
  if (a.shift !== undefined && b.shift !== undefined && a.shift !== b.shift) return false
  if ((a.accel === true) !== (b.accel === true)) return false
  return a.keys.some((key) => b.keys.includes(key))
}

/**
 * Every pair of entries that claim the same key in overlapping scopes - the
 * table's one invariant, checked rather than assumed. Defaults to the table in
 * force, so a user's overrides are held to it too.
 *
 * @param {Shortcut[]} [table]
 * @returns {Array<{key: string, a: string, b: string}>} empty when the table is sound
 */
export function shortcutConflicts(table = shortcuts()) {
  const clashes = []
  for (let i = 0; i < table.length; i += 1) {
    for (let j = i + 1; j < table.length; j += 1) {
      const a = table[i]
      const b = table[j]
      if (!clash(a, b)) continue
      for (const key of a.keys) {
        if (b.keys.includes(key)) clashes.push({ key, a: a.id, b: b.id })
      }
    }
  }
  return clashes
}

/**
 * Which entry, if any, already answers this chord in a scope that overlaps
 * `id`'s. The question the rebinding recorder asks before it commits.
 *
 * @param {string} id - the entry being rebound
 * @param {Chord} chord
 * @param {Shortcut[]} [table]
 * @returns {Shortcut|null}
 */
export function chordConflict(id, chord, table = shortcuts()) {
  const self = table.find((shortcut) => shortcut.id === id)
  if (!self) return null
  const candidate = { ...self, keys: [chord.key], shift: chord.shift, accel: chord.accel }
  for (const other of table) {
    if (other.id === id) continue
    if (clash(candidate, other)) return other
  }
  return null
}

/**
 * Does this keyup release the key that shows the original while held?
 *
 * Asked of the table rather than of a constant, because the entry is
 * rebindable: a hold that started on the user's own key must end on it, and an
 * *unbound* hold must never end on the default one.
 *
 * Shift is deliberately not compared - a release must be honoured whatever the
 * modifier state has become since the press, or the original stays stuck on.
 *
 * @param {{key: string, metaKey?: boolean, ctrlKey?: boolean, altKey?: boolean}} event
 * @param {Shortcut[]} [table]
 * @returns {boolean}
 */
export function releasesHoldOriginal(event, table = shortcuts()) {
  if (!event || typeof event.key !== 'string') return false
  const entry = table.find((shortcut) => shortcut.id === HOLD_ORIGINAL_ID)
  if (!entry || entry.keys.length === 0) return false
  if (event.altKey === true) return false
  // A chord that happens to end in the same letter (`⌘O`) is a different press
  // and never started a hold - unless the hold is itself bound to one.
  const accel = event.metaKey === true || event.ctrlKey === true
  if (accel !== (entry.accel === true)) return false
  return entry.keys.includes(normalizeKey(event.key))
}

/**
 * The table in force, grouped for the sheet. Groups keep `SHORTCUT_GROUPS`
 * order and entries keep table order.
 *
 * @param {{scope?: 'home'|'editor'|'all'}} [options]
 * @returns {Array<{group: string, shortcuts: Shortcut[]}>}
 */
export function groupedShortcuts({ scope = 'all' } = {}) {
  const table = shortcuts()
  return SHORTCUT_GROUPS.map((group) => ({
    group,
    shortcuts: table.filter(
      (shortcut) =>
        shortcut.group === group &&
        (scope === 'all' || shortcut.scope === 'global' || shortcut.scope === scope)
    ),
  })).filter((section) => section.shortcuts.length > 0)
}
