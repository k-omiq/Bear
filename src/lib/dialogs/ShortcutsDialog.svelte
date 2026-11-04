<script>
  /**
   * The shortcut sheet on its own - `?` pushes `{kind: 'shortcuts'}` from
   * anywhere, and it must not depend on Settings being open.
   *
   * The body is `ShortcutSheet`, the same component Settings mounts - and the
   * same one a shortcut is rebound in, so `?` reaches the editor as well as the
   * list. Only the heading level differs: here the group headings sit directly
   * under the dialog title, so they are `h3`.
   *
   * The body is not a tab stop of its own, as Settings' scrollers are. The
   * sheet opens on its search field, which the dialog's first focus lands on,
   * and every row under it is a control, so Tab walks the whole list and the
   * scroller follows. A focusable wrapper took that first focus instead, and
   * being taller than the dialog, scrolled the list to its end on opening.
   */
  import { Button, Modal } from '../ui/index.js'
  import { closeModal, modalWidth } from '../state/app.svelte.js'
  import { t } from '../i18n/index.js'
  import ShortcutSheet from './ShortcutSheet.svelte'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()
</script>

<Modal title={t(spec.titleKey)} width={modalWidth(spec.kind)} onclose={() => closeModal(null)}>
  <div class="body" role="group" aria-label={t(spec.titleKey)}>
    <ShortcutSheet headingLevel="h3" />
  </div>

  {#snippet buttons()}
    <Button variant="primary" onclick={() => closeModal('close')}>{t('shell.action.close')}</Button>
  {/snippet}
</Modal>

<style>
  /* One height for every tab and every search, so the dialog does not jump
     as the list changes under it: the list scrolls inside it instead, under
     the sheet's sticky search and tabs. */
  .body {
    height: min(560px, calc(92vh - 130px));
    margin-top: 4px;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
</style>
