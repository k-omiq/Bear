<script>
  import { t } from '../i18n/index.js'

  /**
   * One row of the Pages list: the status mark, the page label, the slim track
   * the finished regions fill, and the `cleaned / total` ratio.
   *
   * The whole row carries one accessible name - mark, label and ratio read as
   * a sentence rather than as three fragments - so its parts are hidden from
   * assistive tech and the name is built from named placeholders. A skipped
   * page carries its reason both there and in the tooltip, which is the only
   * place the reason appears.
   *
   * Nothing here is conveyed by colour alone: the mark is a glyph, the review
   * count is a figure, the track's tone repeats what the mark already says.
   *
   * `total` is the length of the whole list, not of the rendered band: above
   * the virtualisation threshold most rows are not mounted, and without it a
   * screen reader counts "3 of 19" through a 400-page chapter.
   *
   * @type {{
   *   row: import('./pagerows.js').PageRow,
   *   total: number,
   *   selected: boolean,
   *   focused: boolean,
   *   onpick: () => void,
   * }}
   */
  let { row, total, selected, focused, onpick } = $props()

  const label = $derived(t(row.labelKey, { number: row.number }))
  const ratio = $derived(row.total > 0 ? `${row.cleaned} / ${row.total}` : '·')
  // Three weights, as the design file has them: flagged is the loudest, a page
  // with nothing left to do is the middle one, and everything else recedes.
  const complete = $derived(row.total > 0 && row.cleaned === row.total)

  const name = $derived(
    row.skipReasonKey
      ? t('pages.row.skipped', { label, reasonKey: row.skipReasonKey })
      : t('pages.row.name', {
          label,
          statusKey: row.mark.titleKey,
          count: row.mark.count,
          cleaned: row.cleaned,
          total: row.total,
        }),
  )
  // Held candidates are counted apart from the ratio (`model/status.js`): they
  // are not work left undone and not a problem, but a page carrying them has
  // something to choose, and the row says so in words as well as in a mark.
  const candidates = $derived(row.candidates ?? 0)
  const heldLabel = $derived(candidates > 0 ? t('review.page.candidates', { count: candidates }) : '')
  const fullName = $derived(heldLabel ? `${name}. ${heldLabel}` : name)
</script>

<button
  type="button"
  class="row"
  class:selected
  role="option"
  aria-selected={selected}
  aria-label={fullName}
  title={fullName}
  data-index={row.index}
  aria-setsize={total}
  aria-posinset={row.index + 1}
  tabindex={focused ? 0 : -1}
  onclick={onpick}
>
  <span class="mark {row.mark.tone}" aria-hidden="true">
    <span class="glyph">{row.mark.glyph}</span>
    {#if row.mark.count > 0}<span class="count">{row.mark.count}</span>{/if}
  </span>

  <span class="label" aria-hidden="true">{label}</span>

  <span class="track" aria-hidden="true">
    <span class="fill {row.tone}" style:--fill="{row.percent / 100}"></span>
  </span>

  {#if candidates > 0}<span class="held" aria-hidden="true">◌{candidates}</span>{/if}

  <span
    class="ratio"
    class:strong={row.tone === 'review'}
    class:complete
    aria-hidden="true">{ratio}</span>
</button>

<style>
  /* A real button, with `role="option"` over it: the row is one activation
     target, so Enter and Space come for free and only the arrow keys are the
     list's business. */
  .row {
    display: flex;
    align-items: center;
    flex: none;
    gap: var(--s-2);
    width: 100%;
    height: 25px;
    padding: 0 var(--s-3);
    border: none;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--t2);
    font-size: 11.5px;
    text-align: start;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease);
  }
  .row:hover { background: var(--accent-soft) }
  .selected { background: var(--accent-soft); color: var(--text) }

  /* Wider than the constraints table's 14px cell: the fifth
     mark is `✓ 2`, and a fixed cell is what keeps every label aligned whether
     or not its page has a count. */
  .mark {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: none;
    gap: 3px;
    width: 22px;
  }
  .glyph { font-size: 12px; line-height: 1 }
  .count { font-size: 9.5px; line-height: 1 }

  .muted { color: var(--t3) }
  .normal { color: var(--text) }
  .warn { color: var(--warn) }
  /* Text found and waiting to be cleaned: the accent, still, because nothing
     is happening to it until the user asks. */
  .detected { color: var(--accent) }
  /* "Cleaning now" - the prototype's own idiom, on top of a glyph that
     already differs from every other mark. Stopped by reduced motion. */
  .active {
    color: var(--accent);
    animation: mcBlink 900ms ease-in-out infinite;
  }
  /* The filled dot is optically larger than the other four marks at the same
     size, so it is set smaller. */
  .mark.active .glyph { font-size: 10px }

  /* The window resizes between 198 and 560px and a label is a translated
     string: it ellipsises, and it never wraps - a wrapped label would break
     the row's height, and with it the virtual window's arithmetic. */
  .label {
    flex: none;
    /* The design file's 40px is a floor, not a cap: every label in a chapter
       is the same length, so the column still lines up, and `p. 140` is not
       clipped to `p. 14…`. */
    min-width: 40px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .track {
    flex: 1;
    min-width: 24px;
    height: 4px;
    border-radius: 2px;
    background: var(--line);
    overflow: hidden;
  }
  /* Scaled rather than widened: during a run every visible row's fill moves at
     once, and animating width would lay out the whole list on each tick. The
     track's own radius and `overflow: hidden` shape the ends. */
  .fill {
    display: block;
    width: 100%;
    height: 100%;
    /* The fill carries the track's own radius, as the design file's `barStyle`
       does: `overflow: hidden` rounds the left end against the track, and this
       is what keeps the right end of a partial fill from ending square. */
    border-radius: 2px;
    background: var(--accent);
    transform: scaleX(var(--fill, 0));
    transform-origin: left center;
    transition: transform var(--dur-slow) var(--ease);
  }
  .fill.review { background: var(--tint) }
  .fill.warn { background: var(--warn) }

  /* Held candidates: the Layers row's own dotted mark, quiet, beside the
     ratio it is not part of. */
  .held {
    flex: none;
    font-size: 10px;
    color: var(--accent);
    white-space: nowrap;
  }

  .ratio {
    flex: none;
    min-width: 38px;
    text-align: right;
    font-size: 10.5px;
    color: var(--t3);
    white-space: nowrap;
  }
  /* Three tones, design file line 964: flagged, then finished, then the rest.
     `strong` wins - a page that needs review is not a finished page. */
  .ratio.complete { color: var(--t2) }
  .ratio.strong { color: var(--text) }
</style>
