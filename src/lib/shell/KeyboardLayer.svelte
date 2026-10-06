<script>
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
  import { togglePanel } from '../state/session.svelte.js'
  import * as editorState from '../state/editor.svelte.js'
  import { deleteRow } from '../editor/maskactions.svelte.js'

  /** @type {import('../shortcuts.js').ShortcutCommands} */
  const commands = {
    selectToolSlot: (slot) => editorState.setToolBySlot(slot),
    cycleMaskSelectShape: () => editorState.cycleMaskSelectShape(),
    toggleMaskSelectMode: () => editorState.toggleMaskSelectMode(),
    holdOriginal: (held) => editorState.holdOriginal(held),
    togglePinOriginal: () => editorState.togglePinOriginal(),
    toggleMaskOverlay: () => editorState.toggleMaskOverlay(),
    toggleReviewFilter: () => editorState.toggleReviewFilter(),
    undo: () => editorState.undo(),
    redo: () => editorState.redo(),
    deleteSelectedLayer,
    zoomFit: () => editorState.zoomFit(),
    zoomIn: () => editorState.zoomIn(),
    zoomOut: () => editorState.zoomOut(),
    zoomActual: () => editorState.zoomActual(),
    pageByArrow: (side) => editorState.pageByArrow(side),
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
    // named here on its own.
    if (isTextEntry(active) || active?.tagName === 'SELECT') {
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
