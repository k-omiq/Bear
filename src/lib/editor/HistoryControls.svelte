<script>
  /**
   * `↶ ↷` - undo and redo, global across every tool.
   *
   * The pending command's label is part of the control's name, not only of its
   * tooltip: "Undo - delete mask" tells a keyboard user what is about to be
   * reversed before they press it. The command label arrives as an i18n key,
   * which `t()` resolves through the `*Key` param convention.
   */
  import {
    undo,
    redo,
    undoAvailable,
    redoAvailable,
    undoLabelKey,
    redoLabelKey,
  } from '../state/editor.svelte.js'
  import { shortcutProps } from '../state/session.svelte.js'
  import { IconButton } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  const canUndo = $derived(undoAvailable())
  const canRedo = $derived(redoAvailable())
  const undoKey = $derived(undoLabelKey())
  const redoKey = $derived(redoLabelKey())

  const undoLabel = $derived(
    undoKey ? t('editor.action.undoCommand', { commandKey: undoKey }) : t('editor.action.undo'),
  )
  const redoLabel = $derived(
    redoKey ? t('editor.action.redoCommand', { commandKey: redoKey }) : t('editor.action.redo'),
  )
</script>

<IconButton
  icon="undo"
  label={undoLabel}
  {...shortcutProps('edit.undo')}
  size={28}
  disabled={!canUndo}
  onclick={undo}
/>
<IconButton
  icon="redo"
  label={redoLabel}
  {...shortcutProps('edit.redo')}
  size={28}
  disabled={!canRedo}
  onclick={redo}
/>
