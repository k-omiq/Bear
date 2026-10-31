<script>
  /**
   * The view pill: Original (held), the wipe between original and cleaned, its
   * readout, the sticky pin, and the mask overlay.
   *
   * Original is held - press and the source shows, release and it goes - and
   * the pin beside it is the sticky equivalent every held-key interaction
   * requires. The hold works from the keyboard
   * too (Enter or Space held on the button), and `O` / `⇧O` do the same thing
   * globally. When Settings has `originalView: 'pinned'` the press toggles the
   * pin instead; that decision belongs to the state module, not here.
   *
   * The readout is `aria-hidden`: it repeats the slider's own value, which the
   * slider already carries as `aria-valuetext`, and two voices on one number
   * is worse than one.
   */
  import {
    editor,
    holdOriginal,
    togglePinOriginal,
    toggleMaskOverlay,
    setWipe,
  } from '../state/editor.svelte.js'
  import { shortcutProps } from '../state/session.svelte.js'
  import { IconButton, Range } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  // The two ends are words and are translated; everything between them is the
  // bare figure with a percent sign, which is locale-neutral and tabular - the
  // same treatment `Slider` gives its own unit.
  //
  // **Budget for Task 11's catalogue: four characters.** The readout box is
  // 30px wide at 10px type, and the design file prints `clean` and `orig` in
  // it. `editor.wipe.clean` and `editor.wipe.original` are the *abbreviated*
  // strings - the full words are spoken instead, from the slider's
  // `aria-valuetext`, which is this same string. Anything longer is clipped
  // rather than wrapped; the box must not grow, or the view pill's controls
  // shift every time the wipe passes 0 or 100.
  const wipeText = $derived(
    editor.wipe >= 100
      ? t('editor.wipe.clean')
      : editor.wipe <= 0
        ? t('editor.wipe.original')
        : `${editor.wipe}%`,
  )

  /** @param {KeyboardEvent} event */
  function onholdkeydown(event) {
    if (event.repeat) return
    if (event.key !== 'Enter' && event.key !== ' ') return
    event.preventDefault()
    holdOriginal(true)
  }

  /** @param {KeyboardEvent} event */
  function onholdkeyup(event) {
    if (event.key !== 'Enter' && event.key !== ' ') return
    holdOriginal(false)
  }
</script>

<IconButton
  icon={editor.originalVisible ? 'eye' : 'eye-off'}
  label={t('editor.action.originalHold')}
  {...shortcutProps('view.holdOriginal')}
  active={editor.originalVisible}
  pressed={editor.originalVisible}
  onpointerdown={() => holdOriginal(true)}
  onpointerup={() => holdOriginal(false)}
  onpointerleave={() => holdOriginal(false)}
  onpointercancel={() => holdOriginal(false)}
  onkeydown={onholdkeydown}
  onkeyup={onholdkeyup}
/>

<Range
  label={t('editor.action.wipe')}
  value={editor.wipe}
  min={0}
  max={100}
  step={1}
  width={92}
  valueText={wipeText}
  onchange={setWipe}
/>
<span class="readout" aria-hidden="true">{wipeText}</span>

<IconButton
  icon="pin"
  label={t('editor.action.originalPin')}
  {...shortcutProps('view.pinOriginal')}
  active={editor.originalPinned}
  pressed={editor.originalPinned}
  onclick={togglePinOriginal}
/>
<IconButton
  icon="mask-overlay"
  label={t('editor.action.maskOverlay')}
  {...shortcutProps('view.maskOverlay')}
  active={editor.maskOverlay}
  pressed={editor.maskOverlay}
  onclick={toggleMaskOverlay}
/>

<style>
  .readout {
    width: 30px;
    flex: none;
    font-size: 10px;
    color: var(--t3);
    text-align: right;
    white-space: nowrap;
    /* Over budget, a translation is clipped with an ellipsis rather than
       spilling across the pin beside it. The word is still spoken in full. */
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
