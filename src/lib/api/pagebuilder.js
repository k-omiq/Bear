/**
 * Deterministic builders for chapters, pages, regions and their masks.
 *
 * Everything here is seeded - pass a `BuildContext` whose `rng` was created
 * from a stable id and the same pages come out on every launch. No
 * `Math.random`, no `Date.now`: timestamps are measured back from a fixed
 * epoch.
 *
 * `fixtures.js` holds the catalogue these builders are applied to.
 */

import { commitMask } from './provenance.js'
import { createRng } from './rng.js'

/** Panel layouts, as percentages of the page: [x, y, w, h]. */
const LAYOUTS = Object.freeze([
  [
    [6, 4, 88, 27],
    [6, 34, 42, 30],
    [52, 34, 42, 30],
    [6, 67, 88, 29],
  ],
  [
    [6, 4, 42, 44],
    [52, 4, 42, 20],
    [52, 26, 42, 22],
    [6, 51, 88, 45],
  ],
  [
    [6, 4, 88, 43],
    [6, 50, 42, 46],
    [52, 50, 42, 46],
  ],
])

const BUBBLE_TEXT = Object.freeze([
  'なにこれ',
  'まって',
  'ここは…',
  'ちがう',
  'どうして',
  'うそだ',
  'いこう',
  'しまった',
  'だれ？',
  'そんな',
])
const SFX_TEXT = Object.freeze(['ゴォォ', 'ドン', 'ザッ', 'バキ', 'シーン'])

/** Fixed point in time the fixtures' ISO timestamps are measured back from. */
const FIXTURE_EPOCH = Date.UTC(2026, 7, 11, 9, 0, 0)

export const APP_VERSION = '0.9.3'

/**
 * @param {number} minutes - how far back from the fixed epoch
 * @returns {string} ISO 8601 timestamp
 */
export function isoMinutesAgo(minutes) {
  return new Date(FIXTURE_EPOCH - minutes * 60000).toISOString()
}

/**
 * A `CommitContext` (provenance.js) whose timestamps come from the fixed
 * epoch instead of a clock, which is what keeps two launches identical.
 *
 * @typedef {import('./provenance.js').CommitContext} BuildContext
 */

/**
 * @param {string} seed - a stable id; equal seeds build equal pages
 * @param {{ value: number }} sequence - the shared mask revision counter
 * @returns {BuildContext}
 */
export function createBuildContext(seed, sequence) {
  const rng = createRng(seed)
  return {
    rng,
    nextSequence: () => {
      sequence.value += 1
      return sequence.value
    },
    created: () => isoMinutesAgo(rng.int(30, 4000)),
  }
}

/**
 * @param {string} chapterId
 * @param {Object} spec
 * @param {BuildContext} ctx
 * @returns {import('../model/types.js').Page[]}
 */
export function makePages(chapterId, spec, ctx) {
  const pages = []
  for (let n = 1; n <= spec.pages; n += 1) {
    const layout = (n - 1) % LAYOUTS.length
    const pageId = `${chapterId}-p${String(n).padStart(3, '0')}`
    const page = {
      id: pageId,
      chapterId,
      index: n - 1,
      number: n,
      file: `${spec.prefix}${String(n).padStart(3, '0')}.${spec.ext ?? 'png'}`,
      sourceSha: ctx.rng.sha256(),
      width: spec.mode === 'longstrip' ? 800 : 1600,
      height: spec.mode === 'longstrip' ? 4000 : 2400,
      layout,
      panels: LAYOUTS[layout].map(([x, y, w, h]) => ({ x, y, w, h })),
      status: 'unclean',
      skipReason: null,
      regions: [],
    }
    const count = spec.noText ? 0 : ctx.rng.int(2, 4)
    for (let i = 0; i < count; i += 1) page.regions.push(makeRegion(page, i, ctx))
    pages.push(page)
  }
  return pages
}

/**
 * @param {import('../model/types.js').Page} page
 * @param {number} i
 * @param {BuildContext} ctx
 * @returns {import('../model/types.js').Region}
 */
export function makeRegion(page, i, ctx) {
  const panel = LAYOUTS[page.layout][i % LAYOUTS[page.layout].length]
  const kind = ctx.rng.next() < 0.78 ? 'bubble' : 'sfx'
  const w = kind === 'bubble' ? 15 + ctx.rng.next() * 6 : 12 + ctx.rng.next() * 7
  const h = kind === 'bubble' ? 17 + ctx.rng.next() * 7 : 11 + ctx.rng.next() * 6
  return {
    id: `${page.id}-r${i + 1}`,
    pageId: page.id,
    sourceSha: page.sourceSha,
    bbox: {
      x: Math.round(panel[0] + 2 + ctx.rng.next() * Math.max(2, panel[2] - w - 4)),
      y: Math.round(panel[1] + 2 + ctx.rng.next() * Math.max(2, panel[3] - h - 4)),
      w: Math.round(w),
      h: Math.round(h),
    },
    kind,
    text: kind === 'sfx' ? ctx.rng.pick(SFX_TEXT) : ctx.rng.pick(BUBBLE_TEXT),
    detected: true,
    source: 'auto',
    outcome: 'pending',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    mask: null,
  }
}

/**
 * Cleans a region the way a completed automatic pass would have left it.
 * Shares `commitMask` with the live session, so what committing a mask resets
 * is defined once.
 *
 * @param {import('../model/types.js').Region} region
 * @param {string} engine - rung id
 * @param {BuildContext} ctx
 * @param {Object} [extra] - passed through to `buildMask`
 * @returns {import('../model/types.js').Region}
 */
export function cleanRegion(region, engine, ctx, extra = {}) {
  commitMask(region, ctx, { engine, ...extra })
  return region
}

/**
 * @param {import('../model/types.js').Page} page
 * @param {BuildContext} ctx
 */
export function cleanPage(page, ctx) {
  for (const region of page.regions) {
    cleanRegion(region, 'fill', ctx)
  }
  page.status = 'cleaned'
}

/**
 * @param {Object} spec
 * @param {string} projectId
 * @param {number} order
 * @param {BuildContext} ctx
 * @returns {import('../model/types.js').Chapter}
 */
export function makeChapter(spec, projectId, order, ctx) {
  const id = `${projectId}-ch${spec.number}`
  const chapter = {
    id,
    projectId,
    name: spec.name,
    number: spec.number,
    order,
    lastOpened: spec.lastOpened,
    sourcePath: spec.sourcePath ?? '',
    sourceFormat: (spec.ext ?? 'png').toUpperCase(),
    noTextDetected: !!spec.noText,
    inputReports: spec.inputReports ?? [],
    pages: makePages(id, spec, ctx),
  }
  const cleanTo =
    spec.fill === 'all'
      ? chapter.pages.length
      : spec.fill === 'part'
        ? Math.max(1, Math.round(chapter.pages.length * 0.5))
        : 0
  for (let i = 0; i < cleanTo; i += 1) cleanPage(chapter.pages[i], ctx)
  return chapter
}

/**
 * Puts one region of a named review cause on a page, growing the page's
 * region list if it is short. Every cause a region can get today is
 * represented, which is what Wandering Moon Ch. 12 exists to exercise.
 * `review.js`'s two cloud causes are not: only a job saved with one of those
 * legacy review states carries them, and nothing makes one any more.
 *
 * @param {import('../model/types.js').Page} page
 * @param {number} index - which region on the page
 * @param {string} cause
 * @param {BuildContext} ctx
 */
export function flagCause(page, index, cause, ctx) {
  const region = ensureRegion(page, index, ctx)
  region.mask = null
  region.outcome = 'pending'
  region.gateSkipCause = null
  region.declineReason = null

  if (cause === 'fitting') {
    cleanRegion(region, 'lama', ctx, { fittingReconstructed: true })
  } else if (cause === 'large') {
    region.bbox = { ...region.bbox, w: 32, h: 26 }
    region.unusuallyLarge = true
    cleanRegion(region, 'lama', ctx)
  } else if (cause === 'declined') {
    region.outcome = 'declined'
    region.declineReason = 'decline.reason.qualityMetric'
  } else if (cause === 'gate-low') {
    region.outcome = 'gate-skipped'
    region.gateSkipCause = 'low-confidence'
  } else if (cause === 'gate-outside') {
    region.kind = 'outside'
    region.outcome = 'gate-skipped'
    region.gateSkipCause = 'outside-bubble'
  }
  if (page.status === 'unclean' && region.outcome === 'cleaned') page.status = 'cleaned'
}

/**
 * @param {import('../model/types.js').Page} page
 * @param {number} index
 * @param {BuildContext} ctx
 * @returns {import('../model/types.js').Region} the region at `index`, created if the page is short
 */
function ensureRegion(page, index, ctx) {
  while (page.regions.length <= index) {
    page.regions.push(makeRegion(page, page.regions.length, ctx))
  }
  return page.regions[index]
}

/**
 * Marks a region as text the detector never found - what the AI mask brush's
 * fallback mechanism exists for.
 *
 * This is deliberately **not** a review cause: `review.js` reports on what the
 * pipeline decided, and the pipeline never saw this region, so there is
 * nothing for it to report. It is not queueable either - an automatic run
 * would miss it again for exactly the same reason, and re-queueing it would
 * quietly clean the one region the fixture exists to leave uncleaned.
 * `isQueueable` in `tools.js` is what enforces that.
 *
 * @param {import('../model/types.js').Page} page
 * @param {number} index
 * @param {BuildContext} ctx
 */
export function markUndetected(page, index, ctx) {
  const region = ensureRegion(page, index, ctx)
  region.detected = false
  region.source = 'auto'
  region.outcome = 'pending'
  region.gateSkipCause = null
  region.declineReason = null
  region.mask = null
}
