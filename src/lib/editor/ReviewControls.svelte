<script>
  import { editor, reviewEntries, stepReview } from '../state/editor.svelte.js'
  import { shortcutProps } from '../state/session.svelte.js'
  import { IconButton, Readout } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  /**
   * `« »` - previous / next region needing review, with an `n / total`
   * readout, in the bottom-right pill beside the page controls.
   *
   * The set is the chapter's, computed by `model/review.js`; stepping it moves
   * the page as well as the selection, which is why it goes through
   * `stepReview` rather than through the selection directly. Before the user
   * has entered the set there is no current region, and the readout says so
   * with a dash rather than with a `0` that would read as a position.
   *
   * The double chevrons point the way the *set* runs, not the way the pages
   * do: the review set is in page order whichever direction the chapter reads,
   * so these two are the one pair in the chrome that reading direction does
   * not flip.
   */
  const entries = $derived(reviewEntries())
  const total = $derived(entries.length)
  const at = $derived(entries.findIndex((entry) => entry.id === editor.reviewCurrentId))
  const index = $derived(at >= 0 ? at + 1 : 0)

  const label = $derived(
    index > 0
      ? t('editor.readout.review', { index, total })
      : t('editor.readout.reviewPending', { total }),
  )
</script>

<IconButton
  icon="chevrons-left"
  label={t('editor.action.prevReview')}
  {...shortcutProps('review.prev')}
  size={28}
  disabled={total === 0}
  onclick={() => stepReview('prev')}
/>
<Readout text="{index > 0 ? index : '·'} / {total}" {label} minWidth={42} />
<IconButton
  icon="chevrons-right"
  label={t('editor.action.nextReview')}
  {...shortcutProps('review.next')}
  size={28}
  disabled={total === 0}
  onclick={() => stepReview('next')}
/>
