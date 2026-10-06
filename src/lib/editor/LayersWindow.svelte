<script>
  /**
   * The Layers window - masks and review. The shell is `FloatingWindow`; the
   * body is `MaskList`, which Task 8 owns (see that file's header for the
   * contract).
   */
  import { editor, scopedRegions } from '../state/editor.svelte.js'
  import { candidateCount, flaggedCount } from './maskrows.js'
  import { isDetected } from '../model/masks.js'
  import { t } from '../i18n/index.js'
  import FloatingWindow from './FloatingWindow.svelte'
  import MaskList from './MaskList.svelte'

  // Every count is of the panel's scope - the open page, or the strip's
  // viewport in longstrip. A header counting the chapter while the list showed
  // one page would be two answers to one question. Each is read off the same
  // region state the rows are drawn from (`model/review.js#regionState`), so
  // the header, the rows and the page's own counts cannot disagree.
  const regions = $derived(scopedRegions())
  // Applied is every layer on the page, flagged or not: a detection holds its
  // mask but has changed nothing yet, so it is counted as found instead.
  // A detection flagged for repair is still a detection: it is found, and it
  // is flagged, but nothing on the page is applied for it.
  const applied = $derived(regions.filter((region) => region.mask && !isDetected(region)).length)
  const detected = $derived(regions.filter(isDetected).length)
  const flagged = $derived(flaggedCount(regions))
  // Held candidates are neither applied nor flagged. They are said apart, so
  // a page whose text is all cleaned still shows there is something to choose.
  const candidates = $derived(candidateCount(regions))

  // The header counts whatever the panel is currently listing. A page with
  // only detections says so, rather than "no masks" over a list of them.
  const base = $derived(
    editor.reviewFilter
      ? t('editor.meta.masksFlagged', { count: flagged })
      : !applied && detected
        ? t('editor.meta.detected', { count: detected })
        : t('editor.meta.masksApplied', { count: applied }),
  )
  // The review filter lists flagged regions only, so its header does not
  // count candidates it is not showing.
  const meta = $derived(
    candidates > 0 && !editor.reviewFilter
      ? t('review.meta.withCandidates', { base, count: candidates })
      : base,
  )
</script>

<FloatingWindow id="layers" title={t('editor.panel.layers')} {meta} pad="0 7px 12px">
  <MaskList />
</FloatingWindow>
