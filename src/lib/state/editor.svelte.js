/**
 * The open chapter: everything the editor screen, its panels, its canvas and
 * its tools read and write.
 *
 * This module owns editor state from the backend event channel
 * (`backend.subscribe`). Events become page marks, region updates and run
 * counters here; `notice` events are forwarded to `app.notify`. The cloud GPU
 * tracker listens separately only for run endings, since it outlives the editor.
 *
 * Autosave restores page, scroll position, zoom and the
 * user's position in the review set when a chapter is reopened. The review set
 * itself is *recomputed* from the chapter on open rather than restored from
 * disk - it is derived state, and recomputing it is both cheaper and more
 * correct than trusting a stale copy.
 */

import { getBackend } from '../api/backend.js'
import { presentNotice } from '../model/cloudnotices.js'
import {
  adopt as adoptHistory,
  createHistory,
  push as pushHistory,
  undo as undoHistory,
  redo as redoHistory,
  canUndo,
  canRedo,
  undoLabel,
  redoLabel,
} from '../model/history.js'
import { reviewList, reviewReason, stepIssue } from '../model/review.js'
import { runDetection, unifyAnalysisTargets } from '../model/pipelines.js'
import { recountPage } from '../model/status.js'
import { toolSpec } from '../editor/tools.js'
import { MAX_MASK_PADDING } from '../model/masks.js'
import { evictRegions, slideWindow, windowIndices } from './pagewindow.svelte.js'
import { pageNavControls, defaultReadingDirection } from '../model/paging.js'
import { clearCloneSource, clearDraft, resetDraftState } from '../editor/draft.svelte.js'
import { notify, pushModal } from './app.svelte.js'
import { session, openWindow } from './session.svelte.js'
import { loadAutosave, storeAutosave } from './autosave.js'
import { numberIn } from './persist.js'
import { settleCreation } from './heldcreations.js'
import { RUN_KINDS, jobById, refreshJobs, registerJob } from './jobs.svelte.js'

const AUTOSAVE_DEBOUNCE_MS = 250
const HISTORY_RETRY_MS = 50
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

/** Tool ids, in the order the `1` to `6` shortcuts and the tool rail use. */
export const TOOLS = /** @type {const} */ ([
  'autoClean',
  'brush',
  'shapes',
  'aiMaskBrush',
  'cloneHeal',
  'maskSelect',
])

/**
 * The ceiling every automatic run is pinned to: the highest rung of
 * `src/lib/model/ladder.js` that runs on this machine.
 *
 * This is the enforcement of the ruling in `src/lib/editor/tools.js` - a batch
 * run is local-only - and it lives here, at the one call site that sends, so
 * that it holds however the settings are configured. Every cloud render asks
 * for consent first, one request at a time
 * (`src/lib/editor/cloudflow.svelte.js`), and the run protocol has no step
 * that could ask, so a run that could reach the cloud would send pages
 * without it. The cloud stays reachable per region, from the AI mask brush
 * and from a Layers row, each of which asks.
 *
 * If `runClean` ever grows a consent step, this constant is what gives way -
 * not the gate in `src/lib/editor/cloudflow.svelte.js`.
 */
export const LOCAL_CEILING = 'lama'

export const MIN_ZOOM = 0.4
export const MAX_ZOOM = 2.4
export const ZOOM_STEP = 0.15

/**
 * Starting parameters per tool. The parameter *rows* - their ranges, steps and
 * options - are declared once in `src/lib/editor/tools.js`; this is only the
 * value each one starts at, kept here because it is chapter-session state.
 *
 * Auto clean carries no `engineCeiling`: a run is local-only, and its ceiling
 * is pinned at `LOCAL_CEILING` by `startRun` rather than chosen here. See
 * `src/lib/editor/tools.js` for why.
 *
 * Its two engine picks name a **model each**
 * and are the defaults the two kinds of text want: a speech balloon is flat
 * paper and rung 0's fill covers it exactly, and text over art has to have the
 * art put back, which is LaMa's job. Neither is a ceiling - the run escalates
 * past either one when the quality metric declines the patch.
 *
 * @returns {Record<string, Record<string, unknown>>}
 */
function defaultToolParams() {
  return {
    autoClean: {
      // The step (docs/detect-clean.md): find and store the regions, clean
      // the stored ones, or both, which is what Auto clean always did.
      step: 'auto',
      scope: 'page',
      bubbleEngine: 'fill',
      bubbleColor: '#ffffff',
      outsideEngine: 'lama',
      outsideBubbles: 'review',
      // Page pixels every detected mask is grown by: Detect stores masks this
      // much wider, and Apply re-pads the ones already stored.
      maskPadding: 0,
    },
    // `mode` is pinned to `paint` and has no control: the brush lays down a
    // colour and nothing else (`tools.js`). It stays in the record because it
    // is the field the seam reads to tell a paint stroke from a mask stroke
    // (`paint.js#paintParamsOf`, `region.rs#paint_plan`).
    brush: {
      size: 28,
      hardness: 100,
      spacing: 12,
      mode: 'paint',
      color: '#ffffff',
      opacity: 100,
      flow: 100,
    },
    // A new shape paints with a solid colour. Its outline is optional.
    shapes: { shape: 'rect', mode: 'solid', color: '#ffffff', opacity: 100, outlineColor: '#000000', outlineWidth: 0, feather: 0 },
    // The AI mask brush defaults to the local LaMa Manga model.
    aiMaskBrush: { engine: 'lama', size: 36 },
    contentAwareFill: { fillMode: 'match-surround', engine: 'local' },
    cloneHeal: {
      size: 32,
      hardness: 60,
      opacity: 100,
      flow: 100,
      alignment: 'aligned',
      mode: 'heal',
    },
    // The selection tool edits the detected masks Clean erases: it adds by
    // default, with a round brush.
    maskSelect: { mode: 'add', shape: 'brush', size: 32 },
  }
}

/**
 * @typedef {Object} RunState
 * @property {boolean} active
 * @property {string|null} runId
 * @property {'page'|'chapter'|'project'|null} scope
 * @property {import('../api/backend.js').RunMode} mode - the step the run takes, which the status line names
 * @property {number} queued
 * @property {number} pagesDone
 * @property {number|null} currentPageIndex - the page showing the `●` mark
 * @property {number|null} nextPageIndex - where a resume picks up, set on cancel
 */

export const editor = $state({
  /** @type {import('../api/backend.js').ApiProject|null} */
  project: null,
  /** @type {import('../api/backend.js').ApiChapter|null} */
  chapter: null,
  loading: false,

  /* view */
  pageIndex: 0,
  /**
   * Where the canvas is scrolled to. `top` is measured from the position
   * where the page's top sits at its place under the top bar, not from the
   * scroller's own zero: the canvas keeps `scrollRoom` of empty space above
   * the page, and a position that counted it would move with the window's
   * height. Negative while the page is pulled down into that space.
   */
  scroll: { top: 0, left: 0 },
  /**
   * The empty space the canvas keeps above and below the page, in CSS pixels
   * (`CanvasStage.svelte`, "Scroll room"). The canvas reports it; the
   * scroller's `scrollTop` is `scroll.top` plus this. Never saved.
   */
  scrollRoom: 0,
  /**
   * The zoom the user chose: the fraction of the page's own pixels the sheet
   * is drawn at, so `1` is 1:1. **Always inside `[MIN_ZOOM, MAX_ZOOM]`** -
   * `setZoom` is the only writer and it clamps.
   *
   * It is not necessarily what the page is drawn at: with `fit` on, that is
   * `fitScale`. Read `displayZoom()` for "how big is the page right now",
   * which is what the readout, the step buttons and the pinch all want.
   */
  zoom: 1,
  fit: true,
  /**
   * What "fit" works out to for the page and the viewport as they are now,
   * reported by the canvas through `reportFitScale`.
   *
   * A field of its own rather than a write into `zoom`, because a fitted
   * 1600px page in a 1232px viewport is genuinely 32% - below `MIN_ZOOM`, and
   * not a zoom anybody chose. Keeping it here is what lets `zoom` hold its
   * documented range while the readout still tells the truth. Session-only: it
   * is recomputed from the viewport on every open, so it is never autosaved.
   */
  fitScale: 1,

  /* longstrip - see "The strip handshake" below */
  /** @type {number[]} the positions the strip currently has on screen */
  stripScope: [],
  stripFocus: -1,
  /**
   * @type {number[]} the positions the column has mounted - on screen, the
   * screen fetched ahead and the overscan - which the window keeps resident
   * with one neighbour either side, so a sheet has its patch layers before it
   * scrolls into view (`editor/PatchLayers.svelte`)
   */
  stripMounted: [],
  /** @type {{index: number, token: number}|null} */
  stripScrollRequest: null,

  /* tools */
  /** @type {string} */
  tool: 'autoClean',
  toolParams: defaultToolParams(),

  /* interaction */
  /** @type {string|null} */
  selectionId: null,
  /** @type {string|null} */
  hoverId: null,

  /* overlays */
  /**
   * Whether **every** region's outline is drawn at once (`M`).
   *
   * Off by default. A region's outline is otherwise per-region and transient -
   * hover, focus or selection - and a chapter that opens with a box around
   * every bubble is one the user has to switch off before they can judge the
   * artwork underneath, which is the only thing the editor is for.
   */
  maskOverlay: false,
  /** true while the original is shown - held (`O`) or pinned (`⇧O`) */
  originalVisible: false,
  originalPinned: false,
  /** The wipe between original (0) and cleaned (100). */
  wipe: 100,

  /* review */
  reviewFilter: false,
  /** @type {string|null} */
  reviewCurrentId: null,

  /* run */
  /** @type {RunState} */
  run: {
    active: false,
    runId: null,
    scope: null,
    mode: 'auto',
    queued: 0,
    pagesDone: 0,
    currentPageIndex: null,
    nextPageIndex: null,
  },

  /**
   * The undo journal's **index** - a cursor and one `{seq, label}` per entry.
   * The deltas themselves live in the project folder and are
   * fetched one at a time, at the moment they are replayed, so this is flat in
   * the size of the edits rather than in their number times their size.
   */
  history: createHistory(),
})

/* ------------------------------------------------------------------ */
/* Derived reads                                                       */
/* ------------------------------------------------------------------ */

/** @returns {import('../api/backend.js').ApiPage[]} */
export function pages() {
  return editor.chapter?.pages ?? []
}

/** @returns {import('../api/backend.js').ApiPage|null} */
export function currentPage() {
  return pages()[editor.pageIndex] ?? null
}

/**
 * The page a run is working on right now, which is **not** the page being
 * looked at: a run walks the queue while the user stays where they are, and
 * reading the viewed page for the run's status line froze it at whichever page
 * the run happened to start from.
 *
 * @returns {import('../api/backend.js').ApiPage|null}
 */
export function runningPage() {
  const index = editor.run.currentPageIndex
  return index == null ? null : (pages()[index] ?? null)
}

/** @returns {number} */
export function pageCount() {
  return pages().length
}

/**
 * The reading direction chosen in Settings applies immediately, including to
 * existing projects. A project's recorded import default is only a fallback.
 * @returns {'rtl'|'ltr'}
 */
export function readingDirection() {
  return session.readingDirection ?? editor.project?.readingDirection ?? defaultReadingDirection()
}

/**
 * Every region in the chapter that needs review, in page order.
 *
 * **Read from the chapter's review index, not derived from its regions.** The
 * review set is chapter-wide by nature - the next issue may be eleven pages
 * away - and deriving it meant every region of every page had to be resident,
 * which is precisely what the resident window removes. The backend builds the
 * index one page at a time and hands over ~90 bytes an entry;
 * `syncReviewForPage` keeps the entries of pages the interface is holding in
 * step as they are edited.
 *
 * @returns {import('../api/backend.js').ReviewRef[]}
 */
export function reviewEntries() {
  return editor.chapter?.review ?? []
}

/**
 * Which pages the Layers panel is looking at.
 *
 * Single page: the open page, and nothing else. **Longstrip**: the positions
 * the strip actually has on screen, which the canvas
 * reports through `setStripScope` on every scroll and resize. Before the strip
 * has measured itself - the first frame after a chapter opens - it falls back
 * to the current position, so the panel is never momentarily empty.
 *
 * @returns {number[]} page indices, in strip order
 */
export function scopePageIndices() {
  const last = pageCount() - 1
  if (last < 0) return []
  if (editor.project?.mode !== 'longstrip') return [editor.pageIndex]
  const scope = editor.stripScope.filter((index) => index >= 0 && index <= last)
  return scope.length > 0 ? scope : [editor.pageIndex]
}

/**
 * Every region the Layers panel lists, in page order. The panel's own review
 * filter runs `reviewList` over exactly this, which makes the filtered list a
 * contiguous slice of `reviewEntries()` - the set the bottom-right pill steps
 * through - rather than a second ordering of the same regions.
 *
 * @returns {import('../api/backend.js').ApiRegion[]}
 */
export function scopedRegions() {
  const all = pages()
  const regions = []
  for (const index of scopePageIndices()) {
    const page = all[index]
    if (page) regions.push(...page.regions)
  }
  return regions
}

/**
 * Replace a region in the open chapter with the version a backend call
 * returned. The event channel does the same thing for a run's regions
 * (`region-done`); this is the direct-call half, for the panel's delete,
 * re-run, clean-anyway and undo.
 *
 * `pageStatus` is the page the region sits on, as the backend now holds it.
 * Some region-level edits move the page - cleaning a gate-skipped region on an
 * unclean page, deleting the last mask on a cleaned one - and a region put
 * back without its page's status is how a full track ends up under a "not
 * cleaned" mark. Omit it only for an edit that cannot move the page.
 *
 * **A page the window has evicted still takes the answer.** An edit can be
 * answered after the reader has turned far enough for its page to be reduced
 * to a header - a cloud render takes minutes - and the native side has
 * already written the result by then. The regions are not in hand, so only
 * the header moves (its status, and the review index entry), and the answer
 * counts as landed so the caller records its undo entry. A region missing from
 * a page that **is** in hand is different: it was deleted meanwhile, and that
 * answer is refused.
 *
 * @param {import('../api/backend.js').ApiRegion|null} region
 * @param {string} [pageStatus]
 * @returns {boolean} whether the region was found and replaced, or its page is out of the window
 */
export function replaceRegion(region, pageStatus) {
  if (!region) return false
  for (const page of pages()) {
    const index = page.regions.findIndex((candidate) => candidate.id === region.id)
    if (index >= 0) {
      page.regions[index] = region
      if (pageStatus) page.status = pageStatus
      // An edit can add a region to the review set or take it out of it, and
      // neither the set nor the row's counts are derived from the regions on
      // every read any more.
      recountPage(page)
      syncReviewForPage(page)
      return true
    }
  }
  const page = pages().find((candidate) => candidate.id === region.pageId) ?? pageHolding(region.id)
  // `resident: false` is what an eviction writes; a page without the flag at
  // all is one the window never reduced, so it is treated as in hand.
  if (!page || page.resident !== false) return false
  if (pageStatus) page.status = pageStatus
  syncReviewEntry(page, region)
  return true
}

/**
 * The page a region id belongs to, read off the id itself.
 *
 * Every region id is built from its page's - the backend's `chapter_holding`
 * is the same reading one level up - so a page whose id, plus the separator,
 * prefixes the region id is the page that holds it. The route for a page whose
 * regions are **not** in hand: there is nothing to search there, and the id is
 * the only thing left that says where the region lived.
 *
 * **Longest prefix wins**, for the reason the backend gives: page ids are
 * minted from a counter and are not prefix-free, so a shorter match can name
 * the wrong page.
 *
 * @param {string} regionId
 * @returns {import('../api/backend.js').ApiPage|null}
 */
function pageHolding(regionId) {
  let best = null
  for (const page of pages()) {
    if (!regionId.startsWith(`${page.id}-`)) continue
    if (!best || page.id.length > best.id.length) best = page
  }
  return best
}

/**
 * Whether the open chapter still has a region: on a page whose regions are in
 * hand, or on a page the window has reduced to a header - whose regions are
 * not in hand, so nothing there says it is gone. A region missing from a page
 * that **is** in hand was deleted, the rule `replaceRegion` refuses a late
 * answer by.
 *
 * @param {string} regionId
 * @returns {boolean}
 */
export function holdsRegion(regionId) {
  if (pages().some((page) => page.regions.some((candidate) => candidate.id === regionId))) return true
  return pageHolding(regionId)?.resident === false
}

/**
 * A region has left the chapter: drop every id still pointing at it.
 *
 * The highlight contract says a selection is sticky until something else is
 * selected, the page changes or `Esc` - none of which covers *the region
 * itself is gone*, and a selection pointing at nothing highlights nothing,
 * steps to nothing, and hands `⌘⌫` a region the backend no longer has. One
 * place rather than at each delete, because there are four routes into a
 * removal now (the row's trash, the context menu, the shortcut, an undo or a
 * redo replaying any of them) and three of them used to leave the id behind.
 *
 * `hoverId` goes with it: whoever set it clears it on the way out, but a row
 * that was removed under the pointer never gets its `pointerleave`.
 *
 * @param {string} regionId
 */
function forgetRegion(regionId) {
  if (editor.selectionId === regionId) editor.selectionId = null
  if (editor.hoverId === regionId) editor.hoverId = null
}

/**
 * Put a region into the open chapter in whatever state a snapshot describes -
 * including "it was not there".
 *
 * The three cases are the three halves of a hand-drawn mask's life: it is
 * created (insert), it is undone (remove), it is redone (insert again). One
 * function so that a command's two directions cannot diverge, and so the
 * interface's copy always ends up matching what `restoreRegion` just did to the
 * backend's.
 *
 * @param {string} regionId
 * @param {import('../api/backend.js').ApiRegion|null} region - null means it was not there
 * @param {string} [pageStatus]
 * @returns {boolean} whether the chapter changed
 */
export function applyRegionState(regionId, region, pageStatus) {
  for (const page of pages()) {
    const index = page.regions.findIndex((candidate) => candidate.id === regionId)
    if (index < 0) continue
    if (region) page.regions[index] = region
    else {
      page.regions.splice(index, 1)
      forgetRegion(regionId)
    }
    if (pageStatus) page.status = pageStatus
    recountPage(page)
    syncReviewForPage(page)
    return true
  }
  if (!region) {
    const chapter = editor.chapter
    if (!chapter) return false
    // A removal for a page the window is not holding. Two ways to find that
    // page, and the second one is why this branch used to give up: the review
    // index knows where a *flagged* region lived, and a region that was never
    // flagged is not in it at all - so a replayed delete of an ordinary mask
    // on an evicted page left the page's own counts and status untouched, and
    // the Pages row went on marking a region that had gone.
    // Every region id is built from its page's, so the page is recoverable
    // from the id whether the index knows it or not.
    const reviewEntry = (chapter.review ?? []).find((candidate) => candidate.id === regionId)
    if (reviewEntry) {
      chapter.review = chapter.review.filter((candidate) => candidate.id !== regionId)
    }
    const page = reviewEntry
      ? pages().find((candidate) => candidate.id === reviewEntry.pageId)
      : pageHolding(regionId)
    if (!page) return !!reviewEntry
    if (page.regionCount) page.regionCount = Math.max(0, page.regionCount - 1)
    page.reviewCount = (chapter.review ?? []).filter(
      (candidate) => candidate.pageId === page.id,
    ).length
    // The page's regions are not in hand, so *what* was removed is unknown -
    // only that it is gone. `doneCount` is therefore clamped rather than
    // decremented: it cannot exceed what is left once the flagged regions are
    // taken off, which is what keeps the row from reading `3 / 2`. It can
    // still stand one high until the page is paged back in and recounted from
    // its regions.
    page.doneCount = Math.max(
      0,
      Math.min(page.doneCount ?? 0, (page.regionCount ?? 0) - page.reviewCount),
    )
    if (pageStatus) page.status = pageStatus
    forgetRegion(regionId)
    return true
  }
  const page = pages().find((candidate) => candidate.id === region.pageId)
  if (!page) return false
  if (pageStatus) page.status = pageStatus
  if (page.resident) {
    page.regions.push(region)
    recountPage(page)
    syncReviewForPage(page)
  } else {
    syncReviewEntry(page, region)
  }
  return true
}

/**
 * The status of the page a region sits on - the other half of an undoable
 * region edit's snapshot (see `replaceRegion`).
 *
 * @param {string} regionId
 * @returns {string|null} null when no open page holds that region
 */
export function pageStatusOf(regionId) {
  for (const page of pages()) {
    if (page.regions.some((candidate) => candidate.id === regionId)) return page.status
  }
  return null
}

/* ------------------------------------------------------------------ */
/* The resident page window                                             */
/* ------------------------------------------------------------------ */

/**
 * The page indices whose regions should be in RAM right now: previous,
 * current, next, and - in a longstrip chapter - whatever the column has on
 * screen, because those are the pages the Layers panel is listing, and
 * whatever it has mounted with a neighbour either side, because a sheet draws
 * its patches from its own regions and from the ones reaching across its
 * joins.
 *
 * @returns {number[]}
 */
export function residentIndices() {
  const mounted = editor.stripMounted
  const reach = mounted.length > 0 ? [Math.min(...mounted) - 1, ...mounted, Math.max(...mounted) + 1] : []
  return windowIndices({
    pageIndex: editor.pageIndex,
    pageCount: pageCount(),
    stripScope: [...editor.stripScope, editor.stripFocus, ...reach],
    longstrip: editor.project?.mode === 'longstrip',
  })
}

/**
 * Put a page the backend has just handed over into the open chapter, regions
 * and all, and bring its share of the review index up to date with it.
 *
 * @param {import('../api/backend.js').ApiPage} page
 */
function applyLoadedPage(page) {
  const list = pages()
  const at = list.findIndex((candidate) => candidate.id === page.id)
  if (at < 0) return
  // The whole page, not just its regions: a page whose last mask was deleted
  // while it sat outside the window comes back with the status it now has, and
  // a header that kept the old one would draw a full track under a stale mark.
  list[at] = page
  syncReviewForPage(page)
}

/**
 * Drop one page's regions. The page itself stays - its header is what the
 * Pages list, the strip geometry and the exporter read.
 *
 * @param {number} index
 */
function evictPage(index) {
  const page = pages()[index]
  if (!page) return
  // A region the user is pointing at must not be highlighted once it is gone;
  // the highlight contract already says both ids belong to a page.
  if (page.regions.some((region) => isHighlighted(region.id))) clearHighlight()
  evictRegions(page)
}

/**
 * Slide the window to wherever the reader now is. Awaited on open - the editor
 * is already showing its loading state there - and fired and forgotten on a
 * page turn, where the panel filling in a frame later is the only visible
 * effect.
 *
 * @returns {Promise<void>}
 */
export async function syncPageWindow() {
  const chapter = editor.chapter
  if (!chapter) return
  await slideWindow({
    chapterId: chapter.id,
    pages: pages(),
    wanted: residentIndices(),
    applyPage: applyLoadedPage,
    evictPage,
  })
}

/** Refresh one resident page after a native edit changes later-layer review state. */
const reloadVersions = new Map()
export async function reloadPage(index) {
  const chapter = editor.chapter
  if (!chapter || !Number.isInteger(index)) return
  const key = `${chapter.id}:${index}`
  const version = (reloadVersions.get(key) ?? 0) + 1
  reloadVersions.set(key, version)
  const loaded = await getBackend().loadPages({ chapterId: chapter.id, indices: [index] })
  if (editor.chapter?.id !== chapter.id || reloadVersions.get(key) !== version) return
  for (const page of loaded ?? []) applyLoadedPage(page)
}

/**
 * Recompute one page's entries in the chapter's review index.
 *
 * Only ever called for a page whose regions are in hand, which is the only kind
 * this can be honest about. Pages outside the window keep the entries they had
 * when they were last loaded - they are not being edited, and nothing but an
 * edit or a run can change them; a run's `page-done` carries the whole page, so
 * that route comes through here too.
 *
 * @param {import('../api/backend.js').ApiPage} page
 */
export function syncReviewForPage(page) {
  const chapter = editor.chapter
  if (!chapter) return
  const fresh = reviewList(page.regions).map((entry) => ({
    id: entry.id,
    pageId: page.id,
    pageIndex: page.index,
    reasonKey: entry.reasonKey,
  }))
  const rest = (chapter.review ?? []).filter((entry) => entry.pageId !== page.id)
  // `sort` is stable, so entries keep their order inside a page and the index
  // stays in page order overall - which is the order the ⌃ ⌄ pill steps.
  chapter.review = [...rest, ...fresh].sort((a, b) => a.pageIndex - b.pageIndex)
  page.reviewCount = fresh.length
}

/**
 * The one-region version of `syncReviewForPage`, for a page whose regions are
 * **not** in hand.
 *
 * A run reports `region-done` for pages all over the chapter and the reader is
 * on three of them; the review set is the one chapter-wide thing in the
 * interface, so it cannot wait for the page to be paged in. The index is keyed
 * by region id, which is what makes this safe on a page with no region list to
 * reconcile against: a region the run has *replaced* upserts its own entry
 * rather than adding a second one, and the page's `reviewCount` is read back
 * off the index rather than incremented.
 *
 * @param {import('../api/backend.js').ApiPage} page
 * @param {import('../api/backend.js').ApiRegion} region
 */
function syncReviewEntry(page, region) {
  const chapter = editor.chapter
  if (!chapter) return
  const reasonKey = reviewReason(region)
  const rest = (chapter.review ?? []).filter((entry) => entry.id !== region.id)
  const next = reasonKey
    ? [...rest, { id: region.id, pageId: page.id, pageIndex: page.index, reasonKey }]
    : rest
  chapter.review = next.sort((a, b) => a.pageIndex - b.pageIndex)
  page.reviewCount = chapter.review.filter((entry) => entry.pageId === page.id).length
}

/* ------------------------------------------------------------------ */
/* Undo / redo - see "Undo / redo" below                               */
/* ------------------------------------------------------------------ */

/** @returns {boolean} */
export function undoAvailable() {
  return canUndo(editor.history)
}

/** @returns {boolean} */
export function redoAvailable() {
  return canRedo(editor.history)
}

/** @returns {string|null} i18n key of the action Undo would reverse */
export function undoLabelKey() {
  return undoLabel(editor.history)
}

/** @returns {string|null} i18n key of the action Redo would repeat */
export function redoLabelKey() {
  return redoLabel(editor.history)
}

/* ------------------------------------------------------------------ */
/* Autosave                                                            */
/* ------------------------------------------------------------------ */

/** @type {ReturnType<typeof setTimeout>|null} */
let autosaveTimer = null

const ZOOM_RANGE = { minZoom: MIN_ZOOM, maxZoom: MAX_ZOOM }

/** Debounced - scroll and zoom change far faster than a write should. */
export function scheduleAutosave() {
  if (autosaveTimer) clearTimeout(autosaveTimer)
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null
    saveAutosave()
  }, AUTOSAVE_DEBOUNCE_MS)
}

/** Write the open chapter's position record immediately. */
export function saveAutosave() {
  const chapter = editor.chapter
  if (!chapter) return
  storeAutosave(chapter.id, {
    pageIndex: editor.pageIndex,
    scrollTop: editor.scroll.top,
    scrollLeft: editor.scroll.left,
    zoom: editor.zoom,
    fit: editor.fit,
    reviewFilter: editor.reviewFilter,
    reviewCurrentId: editor.reviewCurrentId,
  })
}

/** @param {string} chapterId */
function restoreAutosave(chapterId) {
  const record = loadAutosave(chapterId, ZOOM_RANGE)
  const last = Math.max(0, pageCount() - 1)

  editor.pageIndex = Math.min(record.pageIndex, last)
  editor.scroll = { top: record.scrollTop, left: record.scrollLeft }
  editor.zoom = record.zoom
  editor.fit = record.fit
  editor.reviewFilter = record.reviewFilter

  // The review set is recomputed, never restored; what is restored is the
  // user's position in it - and only if that region still needs review.
  const entries = reviewEntries()
  editor.reviewCurrentId = entries.some((entry) => entry.id === record.reviewCurrentId)
    ? record.reviewCurrentId
    : null
}

/* ------------------------------------------------------------------ */
/* Backend channel - the app's single subscription                     */
/* ------------------------------------------------------------------ */

/** @type {(() => void)|null} */
let unsubscribe = null

/**
 * @param {number} index
 * @returns {import('../api/backend.js').ApiPage|null}
 */
function pageAt(index) {
  return pages()[index] ?? null
}

/** @param {import('../api/backend.js').BackendEvent} event */
function onBackendEvent(event) {
  if (event.type === 'notice') {
    // A cloud clean's notice carries a machine code; this says it in words,
    // or leaves it out (`model/cloudnotices.js`).
    const { key, params } = presentNotice(event.key, event.params ?? {})
    notify({ key, params, tone: event.tone })
    return
  }
  // Before the chapter check: a flow waiting on its run (`runFinished`) must
  // hear the end of it even when the reader has opened another chapter since.
  if (event.type === 'run-finished') settleRun(event)
  if (!editor.chapter || event.chapterId !== editor.chapter.id) return

  switch (event.type) {
    case 'page-started': {
      const page = pageAt(event.pageIndex)
      if (page) page.status = 'cleaning'
      editor.run.currentPageIndex = event.pageIndex
      break
    }
    case 'region-done': {
      const page = pageAt(event.pageIndex)
      if (!page) break
      // A run walks the whole chapter while the reader stays where they are, so
      // most of its regions land on pages outside the resident window. Those
      // are counted, not held: the page is reloaded whole the moment it enters
      // the window, and pushing a region into a header would leave a page that
      // claims to hold one region out of twenty.
      if (!page.resident) {
        // The row still has to move while the run is on it: the counts are the
        // header's, so they are incremented here rather than derived from a
        // list this page does not have. `regionCount` and `doneCount` are the
        // ones a missing "is this region new" boolean would sharpen - a
        // replaced region over-counts both until `page-done` arrives with the
        // whole page and sets them outright.
        page.regionCount = (page.regionCount ?? 0) + 1
        const flagged = reviewReason(event.region) !== null
        // A detection carries its fitted mask and is not finished: it counts
        // as found, never as done.
        if (!flagged && event.region.mask && event.region.outcome !== 'detected') page.doneCount = (page.doneCount ?? 0) + 1
        // The review count is not incremented - it is read back off the index,
        // which is keyed by region id and so cannot double-count a region the
        // run has replaced.
        syncReviewEntry(page, event.region)
        break
      }
      const index = page.regions.findIndex((region) => region.id === event.region.id)
      if (index >= 0) page.regions[index] = event.region
      else page.regions.push(event.region)
      recountPage(page)
      syncReviewForPage(page)
      break
    }
    case 'page-done': {
      const index = pages().findIndex((page) => page.id === event.page.id)
      if (index >= 0) {
        // The event carries a whole page, regions and all. It is kept only if
        // the page is inside the window; outside it, the header is taken and
        // the regions are dropped on the floor rather than reinstated one page
        // at a time behind the reader's back.
        // The review index is refreshed from the event first, while the
        // regions are still there to read: it is the one chapter-wide thing,
        // and a run is exactly what changes it.
        syncReviewForPage(event.page)
        const wanted = residentIndices().includes(index)
        editor.chapter.pages[index] = wanted ? event.page : evictRegions(event.page)
      }
      editor.run.pagesDone += 1
      if (editor.run.currentPageIndex === event.page.index) editor.run.currentPageIndex = null
      break
    }
    case 'run-finished': {
      if (editor.run.runId !== event.runId) break
      const unfinishedPage = editor.run.currentPageIndex
      editor.run.active = false
      editor.run.runId = null
      editor.run.currentPageIndex = null
      editor.run.nextPageIndex = event.nextPageIndex
      if (unfinishedPage !== null) void reloadPage(unfinishedPage).catch(() => {})
      scheduleAutosave()
      break
    }
  }
}

function subscribeOnce() {
  if (unsubscribe) return
  unsubscribe = getBackend().subscribe(onBackendEvent)
}

/* ------------------------------------------------------------------ */
/* Lifecycle                                                           */
/* ------------------------------------------------------------------ */

/**
 * Open a chapter into the editor. Called by the editor screen when it mounts
 * and whenever the route's chapter changes.
 *
 * `convert` is the answer to the format-conversion dialog this function itself
 * raises: the chapter is already open by then, so the same call has to be able
 * to run again on a chapter it would otherwise skip. Converting rewrites the
 * chapter's files, so the re-open is a real one - the session state goes with
 * them, and the page window and the undo journal's index are read again from
 * scratch.
 *
 * `reload` reads the open chapter again rather than answering that it is
 * open already.
 *
 * @param {string} projectId
 * @param {string} chapterId
 * @param {{convert?: boolean, reload?: boolean}} [options]
 * @returns {Promise<boolean>} whether the chapter opened
 */
export async function openEditorChapter(projectId, chapterId, options = {}) {
  const convert = options.convert === true
  const reload = options.reload === true
  if (!convert && !reload && editor.chapter?.id === chapterId && editor.project?.id === projectId) return true
  subscribeOnce()
  editor.loading = true
  let result
  try {
    result = await getBackend().openChapter({ projectId, chapterId, convert })
  } catch (error) {
    editor.loading = false
    // Another process holding the library for longer than a command waits.
    if (reportJobConflict(error)) return false
    throw error
  }
  if (!result) {
    editor.loading = false
    return false
  }

  editor.project = result.project
  editor.chapter = result.chapter
  resetSessionState()
  restoreAutosave(chapterId)
  // The chapter arrived as page headers and a review index; the
  // window is filled here, while the screen is still showing its loading state,
  // so the Layers panel is never momentarily empty for a page full of masks.
  // The journal comes back at the same time - undo survives a restart, and this
  // is where it is picked up again.
  // A run this chapter already has - started before the editor was left, or
  // before the window reloaded - is picked up here, so the Pages list and the
  // tool's Cancel follow it again (`adoptChapterJob`).
  await Promise.all([syncPageWindow(), loadHistory(chapterId), adoptChapterJob()])
  editor.loading = false

  if (result.pendingConversion) {
    pushModal({
      kind: 'formatConversion',
      titleKey: 'modal.title.formatConversion',
      blocking: true,
      props: { ...result.pendingConversion, projectId, chapterId },
      actions: [
        { id: 'cancel', labelKey: 'shell.action.cancel' },
        { id: 'convert', labelKey: 'shell.action.convert', variant: 'primary' },
      ],
    })
  }
  return true
}

/** Everything that is about *this* opening of *this* chapter. */
function resetSessionState() {
  editor.tool = 'autoClean'
  editor.selectionId = null
  editor.hoverId = null
  editor.originalVisible = false
  editor.originalPinned = false
  editor.wipe = 100
  editor.maskOverlay = false
  editor.stripScope = []
  editor.stripFocus = -1
  editor.stripMounted = []
  editor.stripScrollRequest = null
  // A gesture in progress, and the clone source it may have sampled, belong to
  // the page they were drawn on.
  resetDraftState()
  editor.run = {
    active: false,
    runId: null,
    scope: null,
    mode: 'auto',
    queued: 0,
    pagesDone: 0,
    currentPageIndex: null,
    nextPageIndex: null,
  }
  editor.history = createHistory()
}

/**
 * Leave the editor: flush autosave and drop the subscription.
 *
 * What goes is the *index* - the labels and the cursor. The journal itself is a
 * file in the job folder and stays exactly where it is, which is
 * what makes undo survive the chapter being closed and the application being
 * restarted. The resident regions go with the chapter for the same reason they
 * were ever evicted: they are a cache of the manifest, never the only copy.
 */
export function closeEditorChapter() {
  // A resume that was asked for and never consumed dies with the screen it was
  // meant for; it must not fire the next time that chapter is opened by hand.
  pendingResume = null
  pendingRunStart = null
  // The editor owns the event subscription. A flow waiting for its first run
  // cannot hear the native end after this subscription is removed. The run
  // itself goes on: the jobs list follows it (`state/jobs.svelte.js`), and
  // reopening the chapter adopts it again (`adoptChapterJob`).
  const chapterId = editor.chapter?.id
  for (const runId of new Set([editor.run.runId, ...runWaiters.keys()].filter(Boolean))) {
    settleRun({ type: 'run-finished', runId, chapterId, reason: 'editor-closed' })
  }
  if (autosaveTimer) {
    clearTimeout(autosaveTimer)
    autosaveTimer = null
  }
  saveAutosave()
  unsubscribe?.()
  unsubscribe = null
  editor.project = null
  editor.chapter = null
  resetSessionState()
}

/* ------------------------------------------------------------------ */
/* Paging                                                              */
/* ------------------------------------------------------------------ */

/*
 * The strip handshake - `goToPage` and the longstrip canvas
 * ---------------------------------------------------------
 * Two different things want to change "which page am I on", and they must not
 * be the same call:
 *
 *   `goToPage(index)`        - *put me on this page.* The Pages list, the
 *     bottom pill's ‹ ›, `stepReview` and a resumed run all call it. In a
 *     longstrip chapter it cannot know where position 4 sits in the column, so
 *     it leaves a **scroll request** the canvas consumes and clears.
 *   `setStripPosition(index)` - *this is the page I am looking at now.* Only
 *     the strip calls it, from its own scrolling, and it deliberately does not
 *     touch scroll: routing the viewport-centre readout through `goToPage`
 *     would reset the scroller to {0,0} and fight the user for the scrollbar.
 *
 * The request is a `{index, token}` pair rather than a bare index so that
 * asking twice for the same position still scrolls. The canvas is the only
 * consumer; nothing else may read or clear it.
 */

/** @param {number} index */
export function goToPage(index) {
  const last = Math.max(0, pageCount() - 1)
  const next = Math.min(last, Math.max(0, Math.trunc(index)))
  const longstrip = editor.project?.mode === 'longstrip'
  if (longstrip) requestStripScroll(next)
  if (next === editor.pageIndex) return
  editor.pageIndex = next
  // The strip is one continuous scroll: resetting it to the top here would
  // undo the scroll the request above is about to make.
  if (!longstrip) editor.scroll = { top: 0, left: 0 }
  clearHighlight()
  // The window follows the reader. Not awaited: a page turn is
  // synchronous and the regions filling in a frame later is the whole of the
  // visible difference.
  void syncPageWindow()
  scheduleAutosave()
}

/**
 * The strip reporting its own position - the page at the centre of the
 * viewport. No scroll reset, no selection change: the user is scrolling, not
 * navigating.
 *
 * @param {number} index
 * @param {number[]} [indices] visible pages, updated with the centre before loading
 * @param {number} [focus] a focused sheet kept resident outside the visible band
 * @param {number[]} [mounted] the positions the column has mounted (`editor.stripMounted`)
 */
export function setStripPosition(index, indices = editor.stripScope, focus = editor.stripFocus, mounted = editor.stripMounted) {
  const last = Math.max(0, pageCount() - 1)
  const next = Math.min(last, Math.max(0, Math.trunc(index)))
  const moved = next !== editor.pageIndex
  const scopeChanged = updateStripScope(indices)
  const focusChanged = focus !== editor.stripFocus
  const mountedChanged = updateStripMounted(mounted)
  if (!moved && !scopeChanged && !focusChanged && !mountedChanged) return
  editor.pageIndex = next
  editor.stripFocus = focus
  void syncPageWindow()
  if (moved) scheduleAutosave()
}

/**
 * The positions the strip has on screen, in strip order. Feeds
 * `scopePageIndices()` and nothing else.
 *
 * @param {number[]} indices
 */
export function setStripScope(indices) {
  if (updateStripScope(indices)) void syncPageWindow()
}

/** @param {number[]} indices */
function updateStripMounted(indices) {
  const next = Array.isArray(indices) ? indices : []
  const same =
    next.length === editor.stripMounted.length &&
    next.every((value, i) => value === editor.stripMounted[i])
  if (!same) editor.stripMounted = next
  return !same
}

/** @param {number[]} indices */
function updateStripScope(indices) {
  const next = Array.isArray(indices) ? indices : []
  const same =
    next.length === editor.stripScope.length &&
    next.every((value, i) => value === editor.stripScope[i])
  if (!same) {
    editor.stripScope = next
  }
  return !same
}

let stripScrollToken = 0

/** @param {number} index */
export function requestStripScroll(index) {
  stripScrollToken += 1
  editor.stripScrollRequest = { index: Math.max(0, Math.trunc(index)), token: stripScrollToken }
}

/**
 * Take the pending scroll request, if there is one. Consuming clears it, so a
 * request is acted on exactly once.
 *
 * @returns {number|null} the position to scroll to
 */
export function consumeStripScroll() {
  const request = editor.stripScrollRequest
  if (!request) return null
  editor.stripScrollRequest = null
  return request.index
}

/** @param {'next'|'prev'} action */
export function stepPage(action) {
  goToPage(editor.pageIndex + (action === 'next' ? 1 : -1))
}

/**
 * What the fixed-position ‹ / › controls and the ← / → keys do, which depends
 * on the project's reading direction.
 *
 * @param {'left'|'right'} side
 */
export function pageByArrow(side) {
  stepPage(pageNavControls(readingDirection())[side].action)
}

/**
 * @param {number} top from the page's place at rest, `editor.scrollRoom` taken off
 * @param {number} left
 */
export function setScroll(top, left) {
  editor.scroll = { top, left }
  scheduleAutosave()
}

/**
 * The canvas reporting the space it keeps above the page, so that the shell
 * can turn a saved position into a `scrollTop` and back.
 *
 * @param {number} room CSS pixels
 */
export function reportScrollRoom(room) {
  const next = Math.max(0, Math.round(Number(room) || 0))
  if (next !== editor.scrollRoom) editor.scrollRoom = next
}

/* ------------------------------------------------------------------ */
/* Zoom                                                                */
/* ------------------------------------------------------------------ */

/**
 * How big the page is drawn right now - the chosen zoom, or the fit scale
 * while `fit` is on. The one number a readout, a step button or a pinch should
 * ever ask for; `editor.zoom` alone answers a different question and is a lie
 * under fit.
 *
 * @returns {number}
 */
export function displayZoom() {
  return editor.fit ? editor.fitScale : editor.zoom
}

/**
 * Rounded to two places: `1 + 0.15 + 0.15` is otherwise `1.2999999999999998`,
 * which then accumulates through the autosave record and back.
 *
 * @param {number} zoom
 */
export function setZoom(zoom) {
  const clamped = numberIn(zoom, { min: MIN_ZOOM, max: MAX_ZOOM, fallback: editor.zoom })
  editor.zoom = Math.round(clamped * 100) / 100
  editor.fit = false
  scheduleAutosave()
}

// Stepping starts from what is on screen, not from the last chosen zoom: `+`
// pressed at fit must grow the fitted page, not jump back to wherever the user
// was before they fitted it.
export function zoomIn() {
  setZoom(displayZoom() + ZOOM_STEP)
}

export function zoomOut() {
  setZoom(displayZoom() - ZOOM_STEP)
}

/** 1:1 - the image at its own pixels. */
export function zoomActual() {
  setZoom(1)
}

export function zoomFit() {
  editor.fit = true
  scheduleAutosave()
}

/**
 * The canvas reporting the scale "fit" actually works out to, so that
 * `displayZoom()` is the one true answer to "how big is the page right now"
 * and the readout does not need a source of its own (Task 9).
 *
 * It lands in `editor.fitScale`, **not** in `editor.zoom`: it is deliberately
 * not clamped to `MIN_ZOOM` - a 1600px page fitted into a 1232px viewport is
 * genuinely 32% - and `editor.zoom` promises that range. Deliberately not
 * autosaved either: it is recomputed from the viewport on every open, so
 * writing it on every resize would be a write storm about nothing.
 *
 * @param {number} scale
 */
export function reportFitScale(scale) {
  if (!Number.isFinite(scale) || scale <= 0) return
  const next = Math.round(scale * 100) / 100
  if (next !== editor.fitScale) editor.fitScale = next
}

/* ------------------------------------------------------------------ */
/* Tools, selection, overlays                                          */
/* ------------------------------------------------------------------ */

/**
 * Choose a tool. Selecting one also opens and raises the tool window - a tool
 * whose parameters are hidden behind a second action is a tool the user has to
 * pick twice. Both routes in (the rail and the `1` to `6` keys) come through
 * here, so neither has to remember.
 *
 * @param {string} tool
 */
export function setTool(tool) {
  if (tool === 'contentAwareFill') tool = 'aiMaskBrush'
  if (!TOOLS.includes(/** @type {any} */ (tool))) return
  editor.tool = tool
  openWindow('tool')
}

/** @param {number} slot - 1..6, as the number-key shortcuts number them */
export function setToolBySlot(slot) {
  const tool = TOOLS[slot - 1]
  if (tool) setTool(tool)
}

/**
 * @param {string} tool
 * @param {string} key
 * @param {unknown} value
 */
export function setToolParam(tool, key, value) {
  if (!editor.toolParams[tool]) editor.toolParams[tool] = {}
  editor.toolParams[tool][key] = value
}

/**
 * The selection tool's keys, `S` and `X`. Pressed while another tool is
 * active, either one arms the selection tool as it was left and changes
 * nothing else: a key that switched tool *and* changed a setting the user
 * cannot see yet would land them in a state they did not choose. Pressed with
 * the selection tool already active, `S` steps to the next shape in the tool
 * bar's order and `X` swaps add for remove, the way a mask painter swaps its
 * two colours.
 */
export function cycleMaskSelectShape() {
  if (editor.tool !== 'maskSelect') return setTool('maskSelect')
  const shapes = toolSpec('maskSelect').params.find((param) => param.key === 'shape')
  const values = shapes?.kind === 'choice' ? shapes.options.map((option) => option.value) : []
  if (values.length === 0) return
  const at = values.indexOf(String(editor.toolParams.maskSelect?.shape ?? values[0]))
  setToolParam('maskSelect', 'shape', values[(at + 1) % values.length])
}

/** @see cycleMaskSelectShape */
export function toggleMaskSelectMode() {
  if (editor.tool !== 'maskSelect') return setTool('maskSelect')
  setToolParam('maskSelect', 'mode', editor.toolParams.maskSelect?.mode === 'remove' ? 'add' : 'remove')
}

/*
 * Highlight contract - the Layers panel and the canvas (Task 9)
 * -------------------------------------------------------------
 * Two region ids, and neither surface talks to the other:
 *
 *   `editor.selectionId` - the region the user is working on. Sticky until
 *     something else is selected, the page changes, or `Esc`.
 *   `editor.hoverId`     - the region under the pointer *or* under keyboard
 *     focus. Transient; whoever sets it clears it.
 *
 * Writers: the Layers panel calls `hover(id)` on `pointerenter` and on
 * `focusin` of a row and `hover(null)` on `pointerleave` / `focusout`, and
 * `select(id)` when a row is activated. The canvas does the same for a region
 * on the page. `stepReview` writes `selectionId` (and `reviewCurrentId`) but
 * not `hoverId`, which is why the panel's row and the canvas's marker both
 * light up from `isHighlighted` rather than from hover alone.
 *
 * Readers: both. A region is highlighted when its id is either one, so hover
 * in the panel lights the canvas and hover on the canvas lights the panel -
 * one rule, no cross-component reference.
 *
 * **Both ids belong to a page.** Leaving that page must drop them, or the
 * canvas draws a highlight for a region the user can no longer see -
 * `clearHighlight` is what `goToPage` calls, and it is the reason `hoverId` is
 * cleared there and not only `selectionId`.
 */

/** @param {string|null} regionId */
export function select(regionId) {
  editor.selectionId = regionId
}

/** @param {string|null} regionId */
export function hover(regionId) {
  editor.hoverId = regionId
}

/**
 * The region `editor.selectionId` names, or null - the selection as an object
 * rather than as an id.
 *
 * The editor-wide `⌘⌫` needs it: the Layers row's Delete has the region in
 * hand because it renders one, and a shortcut pressed with the canvas focused
 * has only the id. Searching the resident pages is the whole of it; a
 * selection is dropped the moment its page leaves (`clearHighlight`,
 * `forgetRegion`), so an id that matches nothing is an id that stands for
 * nothing.
 *
 * @returns {import('../api/backend.js').ApiRegion|null}
 */
export function selectedRegion() {
  const id = editor.selectionId
  if (!id) return null
  for (const page of pages()) {
    const found = page.regions.find((candidate) => candidate.id === id)
    if (found) return found
  }
  return null
}

/** Drop both region ids - the page they point into is no longer on screen. */
export function clearHighlight() {
  editor.selectionId = null
  editor.hoverId = null
}

/**
 * Whether a region should be drawn highlighted - the panel row and the canvas
 * marker ask the same question of the same state.
 *
 * @param {string} regionId
 * @returns {boolean}
 */
export function isHighlighted(regionId) {
  return editor.hoverId === regionId || editor.selectionId === regionId
}

export function toggleMaskOverlay() {
  editor.maskOverlay = !editor.maskOverlay
}

/**
 * The held `O`. Two things override the hold:
 *
 * - `session.originalView === 'pinned'` - the user has asked for the sticky
 *   mode outright, so the press toggles and the release is ignored.
 *   A sticky equivalent is required for every held-key
 *   interaction, and this is the setting that makes it the default.
 * - an original already pinned with `⇧O` stays up when the key is released.
 *
 * @param {boolean} held
 */
export function holdOriginal(held) {
  if (session.originalView === 'pinned') {
    if (held) togglePinOriginal()
    return
  }
  if (editor.originalPinned) return
  editor.originalVisible = held
}

export function togglePinOriginal() {
  editor.originalPinned = !editor.originalPinned
  editor.originalVisible = editor.originalPinned
}

/**
 * The wipe between the original (0) and the cleaned page (100). Session-only:
 * it is a way of looking at the page right now, not a position in the chapter,
 * so it is not part of the autosave record.
 *
 * @param {number} value
 */
export function setWipe(value) {
  editor.wipe = numberIn(Math.round(value), { min: 0, max: 100, fallback: editor.wipe })
}

/* ------------------------------------------------------------------ */
/* Review                                                              */
/* ------------------------------------------------------------------ */

export function toggleReviewFilter() {
  editor.reviewFilter = !editor.reviewFilter
  scheduleAutosave()
}

/**
 * Move to the next / previous region needing review, wrapping, and bring its
 * page into view.
 *
 * @param {'next'|'prev'} direction
 * @returns {import('../model/review.js').ReviewEntry|null}
 */
export function stepReview(direction) {
  const entries = reviewEntries()
  const entry = stepIssue(entries, editor.reviewCurrentId, direction)
  if (!entry) return null
  // The index comes off the review entry itself: the set is chapter-wide and
  // the page it names may not be one the interface is holding.
  const index = entry.pageIndex ?? pages().findIndex((page) => page.id === entry.pageId)
  if (index >= 0) goToPage(index)
  editor.reviewCurrentId = entry.id
  editor.selectionId = entry.id
  scheduleAutosave()
  return entry
}

/* ------------------------------------------------------------------ */
/* Runs                                                                */
/* ------------------------------------------------------------------ */

let pendingRunStart = null

/** Claim the interval before a run handle exists, including any consent. */
export function claimRunStart(token = null) {
  if (token && pendingRunStart === token && !editor.run.active) return token
  if (pendingRunStart || editor.run.active) {
    notify({ key: 'notice.run.busy', tone: 'warn' })
    return null
  }
  pendingRunStart = {}
  return pendingRunStart
}

/** Release only the caller's claim, so an older exit cannot unlock a newer flow. */
export function releaseRunStart(token) {
  if (pendingRunStart === token) pendingRunStart = null
}

/**
 * Start an auto clean. `runClean` resolves with the queue; the result arrives
 * on the event channel. Cloud detection carries its single-use grant from the
 * consent in `editor/cloudrun.js`, and `mode` names the tool's step.
 *
 * `area` holds a page Detect to one area of `pageIndex` (page percent): what
 * it finds there is added to the page's detections and nothing is replaced.
 *
 * @param {'page'|'chapter'|'project'} [scope]
 * @param {{cloudGrant?: string|null, mode?: import('../api/backend.js').RunMode, pendingToken?: object|null, area?: {x: number, y: number, w: number, h: number}|null, pageIndex?: number}} [options]
 * @returns {Promise<string|null>} the run id, or null when nothing was queued
 */
export async function startRun(scope = 'page', { cloudGrant = null, mode = 'auto', pendingToken = null, area = null, pageIndex = editor.pageIndex } = {}) {
  if (!editor.chapter) return null
  const token = claimRunStart(pendingToken)
  if (!token) return null
  try {
    const chapter = editor.chapter
    const params = editor.toolParams.autoClean ?? {}
    // Detect on its own finds all text, outside bubbles too, for the review to
    // keep or drop: which text is cleaned is Detect & clean's choice. The
    // detection half of a Detect & clean whose clean runs on the cloud is a
    // Detect run too, and keeps the panel's choices.
    // So does a Detect of one area: the user pointed at the text.
    const findAll = mode === 'detect' && (params.step === 'detect' || !!area)
    // The models and reader this run uses: the user's own on this computer,
    // the fixed best combination on the cloud GPU (`pipelines.js#runDetection`).
    const run = runDetection(session)
    const handle = await getBackend().runClean({
      scope,
      mode,
      chapterId: chapter.id,
      pageIndex,
      engineCeiling: LOCAL_CEILING,
      // The tool window's two rows, on the wire. The backend applies them per
      // region by whether the region is inside a speech balloon, and treats an
      // absent or unrecognised value as its own default rather than as "no
      // preference" - the seam's names are the ladder's own rung names
      // (`fill`, `lama`).
      bubbleEngine: String(params.bubbleEngine ?? 'fill'),
      outsideEngine: String(params.outsideEngine ?? 'lama'),
      // The opt-in for text outside bubbles. Anything but `clean`
      // is the default on the other side of the seam too, so an old stored
      // record with no such key reviews that text as it always did.
      outsideBubbles: findAll ? 'clean' : String(params.outsideBubbles ?? 'review'),
      bubbleColor: String(params.bubbleColor ?? '#ffffff'),
      maskPaddingPx: paddingOf(params),
      detection: { ...session.detection },
      detectorModels: run.detectorModels,
      geometryPolicy: 'legacy',
      textPolicy: findAll ? 'all_text' : session.textPolicy,
      ocrRescue: run.ocrRescue,
      analysisTargets: unifyAnalysisTargets(session.analysisTargets),
      ...(cloudGrant ? { cloudGrant } : {}),
      ...(area ? { area } : {}),
    })
    if (editor.chapter !== chapter) return null
    return adoptRun(handle, scope, mode)
  } finally {
    if (!pendingToken) releaseRunStart(token)
  }
}

/**
 * The Mask padding on the wire: a whole number of page pixels the native side
 * takes, whatever the record holds.
 *
 * @param {Record<string, unknown>} params
 */
function paddingOf(params) {
  return Math.round(numberIn(Number(params.maskPadding ?? 0), { min: 0, max: MAX_MASK_PADDING, fallback: 0 }))
}

/**
 * Re-pad the masks already detected, over the page or the whole chapter: the
 * Text cleanup panel's Apply beside Mask padding. Each one is grown from its
 * unpadded mask (`region.rs#set_detection_padding`), so 0 gives those back.
 * Masks a padding runs together become one, and stay one at a smaller padding.
 * The resident pages it changed are read again, which moves each changed
 * detection's mask URL; a page outside the window is read when it comes in.
 *
 * @param {'page'|'chapter'} scope
 * @returns {Promise<boolean>} whether any mask changed
 */
export async function applyDetectionPadding(scope) {
  const chapter = editor.chapter
  if (!chapter || editor.run.active) return false
  const paddingPx = paddingOf(editor.toolParams.autoClean ?? {})
  let result
  try {
    result = await getBackend().setDetectionPadding({
      chapterId: chapter.id,
      ...(scope === 'page' ? { pageIndex: editor.pageIndex } : {}),
      paddingPx,
    })
  } catch (error) {
    console.error('the mask padding was refused', error)
    if (!reportJobConflict(error)) notify({ key: 'notice.mask.paddingFailed', tone: 'warn' })
    return false
  }
  if (!result || editor.chapter !== chapter) return false
  if (result.changed.length === 0) {
    notify({ key: 'notice.mask.paddingNothing' })
    return false
  }
  notify({ key: 'notice.mask.paddingApplied', params: { px: paddingPx, count: result.changed.length } })
  if (result.removed?.length) notify({ key: 'notice.mask.paddingMerged', params: { count: result.removed.length } })
  const resident = new Set(residentIndices())
  await Promise.all(result.pages.filter((index) => resident.has(index)).map((index) =>
    reloadPage(index).catch((error) => console.error('the page could not be read again after a padding change', error))))
  return true
}

/**
 * Take over a queue the adapter has just started. Every route that can start a
 * run comes through here - `startRun`, and the cloud clean's render run
 * (`src/lib/editor/cloudrun.js`). A click on a region starts none: Text
 * cleanup is not a per-region tool (`toolapply.svelte.js`).
 *
 * @param {{runId: string|null, pages?: Array<Object>, alreadyRunning?: boolean, atCapacity?: boolean}} handle
 * @param {'page'|'chapter'|'project'} scope
 * @param {import('../api/backend.js').RunMode} [mode] - the step it takes
 * @param {import('./jobs.svelte.js').JobKind|null} [kind] - the job it is on the jobs list; the step's own kind when null
 * @returns {string|null} the run id, or null when nothing was queued
 */
export function adoptRun(handle, scope, mode = 'auto', kind = null) {
  // The backend runs a few chapters at once and says when that many are going.
  if (handle?.atCapacity) {
    notify({ key: 'notice.run.atCapacity', tone: 'warn' })
    return null
  }
  if (handle?.alreadyRunning) {
    notify({ key: 'notice.run.busy', tone: 'warn' })
    // The run it names is this chapter's (one run per chapter). If the jobs
    // list knows it, the editor follows it instead of showing nothing.
    const job = jobById(handle.runId)
    if (job?.status === 'running' && job.chapterId === editor.chapter?.id && !editor.run.active) void adoptChapterJob()
    return null
  }
  if (!handle?.runId) return null
  editor.run = {
    active: true,
    runId: handle.runId,
    scope,
    mode,
    queued: handle.pages?.length ?? 0,
    pagesDone: 0,
    currentPageIndex: null,
    nextPageIndex: null,
  }
  registerEditorJob(handle.runId, kind ?? kindOfMode(mode), editor.run.queued)
  return handle.runId
}

/** The job kind a run of this step is. @param {import('../api/backend.js').RunMode} mode */
function kindOfMode(mode) {
  return mode === 'detect' ? 'detect' : 'clean'
}

/**
 * Put the open chapter's run on the jobs list, named, so it can be followed
 * from Home once the editor is left.
 *
 * @param {string} runId
 * @param {import('./jobs.svelte.js').JobKind} kind
 * @param {number} total
 */
function registerEditorJob(runId, kind, total) {
  if (!editor.chapter) return
  registerJob({
    runId,
    kind,
    chapterId: editor.chapter.id,
    projectId: editor.project?.id ?? null,
    projectName: editor.project?.name ?? null,
    chapterName: editor.chapter.name ?? null,
    chapterNumber: editor.chapter.number ?? null,
    total,
  })
}

/**
 * Take over the run the open chapter already has, as the backend lists it
 * (`listJobs`): the run keeps going when the editor is left, and this is what
 * the editor finds when it comes back. Only a listed run is adopted, so a job
 * the store still shows but the backend has finished is never shown running.
 *
 * @returns {Promise<string|null>} the adopted run's id
 */
export async function adoptChapterJob() {
  const chapter = editor.chapter
  if (!chapter || editor.run.active) return null
  const listed = await refreshJobs()
  if (editor.chapter !== chapter || editor.run.active) return null
  const entry = listed.find((job) => job?.chapterId === chapter.id && RUN_KINDS.includes(job.kind))
  if (!entry) return null
  const job = jobById(entry.runId)
  if (job?.status !== 'running') return null
  editor.run = {
    active: true,
    runId: job.runId,
    scope: 'chapter',
    mode: job.kind === 'detect' ? 'detect' : 'clean',
    queued: job.total,
    pagesDone: Math.min(job.done, job.total || job.done),
    currentPageIndex: null,
    nextPageIndex: null,
  }
  registerEditorJob(job.runId, job.kind, job.total)
  return job.runId
}

/**
 * The runs somebody is waiting on, by id, and the ends that arrived before
 * anyone asked. The second map is kept short: it exists for the moment
 * between `runClean` answering and the caller asking, not as a history.
 *
 * @type {Map<string, Array<(event: import('../api/backend.js').RunFinishedEvent) => void>>}
 */
const runWaiters = new Map()
/** @type {Map<string, import('../api/backend.js').RunFinishedEvent>} */
const finishedRuns = new Map()

/** @param {import('../api/backend.js').RunFinishedEvent} event */
function settleRun(event) {
  const waiting = runWaiters.get(event.runId)
  runWaiters.delete(event.runId)
  if (waiting) for (const resolve of waiting) resolve(event)
  else {
    finishedRuns.set(event.runId, event)
    if (finishedRuns.size > 8) finishedRuns.delete(finishedRuns.keys().next().value)
  }
}

/**
 * The end of one run, as its `run-finished` event says it ended - completed or
 * cancelled. What a flow that has a second half waits on: a cloud clean
 * detects here, then prepares its batch once the detection run has ended.
 *
 * @param {string} runId
 * @returns {Promise<import('../api/backend.js').RunFinishedEvent>}
 */
export function runFinished(runId) {
  const done = finishedRuns.get(runId)
  if (done) {
    finishedRuns.delete(runId)
    return Promise.resolve(done)
  }
  return new Promise((resolve) => {
    const waiting = runWaiters.get(runId) ?? []
    waiting.push(resolve)
    runWaiters.set(runId, waiting)
  })
}

/** @returns {Promise<string|null>} the cancelled run's id */
export async function cancelRun() {
  if (!editor.run.runId) return null
  return getBackend().cancelRun({ runId: editor.run.runId })
}

/* ------------------------------------------------------------------ */
/* Resume - routed first, started second                               */
/* ------------------------------------------------------------------ */

/**
 * A resume asked for from Home, waiting for the editor to be ready.
 * @type {{projectId: string, chapterId: string}|null}
 */
let pendingResume = null

/**
 * Ask for an interrupted job to be resumed *after* the editor has opened the
 * chapter. Home calls this and then routes to the editor; the editor screen
 * calls `consumeResume()` once `openEditorChapter` has resolved.
 *
 * The order matters: `resumeJob` starts a run, and the run's events are only
 * useful to a subscriber that already holds the chapter they describe. Starting
 * the run first works with the mock's timings and is a race with any backend
 * whose latencies differ.
 *
 * @param {string} projectId
 * @param {string} chapterId
 */
export function requestResume(projectId, chapterId) {
  pendingResume = { projectId, chapterId }
}

/**
 * Start the run a pending resume asked for. No-op when nothing is pending or
 * when the open chapter is not the one the resume was requested for.
 *
 * @returns {Promise<string|null>} the run id
 */
export async function consumeResume() {
  const request = pendingResume
  pendingResume = null
  if (!request || editor.chapter?.id !== request.chapterId) return null
  if (editor.run.active) return null

  const result = await getBackend().resumeJob({
    projectId: request.projectId,
    chapterId: request.chapterId,
  })
  if (!result?.runId) return null

  // The chapter snapshot in `result` is deliberately ignored: the one already
  // loaded predates the run, and every change the run makes arrives on the
  // event channel. Adopting the snapshot would race those events.
  editor.run = {
    active: true,
    runId: result.runId,
    scope: 'chapter',
    // The step the interrupted run was taking, which the native side resumes;
    // an older adapter that does not say is the one-pass run.
    mode: result.mode === 'detect' || result.mode === 'clean' ? result.mode : 'auto',
    queued: result.pages.length,
    pagesDone: 0,
    currentPageIndex: null,
    nextPageIndex: null,
  }
  registerEditorJob(result.runId, kindOfMode(editor.run.mode), editor.run.queued)
  goToPage(result.resumedFrom)
  return result.runId
}

/* ------------------------------------------------------------------ */
/* Rename                                                              */
/* ------------------------------------------------------------------ */

/**
 * Commit an edit of the project name from the editor's top bar. The rename
 * goes through the adapter, not through local state, so the library and the
 * open project can never disagree about the name.
 *
 * @param {string} name
 * @returns {Promise<import('../api/backend.js').ApiProject|null>}
 */
export async function renameOpenProject(name) {
  const project = editor.project
  if (!project) return null
  const next = String(name).trim()
  if (!next || next === project.name) return null
  const updated = await getBackend().renameProject({ projectId: project.id, name: next })
  if (updated) editor.project = updated
  return updated
}

/* ------------------------------------------------------------------ */
/* Undo / redo - global across every tool, persisted                    */
/* ------------------------------------------------------------------ */

/*
 * A command used to be a pair of closures. It is now a **delta**: an op, a
 * region id, and the two states the region can be in. Three things follow, and
 * the third is the one that matters here.
 *
 * 1. It can be written down, so undo survives a restart.
 * 2. It holds nothing, so an evicted page is actually evicted. A closure over
 *    a region on page 140 kept page 140's regions alive for the whole session,
 *    which made the resident window give back nothing at all.
 * 3. Both directions are **one function of the delta**, so redo cannot drift
 *    from undo - the drift the old `recordEdit` avoided by convention is now
 *    avoided by construction.
 *
 * The interface holds only `{seq, label}` per entry; the payload is fetched by
 * `historyMove` at the moment it is replayed.
 */

/**
 * @typedef {Object} RegionState
 * @property {import('../api/backend.js').ApiRegion|null} region
 * @property {string|null} [pageStatus]
 */

/**
 * One side of a delta, out of the snapshot a call site already had.
 *
 * `region` is metadata - a bbox, an engine, a provenance record and a **mask
 * reference** - and never pixels: the mask and the patch it names are already
 * on disk in the job's sidecar, which is where a soft-deleted mask waits to be
 * turned back on.
 *
 * @param {RegionState} state
 * @returns {import('../model/journal.js').DeltaSide}
 */
function sideOf(state) {
  const region = state?.region ? /** @type {any} */ ($state.snapshot(state.region)) : null
  const revisionId = state?.region?.mask?.legacyPatchRevision
  if (region?.mask && revisionId) region.mask.legacyPatchRevision = revisionId
  return {
    present: !!state?.region,
    pageStatus: state?.pageStatus ?? null,
    region,
  }
}

/**
 * Apply one side of a delta: put the region - and its page's status - into the
 * state the side describes, on the backend and then in the open chapter.
 *
 * The one applier for every op there is, which is what makes undo and redo the
 * same code path in opposite directions.
 *
 * `chapterId` is the chapter the delta belongs to, read before anything was
 * awaited. The backend applies it to that chapter's files whatever happens;
 * the interface's copy is only touched while that chapter is still the open
 * one, so an answer that arrives after a switch changes nothing on screen.
 *
 * @param {string} regionId
 * @param {import('../model/journal.js').DeltaSide} side
 * @param {string|null} [chapterId]
 * @returns {Promise<void>}
 */
export async function applyRegionDelta(regionId, side, chapterId = editor.chapter?.id ?? null) {
  const pageStatus = side?.pageStatus ?? undefined
  const result = await getBackend().restoreRegion({
    regionId,
    region: side?.region ?? null,
    pageStatus,
  })
  if (!chapterId || editor.chapter?.id !== chapterId) return
  // A null answer to a *present* side is a failure - the page the region
  // belongs to is no longer open - and not an instruction to remove anything.
  // Passing it through would make a failed redo silently delete the region it
  // was meant to bring back, so the interface is left as it stands.
  if (side?.present && !result) return
  applyRegionState(regionId, side?.present ? result : null, pageStatus)
  const page = pageHolding(regionId)
  if (page) await reloadPage(page.index).catch(() => {})
}

/**
 * Update the in-memory history once a backend push answers.
 *
 * @param {import('../model/history.js').History} history
 * @param {{cursor?: number, entries?: import('../model/history.js').HistoryLabel[]}|null|undefined} view
 * @param {number} expectedCursor
 */
function applyPushView(history, view, expectedCursor) {
  if (editor.history !== history) return
  const cursorDelta = history.cursor - expectedCursor
  if (history.entries.length === expectedCursor) {
    adoptHistory(history, view)
    if (cursorDelta !== 0) {
      history.cursor = Math.min(
        history.entries.length,
        Math.max(0, history.cursor + cursorDelta),
      )
    }
  } else {
    for (let i = 0; i < (view?.entries?.length ?? 0); i++) {
      if (history.entries[i]) {
        history.entries[i] = { seq: view.entries[i].seq, label: view.entries[i].label }
      }
    }
  }
}

/**
 * Queue a history task behind whatever is already in flight, starting
 * immediately if idle.
 *
 * @param {import('../model/history.js').History} history
 * @param {() => Promise<void>} task
 */
function enqueueHistoryTask(history, task) {
  if (!history._busy) {
    history._busy = true
    const p = (async () => {
      try {
        await task()
      } finally {
        history._busy = false
      }
    })()
    history.running = p.then(
      () => undefined,
      (error) => {
        if (editor.history === history) history.error = error
      },
    )
  } else {
    history.running = history.running.then(
      async () => {
        history._busy = true
        try {
          await task()
        } finally {
          history._busy = false
        }
      },
      async () => {
        history._busy = true
        try {
          await task()
        } finally {
          history._busy = false
        }
      },
    ).then(
      () => undefined,
      (error) => {
        if (editor.history === history) history.error = error
      },
    )
  }
}

/**
 * Record a region edit that has **already been performed**, and persist it.
 *
 * Fire-and-forget on the write: the interface's own index moves synchronously
 * so the Undo button never lags the click that enabled it, and the journal is
 * caught up on the next answer the backend gives. A push that fails leaves the
 * index one entry ahead of the file, which the next `openChapter` corrects -
 * losing an undo step is recoverable, and blocking the editor on a disk write
 * after every brush stroke is not.
 *
 * **`chapterId` is the chapter the edit was made in**, read by the caller
 * before it awaited anything. An edit answered after that chapter stopped
 * being the open one goes on that chapter's journal
 * (`recordBackgroundRegionEdit`), never on the open one's: recorded here it
 * would be an undo step in the wrong chapter that replays against a region
 * this chapter does not have. Omitted, it is the open chapter, which is right
 * for an edit that awaited nothing.
 *
 * @param {string} label - i18n key for the undo/redo tooltip
 * @param {string} regionId
 * @param {RegionState} before
 * @param {RegionState} after
 * @param {string|null} [chapterId]
 */
export function recordRegionEdit(label, regionId, before, after, chapterId = editor.chapter?.id ?? null) {
  if (!chapterId) return
  // A cloud stroke's creation still waiting on its render goes on first, so
  // an edit to its seed is never the only step behind it (`heldcreations.js`).
  settleCreation(regionId, before)
  const chapter = editor.chapter
  if (chapter?.id !== chapterId) {
    void recordBackgroundRegionEdit(chapterId, label, regionId, before, after)
    return
  }
  const entry = {
    label,
    op: 'region-state',
    regionId,
    before: sideOf(before),
    after: sideOf(after),
  }
  // A provisional number, replaced by the backend's the moment it answers. It
  // only has to be distinct from the entries already held, and the cursor is
  // what `canUndo` actually reads.
  const provisional = (editor.history.entries.at(-1)?.seq ?? 0) + 1
  pushHistory(editor.history, { seq: provisional, label })
  const history = editor.history
  const expectedCursor = history.cursor
  enqueueHistoryTask(history, async () => {
    try {
      const view = await pushHistoryInChapterOrder(chapter.id, entry)
      applyPushView(history, view, expectedCursor)
    } catch (error) {
      if (editor.history === history) history.error = error
      if (!reportJobConflict(error)) notify({ key: 'notice.history.saveFailed', tone: 'warn' })
    }
  })
}

const chapterHistoryWrites = new Map()
function pushHistoryInChapterOrder(chapterId, entry) {
  const previous = chapterHistoryWrites.get(chapterId)
  const perform = async () => {
    try {
      return await getBackend().historyPush({ chapterId, entry })
    } catch (_firstError) {
      await delay(HISTORY_RETRY_MS)
      return getBackend().historyPush({ chapterId, entry })
    }
  }
  const next = previous ? previous.catch(() => {}).then(perform) : perform()
  chapterHistoryWrites.set(chapterId, next)
  void next.then(
    () => { if (chapterHistoryWrites.get(chapterId) === next) chapterHistoryWrites.delete(chapterId) },
    () => { if (chapterHistoryWrites.get(chapterId) === next) chapterHistoryWrites.delete(chapterId) },
  )
  return next
}

/** Persist an edit that finished after its chapter stopped being the open one. */
export async function recordBackgroundRegionEdit(chapterId, label, regionId, before, after) {
  const entry = {
    label,
    op: 'region-state',
    regionId,
    before: sideOf(before),
    after: sideOf(after),
  }
  try {
    await pushHistoryInChapterOrder(chapterId, entry)
    return true
  } catch (error) {
    console.error('a background region edit could not be saved to undo history', error)
    notify({ key: 'notice.history.saveFailed', tone: 'warn' })
    return false
  }
}

/**
 * Move the journal's cursor on disk and replay whichever delta it names.
 *
 * The cursor moves on the backend *before* anything is applied, deliberately:
 * re-applying a delta to a manifest already in that state is a no-op by
 * construction (`set_mask_visible` answers "it is as you asked" rather than
 * refusing), so an undo interrupted half-way resolves the same way twice.
 *
 * @param {'undo'|'redo'} direction
 * @returns {Promise<void>}
 */
async function replayHistory(direction) {
  const chapter = editor.chapter
  if (!chapter) return
  const history = editor.history
  history._busy = true
  try {
    let step
    try {
      step = await getBackend().historyMove({ chapterId: chapter.id, direction })
    } catch (_firstError) {
      await delay(HISTORY_RETRY_MS)
      try {
        step = await getBackend().historyMove({ chapterId: chapter.id, direction })
      } catch (error) {
        if (editor.history === history) history.error = error
        if (!reportJobConflict(error)) notify({ key: 'notice.history.saveFailed', tone: 'warn' })
        return
      }
    }
    const entry = step?.entry
    if (!entry) return
    if (entry.op !== 'region-state') return
    const side = direction === 'undo' ? entry.before : entry.after
    await applyRegionDelta(entry.regionId, side, chapter.id)
  } finally {
    history._busy = false
  }
}

export function undo() {
  undoHistory(editor.history, replayHistory)
}

/**
 * The key for a chapter write the backend refused because the chapter was not
 * this window's to write: another Manga Cleaner process holds it (`job_busy`),
 * or saved it after this window read it (`job_stale`). The code leads the
 * backend's message, and is looked for anywhere in it because a layer that
 * wraps the message puts a path in front.
 *
 * @param {unknown} error
 * @returns {'notice.job.busy'|'notice.job.stale'|null}
 */
export function jobConflictKey(error) {
  const text = error instanceof Error ? error.message : String(error ?? '')
  if (/\bjob_stale\b/.test(text)) return 'notice.job.stale'
  if (/\bjob_busy\b/.test(text)) return 'notice.job.busy'
  return null
}

/**
 * Say a refused chapter write in its own words, and read a chapter that moved
 * under this window again so the next edit starts from what is on disk.
 * Answers whether it was one, so the caller skips its own notice.
 *
 * @param {unknown} error
 * @returns {boolean}
 */
export function reportJobConflict(error) {
  const key = jobConflictKey(error)
  if (!key) return false
  notify({ key, tone: 'warn' })
  const { project, chapter } = editor
  if (key === 'notice.job.stale' && project && chapter) {
    void openEditorChapter(project.id, chapter.id, { reload: true }).catch((reloadError) => {
      console.error('the chapter could not be read again', reloadError)
    })
  }
  return true
}

export function redo() {
  redoHistory(editor.history, replayHistory)
}

/**
 * Read the chapter's journal index back off disk. Called on open, which is what
 * makes undo survive a restart.
 *
 * @param {string} chapterId
 * @returns {Promise<void>}
 */
async function loadHistory(chapterId) {
  const history = editor.history
  try {
    const view = await getBackend().historyLoad({ chapterId })
    if (editor.history === history) adoptHistory(history, view)
  } catch (error) {
    history.error = error
  }
}

/* ------------------------------------------------------------------ */
/* Escape                                                              */
/* ------------------------------------------------------------------ */

/**
 * The editor's half of `Esc`: drop the transient interaction state. Modals are
 * the shell's business, not the editor's.
 * @returns {boolean} whether anything was cancelled
 */
export function cancelInteraction() {
  // A gesture in progress goes first and on its own: abandoning a half-drawn
  // shape must not also drop the selection the user was working against.
  if (clearDraft()) return true
  // Then the stamp's sampled source, and only while the stamp is the armed
  // tool: nothing else could clear it, and for every other tool Escape has to
  // go on meaning "drop what is selected". The source marker vanishing from
  // the page is the confirmation.
  if (editor.tool === 'cloneHeal' && clearCloneSource()) return true
  let cancelled = false
  if (editor.originalPinned || editor.originalVisible) {
    editor.originalPinned = false
    editor.originalVisible = false
    cancelled = true
  }
  if (editor.selectionId !== null) {
    editor.selectionId = null
    cancelled = true
  }
  if (editor.hoverId !== null) {
    editor.hoverId = null
    cancelled = true
  }
  return cancelled
}
