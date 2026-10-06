/**
 * Page status marks. Six marks, six outcomes - this is
 * also the Pages list's progress indicator, so `pageMark` is called once per
 * row on every render.
 *
 * The glyph, the review count and its translated title are returned
 * separately rather than pre-composed into "✓ 2", so the component controls
 * layout and the i18n string controls word order (`t('pages.status
 * .cleanedNeedsReview', { count })`) instead of this module assuming
 * English order (constraints.md).
 */

import { regionState } from './review.js'

/**
 * @typedef {Object} PageMark
 * @property {'·'|'●'|'✓'|'!'|'◇'} glyph
 * @property {'muted'|'active'|'normal'|'warn'|'detected'} tone
 * @property {number} count - regions needing review; nonzero only on the cleaned and detected marks
 * @property {string} titleKey
 */

/**
 * @typedef {Object} PageCounts
 * @property {number} total - regions of work found on the page: every region but a candidate
 * @property {number} done - regions masked and flagged for nothing
 * @property {number} review - regions flagged: failed or needing review
 * @property {number} candidates - held text-group candidates awaiting a choice
 */

/**
 * One page's three numbers - **from wherever they are honest**.
 *
 * A page inside the resident window is counted from the regions it is holding:
 * they are the live copy, and an edit changes them before any header can be
 * refreshed. A page outside it has no regions at all, and
 * counting `page.regions` there is what made the Pages list draw `0 / 0` under
 * a bare `·` for every page but the three in hand - so the header's own
 * `regionCount` / `doneCount` / `reviewCount` answer instead. The backend reads
 * them out of the manifest, which is why they are there the moment a project
 * reopens.
 *
 * `resident === false` is the only reading that means "not loaded": a fixture
 * or the mock carries whole pages and no flag at all, and those are counted
 * from their regions like the window's own.
 *
 * **A candidate is counted apart.** It is listed (the native `regionCount`
 * includes it) but it is neither work left undone nor a problem: counting it in
 * `total` would leave the track of a page whose text is all cleaned short for
 * as long as a sound effect sits there unchosen, and counting it in `review`
 * would make a held suggestion read as a fault.
 *
 * @param {import('./types.js').Page & {resident?: boolean, regionCount?: number, doneCount?: number, reviewCount?: number, candidateCount?: number}} page
 * @returns {PageCounts}
 */
export function pageCounts(page) {
  if (page.resident === false) {
    const candidates = page.candidateCount ?? 0
    return {
      total: Math.max(0, (page.regionCount ?? 0) - candidates),
      done: page.doneCount ?? 0,
      review: page.reviewCount ?? 0,
      candidates,
    }
  }
  const regions = page.regions ?? []
  let done = 0
  let review = 0
  let candidates = 0
  for (const region of regions) {
    const state = regionState(region)
    if (state === 'candidate') candidates += 1
    else if (state === 'failed' || state === 'needsReview') review += 1
    // A detection holds its fitted mask and is still waiting: found, not done.
    else if (state === 'cleaned') done += 1
  }
  return { total: regions.length - candidates, done, review, candidates }
}

/**
 * Write a resident page's counts back onto its header, so the row and the
 * regions cannot disagree once the page is evicted or an edit has moved it.
 *
 * @param {import('./types.js').Page & {regionCount?: number, doneCount?: number, reviewCount?: number, candidateCount?: number}} page
 * @returns {import('./types.js').Page}
 */
export function recountPage(page) {
  const counts = pageCounts(page)
  // `regionCount` is every region listed, as the native header counts it.
  page.regionCount = counts.total + counts.candidates
  page.doneCount = counts.done
  page.reviewCount = counts.review
  page.candidateCount = counts.candidates
  return page
}

/**
 * @param {import('./types.js').Page} page
 * @returns {PageMark}
 */
export function pageMark(page) {
  if (page.status === 'skipped') {
    return { glyph: '!', tone: 'warn', count: 0, titleKey: 'pages.status.skipped' }
  }
  if (page.status === 'cleaning') {
    return { glyph: '●', tone: 'active', count: 0, titleKey: 'pages.status.cleaning' }
  }
  if (page.status === 'unclean') {
    return { glyph: '·', tone: 'muted', count: 0, titleKey: 'pages.status.unclean' }
  }
  // Every page of the chapter carries this mark, and only three of them carry
  // their regions - so the count comes through `pageCounts`, not off the list.
  const { total, review: count, candidates } = pageCounts(page)
  // Text found and stored, nothing cleaned yet: the page is waiting on a
  // Clean. An outline rather than the tick, because nothing on it is done. A
  // detection can still be flagged (a committed cloud result for it went
  // missing), and the outline carries that count like the tick does.
  if (page.status === 'detected') {
    return { glyph: '◇', tone: count > 0 ? 'warn' : 'detected', count, titleKey: 'pages.status.detected' }
  }
  if (total === 0 && candidates > 0 && count === 0) {
    return { glyph: '◇', tone: 'detected', count: 0, titleKey: 'pages.status.held' }
  }
  if (count > 0) {
    return { glyph: '✓', tone: 'warn', count, titleKey: 'pages.status.cleanedNeedsReview' }
  }
  return { glyph: '✓', tone: 'normal', count: 0, titleKey: 'pages.status.cleaned' }
}
