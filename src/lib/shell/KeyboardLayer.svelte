<script>
  import { tick } from 'svelte'
  /**
   * The keyboard layer: one `keydown` listener, one `keyup` listener, and the
   * `ShortcutCommands` implementation `src/lib/shortcuts.js` calls into.
   *
   * It renders nothing. Every binding lives in the table, not here.
   */
  import {
    matchShortcut,
    releasesHoldOriginal,
    isTextEntry,
    shortcutConflicts,
  } from '../shortcuts.js'
  import { app, goLibrary, pushModal, isModalOpen } from '../state/app.svelte.js'
  import { openWindow, togglePanel } from '../state/session.svelte.js'
  import * as editorState from '../state/editor.svelte.js'
  import {
    deleteRow,
    duplicateRegion,
    runMaskAction,
    runRegionMenuItem,
    stepLayerOpacity,
    stepMaskPadding,
  } from '../editor/maskactions.svelte.js'
  import { draft } from '../editor/draft.svelte.js'
  import { applySelection } from '../editor/retouch.svelte.js'
  import { isDetected } from '../model/masks.js'
  import { isRegionCopy } from '../model/layers.js'
  import { cloudParts, runStep } from '../editor/cloudrun.js'

  /** @type {import('../shortcuts.js').ShortcutCommands} */
  const commands = {
    selectToolSlot: (slot) => editorState.setToolBySlot(slot),
    armAutoGradient: () => editorState.armAutoGradient(),
    cycleShape: (direction) => editorState.cycleToolShape(direction),
    cycleMode: (direction) => editorState.cycleToolMode(direction),
    setMaskSelectMode: (mode) => editorState.setMaskSelectMode(mode),
    cycleCloneStamp: (key) => editorState.cycleCloneStamp(key),
    stepBrush,
    stepMaskPadding,
    runTextCleanup,
    cycleCleanScope,
    cancelRun: () => void editorState.cancelRun(),
    cleanSelectedRegion,
    retrySelectedWider: () => {
      const region = editorState.selectedRegion()
      if (region && !isDetected(region)) void runRegionMenuItem('retryWider', region)
    },
    stepWipe: (direction) => editorState.stepWipe(direction),
    focusNextOption: () => void focusNextOption(),
    holdOriginal: (held) => editorState.holdOriginal(held),
    togglePinOriginal: () => editorState.togglePinOriginal(),
    toggleMaskOverlay: () => editorState.toggleMaskOverlay(),
    toggleReviewFilter: () => editorState.toggleReviewFilter(),
    undo: () => editorState.undo(),
    redo: () => editorState.redo(),
    deleteSelectedLayer,
    duplicateSelectedRegion: () => {
      const region = editorState.selectedRegion()
      if (region) void duplicateRegion(region)
    },
    zoomFit: () => editorState.zoomFit(),
    zoomIn: () => editorState.zoomIn(),
    zoomOut: () => editorState.zoomOut(),
    zoomActual: () => editorState.zoomActual(),
    pageByArrow: (side) => editorState.pageByArrow(side),
    pageToEnd: (end) => editorState.goToPage(end === 'first' ? 0 : editorState.pageCount() - 1),
    stepReview: (direction) => editorState.stepReview(direction),
    togglePanel: (panel) => togglePanel(panel),
    openModal: (kind) => pushModal({ kind }),
    goHome: () => goLibrary(),
    cancel,
  }

  /**
   * `Esc`. While a dialog is mounted the dialog owns Escape - Task 4's `Modal`
   * listens on `window` and closes itself through the `onclose` the host wires
   * to `closeModal()`. Acting here as well would pop two dialogs on one press,
   * so this defers. (A dialog with no `onclose` - the overwrite refusal - is
   * deliberately un-escapable, and deferring preserves that too.)
   */
  function cancel() {
    if (isModalOpen()) return
    const active = globalThis.document?.activeElement
    // A text field and a picker both own Escape while focused: it closes or
    // blurs them and must not reach the editor, where it would drop a draft
    // or a selection. `isTextEntry` no longer counts a `<select>` (so digit
    // shortcuts reach the rail past a focused picker), so the picker is
    // named here on its own. So is every tool bar control `\` reaches: a
    // slider left focused keeps stepping on the arrows, and the held Patch or
    // Gradient selection it was tuning must not go with it.
    if (isTextEntry(active) || active?.tagName === 'SELECT' || active?.matches?.(BAR_CONTROLS)) {
      /** @type {HTMLElement} */ (active).blur()
      return
    }
    if (app.route.name === 'editor') editorState.cancelInteraction()
  }

  /**
   * `⌘⌫`. Whatever the Layers row's trash icon would do to the selected
   * region, and not a second meaning of it: `deleteRow` deletes the mask if
   * there is one and the region itself if there is not, and either way it is
   * undoable.
   *
   * Nothing selected, nothing to delete. The table cannot ask that question -
   * it is pure, and the selection is application state - so the guard is here,
   * which is also where the entry's other guard would be if `skipInTextEntry`
   * had not made the matcher able to state it.
   */
  function deleteSelectedLayer() {
    const region = editorState.selectedRegion()
    if (region) deleteRow(region)
  }

  /**
   * `⌘↵`. What the Text cleanup panel's run button starts, as the panel has it
   * set - its step and its scope - and nothing while a run is going: the same
   * keys stopping a run would make a second press of a fumbled chord cancel
   * work. A stored Project reads as Chapter while a half goes to the cloud,
   * as it does in the panel (`ToolBar.svelte#scope`).
   */
  /**
   * `[` `]`, `{` `}`, `I`, `K`, `Y`: the active tool's size, hardness, opacity,
   * flow or spacing. While a
   * region copy is being placed the bar's Opacity row is the copy's own, so
   * `I` steps that instead of the tool's hidden default.
   *
   * @param {'size'|'hardness'|'opacity'|'flow'|'spacing'} key
   * @param {number} direction
   */
  function stepBrush(key, direction) {
    const { editor } = editorState
    const region = editorState.selectedRegion()
    if (key === 'opacity' && editor.tool === 'cloneStamp' && editor.toolParams.cloneStamp?.mode === 'region'
      && region && isRegionCopy(region)) {
      stepLayerOpacity(region, direction)
      return
    }
    editorState.stepToolParam(key, direction)
  }

  function runTextCleanup() {
    // A Patch or Gradient selection waiting for its Apply is what the chord
    // finishes while that tool is out, as Enter on the canvas does.
    if (draft.selection && draft.selection.tool === editorState.editor.tool) {
      void applySelection()
      return
    }
    if (editorState.editor.run.active) return
    // The panel's own button, so the chord is refused exactly when it is (no
    // runtime, models missing, the cloud not set up) and the note says why.
    // With the panel not showing, the first press shows it.
    const button = document.querySelector('[data-run-action]')
    if (button instanceof HTMLButtonElement) {
      if (!button.disabled) button.click()
      return
    }
    if (editorState.editor.tool !== 'autoClean') editorState.setTool('autoClean')
    else openWindow('tool')
  }

  /** Text cleanup's scope as the panel reads it: a stored Project is Chapter while a half is on the cloud. */
  function cleanScope() {
    const stored = editorState.editor.toolParams.autoClean?.scope
    if (stored === 'project') return projectRuns() ? 'project' : 'chapter'
    return stored === 'chapter' ? 'chapter' : 'page'
  }

  function projectRuns() {
    const parts = cloudParts(runStep())
    return !parts.detect && !parts.clean
  }

  /**
   * `V` and `Shift+V`: Text cleanup's scope one step on or back. With another
   * tool out, the first press opens Text cleanup, where the scope is shown.
   *
   * @param {number} direction
   */
  function cycleCleanScope(direction) {
    const { editor } = editorState
    if (editor.tool !== 'autoClean') {
      editorState.setTool('autoClean')
      return
    }
    const scopes = projectRuns() ? ['page', 'chapter', 'project'] : ['page', 'chapter']
    const at = scopes.indexOf(cleanScope())
    editorState.setToolParam('autoClean', 'scope', scopes[(at + (direction < 0 ? scopes.length - 1 : 1)) % scopes.length])
  }

  const BAR_CONTROLS = 'section[data-shell] [data-range-field] input[type="range"], section[data-shell] select, section[data-shell] input[type="checkbox"]'

  /**
   * `\`: focus the tool bar's next control - a slider, a dropdown, a box -
   * and from the last one the first. Every option on the bar is then on the
   * arrows, the wheel, Space or the typed box, without a key of its own each.
   * On the pill, the rows kept in Adjustments come next: past the pill's last
   * control the popover opens and its first one takes the focus.
   */
  async function focusNextOption() {
    const controls = () => [...document.querySelectorAll(BAR_CONTROLS)]
      .filter((control) => !(/** @type {HTMLInputElement} */ (control).disabled))
    const before = controls()
    const at = before.indexOf(/** @type {any} */ (document.activeElement))
    const adjustments = document.querySelector('section[data-shell] [data-adjustments][aria-expanded="false"]')
    if (adjustments instanceof HTMLElement && (at === before.length - 1 || !before.length)) {
      adjustments.click()
      await tick()
      const opened = controls().filter((control) => !before.includes(control))
      if (opened.length) {
        /** @type {HTMLElement} */ (opened[0]).focus()
        return
      }
    }
    if (!before.length) return
    const next = /** @type {HTMLElement} */ (before[(at + 1) % before.length])
    next.focus()
  }

  /** `B`: the selected detection cleaned, or the selected cleaned region tried again. */
  function cleanSelectedRegion() {
    const region = editorState.selectedRegion()
    if (!region) return
    if (isDetected(region)) void runMaskAction('cleanDetected', region)
    else void runRegionMenuItem('retry', region)
  }

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    // Something nearer the key already dealt with it - a rail, a segmented
    // group, the canvas, a window's grip. The layer is the last resort, not
    // the first, and a key handled twice is a key that does two things.
    if (event.defaultPrevented) return
    const shortcut = matchShortcut(event, {
      scope: app.route.name === 'editor' ? 'editor' : 'home',
      modalOpen: isModalOpen(),
      textEntry: isTextEntry(event.target),
    })
    if (!shortcut) return
    // Auto-repeat is opt-in per entry: holding → should page, holding , must
    // not push a stack of Settings dialogs.
    if (event.repeat && !shortcut.repeatable) return
    event.preventDefault()
    shortcut.run(commands)
  }

  /**
   * The only key with a release behaviour: `O` shows the original while held -
   * or whatever the user has rebound it to, which is why the question is asked
   * of the table rather than of a constant. Unconditional - if the key went
   * down, its release must be honoured even if a dialog opened in between, or
   * the original would stay stuck on.
   *
   * @param {KeyboardEvent} event
   */
  function onkeyup(event) {
    if (releasesHoldOriginal(event)) commands.holdOriginal(false)
  }

  // The table is hand-maintained; two entries claiming one key in overlapping
  // scopes is the invariant easiest to break, and silently shadows a binding.
  if (import.meta.env?.DEV) {
    for (const clash of shortcutConflicts()) {
      console.warn(`[shortcuts] "${clash.key}" claimed by both ${clash.a} and ${clash.b}`)
    }
  }
</script>

<svelte:window onkeydown={onkeydown} onkeyup={onkeyup} />
