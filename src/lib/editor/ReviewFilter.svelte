<script>
  import { shortcutProps } from '../state/session.svelte.js'
  import { Button } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import { t } from '../i18n/index.js'

  /**
   * The `Needs review n` filter that heads the Layers panel - the review
   * surface. A toggle, so it carries `aria-pressed`; a
   * count, so it says how much there is to look at before it is pressed; and a
   * word beside the warning glyph, because a glyph on its own is not a status.
   *
   * The window header already switches its own count with the filter, so this
   * carries only its own.
   *
   * @type {{ count: number, active: boolean, ontoggle: () => void }}
   */
  let { count, active, ontoggle } = $props()

  const keys = $derived(shortcutProps('view.reviewFilter'))
</script>

<Button
  variant={active ? 'primary' : 'ghost'}
  block
  title={keys.shortcut ? t('masks.filter.hint', { shortcut: keys.shortcut }) : t('masks.filter.hintUnbound')}
  aria-pressed={active}
  aria-keyshortcuts={keys.keyshortcuts || undefined}
  onclick={ontoggle}
>
  <span class="fill">
    <span class="lead">
      <span class="glyph" class:on={active} aria-hidden="true">
        <Icon name="warning-triangle" size={13} />
      </span>
      {t('masks.filter.needsReview')}
    </span>
    <span class="count" class:none={count === 0} class:on={active}>{count}</span>
  </span>
</Button>

<style>
  .fill {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex: 1;
    gap: var(--s-3);
    font-size: 11.5px;
  }
  .lead { display: flex; align-items: center; gap: 7px; white-space: nowrap }
  .glyph { display: flex; color: var(--t3) }

  .count { font-size: 11px; color: var(--text) }
  .count.none { color: var(--t3) }
  /* Pressed, the whole control is the accent fill: the count and the glyph
     ride its foreground rather than keeping a colour of their own. */
  .on { color: inherit }
</style>
