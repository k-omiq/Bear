<script>
  /**
   * `⊟ fit · − · readout · +`. The readout is a button: pressing it snaps to
   * 100%, which is what the design file's zoom label does and what `Z` does
   * from the keyboard.
   *
   * The two step buttons are disabled on the *drawn* scale, `fit` included: a
   * fitted 1600px page in a 1232px viewport is genuinely 32%, below
   * `MIN_ZOOM`, and an enabled `−` there would make the page *bigger* (Task 9).
   * That scale is `displayZoom()`, which is the fit scale under fit and the
   * chosen `editor.zoom` otherwise.
   */
  import {
    displayZoom,
    editor,
    zoomIn,
    zoomOut,
    zoomFit,
    zoomActual,
    MIN_ZOOM,
    MAX_ZOOM,
  } from '../state/editor.svelte.js'
  import { shortcutProps } from '../state/session.svelte.js'
  import { IconButton, Readout } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  const drawn = $derived(displayZoom())
  const percent = $derived(Math.round(drawn * 100))
  const text = $derived(editor.fit ? t('editor.zoom.fit') : `${percent}%`)
  // Named for what pressing it does, not for what it says - two controls in
  // one bar must not share an accessible name, and `Zoom fit` is already the
  // button to the left of it.
  const label = $derived(
    editor.fit
      ? t('editor.readout.zoomActualFromFit')
      : t('editor.readout.zoomActual', { percent }),
  )
</script>

<IconButton
  icon="zoom-fit"
  label={t('editor.action.zoomFit')}
  {...shortcutProps('zoom.fit')}
  size={28}
  active={editor.fit}
  pressed={editor.fit}
  onclick={zoomFit}
/>
<IconButton
  icon="zoom-out"
  label={t('editor.action.zoomOut')}
  {...shortcutProps('zoom.out')}
  size={28}
  disabled={drawn <= MIN_ZOOM}
  onclick={zoomOut}
/>
<Readout {text} {label} {...shortcutProps('zoom.actual')} minWidth={44} onclick={zoomActual} />
<IconButton
  icon="zoom-in"
  label={t('editor.action.zoomIn')}
  {...shortcutProps('zoom.in')}
  size={28}
  disabled={drawn >= MAX_ZOOM}
  onclick={zoomIn}
/>
