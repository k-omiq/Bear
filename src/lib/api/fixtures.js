/**
 * The mock backend's catalogue: which projects exist, what shape each one is
 * in, and the settings and About panel a session starts with. `pagebuilder.js`
 * turns these specs into pages, regions and masks.
 *
 * Deterministic by construction: every value comes from a seeded generator or
 * from a literal, so two launches produce byte-identical fixtures. Nothing
 * here reads `Math.random` or the clock - relative times ("2 h ago") are
 * fixture data, held as an i18n key plus parameters, never computed from
 * `Date.now()`.
 *
 * Shapes extend the `src/lib/model` typedefs with the fields a backend owns
 * (source paths, file names, interrupted jobs, input reports). See the
 * `ApiProject` / `ApiChapter` / `ApiPage` / `ApiRegion` typedefs in
 * `backend.js`.
 *
 * Project names, chapter names and file names are user data, not interface
 * copy, and so are not translated - as is the Japanese sample text
 * `pagebuilder.js` puts on a region, which stands in for image content.
 */

import {
  APP_VERSION,
  cleanPage,
  createBuildContext,
  flagCause,
  isoMinutesAgo,
  makeChapter,
  markUndetected,
} from './pagebuilder.js'

export { APP_VERSION }

const HOURS_AGO_2 = { key: 'time.relative.hoursAgo', params: { count: 2 } }
const YESTERDAY = { key: 'time.relative.yesterday', params: {} }
const DAYS_AGO_2 = { key: 'time.relative.daysAgo', params: { count: 2 } }
const DAYS_AGO_3 = { key: 'time.relative.daysAgo', params: { count: 3 } }
const DAYS_AGO_4 = { key: 'time.relative.daysAgo', params: { count: 4 } }
const LAST_WEEK = { key: 'time.relative.lastWeek', params: {} }
const WEEKS_AGO_2 = { key: 'time.relative.weeksAgo', params: { count: 2 } }
const WEEKS_AGO_3 = { key: 'time.relative.weeksAgo', params: { count: 3 } }

/** @typedef {import('./pagebuilder.js').BuildContext} BuildContext */

/**
 * Every cause a region can be flagged for today, one region each - five of
 * them, on Wandering Moon Ch. 12. Nothing that is not a review cause belongs
 * in here. `review.js`'s legacy cloud causes are not: nothing makes them any
 * more (see `pagebuilder.js#flagCause`).
 */
const EVERY_REVIEW_CAUSE = Object.freeze([
  [2, 0, 'fitting'],
  [2, 1, 'large'],
  [5, 0, 'gate-low'],
  [6, 0, 'declined'],
  [6, 1, 'gate-outside'],
])

/**
 * @param {import('../model/types.js').Project} project
 * @param {BuildContext} ctx
 */
function decorateWanderingMoon(project, ctx) {
  const chapter = project.chapters[0]
  for (let i = 0; i < 7; i += 1) cleanPage(chapter.pages[i], ctx)
  for (const [pageIndex, regionIndex, cause] of EVERY_REVIEW_CAUSE) {
    flagCause(chapter.pages[pageIndex], regionIndex, cause, ctx)
  }
  // Text the auto pass never found - the AI mask brush's fallback case, not a
  // review cause and not queueable. See `markUndetected`.
  markUndetected(chapter.pages[4], 2, ctx)
  project.interruptedJob = { chapterId: chapter.id, pageIndex: 7 }
}

/**
 * @param {import('../model/types.js').Project} project
 * @param {BuildContext} ctx
 */
function decorateNineSkies(project, ctx) {
  const chapter = project.chapters[0]
  flagCause(chapter.pages[0], 0, 'fitting', ctx)
  flagCause(chapter.pages[3], 0, 'large', ctx)
}

/** @type {ReadonlyArray<Object>} */
const PROJECT_SPECS = Object.freeze([
  {
    id: 'wandering-moon',
    name: 'Wandering Moon',
    mode: 'single',
    sourcePath: '~/scans/wandering-moon',
    lastOpened: HOURS_AGO_2,
    starred: false,
    decorate: decorateWanderingMoon,
    chapters: [
      {
        number: 12,
        name: 'Vault of Ash',
        pages: 24,
        prefix: 'wm',
        fill: 'none',
        lastOpened: HOURS_AGO_2,
      },
      { number: 11, name: 'Grey Tide', pages: 22, prefix: 'wm', fill: 'all', lastOpened: DAYS_AGO_3 },
      { number: 10, name: 'Nightfall', pages: 20, prefix: 'wm', fill: 'all', lastOpened: LAST_WEEK },
    ],
  },
  {
    id: 'emberfall-reborn',
    name: 'Emberfall Reborn',
    mode: 'single',
    sourcePath: '~/scans/emberfall',
    lastOpened: YESTERDAY,
    starred: true,
    chapters: [
      { number: 25, name: 'The Shell Cracks', pages: 36, prefix: 'em', fill: 'all', lastOpened: YESTERDAY },
      { number: 24, name: 'Hatchling', pages: 30, prefix: 'em', fill: 'all', lastOpened: WEEKS_AGO_2 },
    ],
  },
  {
    id: 'tsuki-to-hane',
    name: 'Tsuki to Hane',
    mode: 'single',
    sourcePath: '~/scans/tsuki',
    lastOpened: DAYS_AGO_2,
    starred: false,
    chapters: [
      { number: 107, name: 'Feather Weight', pages: 20, prefix: 'th', fill: 'none', lastOpened: DAYS_AGO_2 },
      { number: 106, name: 'Small Hours', pages: 18, prefix: 'th', fill: 'all', lastOpened: LAST_WEEK },
    ],
  },
  {
    id: 'nine-skies',
    name: 'Nine Skies',
    mode: 'single',
    sourcePath: '~/scans/nine-skies',
    lastOpened: DAYS_AGO_3,
    starred: false,
    decorate: decorateNineSkies,
    chapters: [
      { number: 56, name: 'Ninth Sky', pages: 22, prefix: 'ns', fill: 'all', lastOpened: DAYS_AGO_3 },
      { number: 55, name: 'Eighth Sky', pages: 24, prefix: 'ns', fill: 'all', lastOpened: WEEKS_AGO_2 },
    ],
  },
  {
    id: 'neon-alley',
    name: 'Neon Alley',
    mode: 'longstrip',
    sourcePath: '~/downloads/neon-alley',
    lastOpened: DAYS_AGO_4,
    starred: false,
    conversion: { from: 'WebP', to: 'PNG' },
    chapters: [
      {
        number: 4,
        name: 'Rain Check',
        pages: 6,
        prefix: 'na',
        ext: 'webp',
        fill: 'part',
        lastOpened: DAYS_AGO_4,
        inputReports: [
          { key: 'notice.input.joinAnomaly', params: { first: 3, second: 4 }, tone: 'warn' },
        ],
      },
      { number: 3, name: 'Signal Lost', pages: 6, prefix: 'na', ext: 'webp', fill: 'all', lastOpened: WEEKS_AGO_3 },
    ],
  },
  {
    id: 'yoake-photobook',
    name: 'Yoake photobook',
    mode: 'single',
    sourcePath: '~/scans/yoake',
    lastOpened: LAST_WEEK,
    starred: false,
    chapters: [
      {
        number: 1,
        name: 'Full set',
        pages: 48,
        prefix: 'yo',
        fill: 'none',
        noText: true,
        lastOpened: LAST_WEEK,
      },
    ],
  },
])

/**
 * Builds the whole fixture set. Two calls return equal (but not identical)
 * data - the seeds depend only on ids, never on call order.
 *
 * @returns {{ projects: import('../model/types.js').Project[], sequence: number }}
 */
export function buildFixtures() {
  const sequence = { value: 100 }
  const projects = PROJECT_SPECS.map((spec) => {
    const project = {
      id: spec.id,
      name: spec.name,
      mode: spec.mode,
      readingDirection: 'rtl',
      created: isoMinutesAgo(60 * 24 * 30),
      appVersion: APP_VERSION,
      sourcePath: spec.sourcePath,
      lastOpened: spec.lastOpened,
      starred: spec.starred,
      interruptedJob: null,
      conversion: spec.conversion ?? null,
      chapters: [],
    }
    project.chapters = spec.chapters.map((chapterSpec, order) =>
      makeChapter(
        { ...chapterSpec, mode: spec.mode },
        spec.id,
        order,
        createBuildContext(`${spec.id}-ch${chapterSpec.number}`, sequence),
      ),
    )
    if (spec.decorate) {
      spec.decorate(project, createBuildContext(`${spec.id}-decorate`, sequence))
    }
    return project
  })
  return { projects, sequence: sequence.value }
}

/**
 * A chapter created during a session. Its pages are generated from the same
 * seeded builder as the fixtures, so a chapter with a given id always holds
 * the same pages.
 *
 * @param {import('../model/types.js').Project} project
 * @param {{ number: number, name: string, pages?: number, sourcePath?: string, lastOpened?: Object }} spec
 * @param {{ value: number }} sequence - the mock's shared mask revision counter
 * @returns {import('../model/types.js').Chapter}
 */
export function buildChapter(project, spec, sequence) {
  return makeChapter(
    {
      number: spec.number,
      name: spec.name,
      // A chapter's own folder, which is a different fact from the project's -
      // see `ApiChapter` in `backend.js`. The mock's default is the subfolder
      // the chapter's name implies, because that is the layout a scan folder
      // with several chapters in it actually has.
      sourcePath: spec.sourcePath ?? `${project.sourcePath}/${spec.name}`,
      pages: spec.pages ?? (project.mode === 'longstrip' ? 6 : 18),
      prefix: 'pg',
      fill: 'none',
      mode: project.mode,
      lastOpened: spec.lastOpened ?? { key: 'time.relative.justNow', params: {} },
    },
    project.id,
    project.chapters.length,
    createBuildContext(`${project.id}-ch${spec.number}`, sequence),
  )
}

/**
 * A project created during a session, with its first chapter.
 *
 * @param {{ id: string, name: string, mode: 'single'|'longstrip', sourcePath: string, readingDirection?: 'rtl'|'ltr' }} spec
 * @param {{ value: number }} sequence
 * @returns {import('../model/types.js').Project}
 */
export function buildProject(spec, sequence) {
  const project = {
    id: spec.id,
    name: spec.name,
    mode: spec.mode,
    readingDirection: spec.readingDirection ?? 'rtl',
    created: isoMinutesAgo(0),
    appVersion: APP_VERSION,
    sourcePath: spec.sourcePath,
    lastOpened: { key: 'time.relative.justNow', params: {} },
    starred: false,
    interruptedJob: null,
    conversion: null,
    chapters: [],
  }
  project.chapters.push(buildChapter(project, { number: 1, name: 'Chapter 1' }, sequence))
  return project
}

/**
 * The settings a fresh install starts with. Reading direction is RTL by
 * default; cloud is opt-in and off.
 *
 * `theme` is `system`, matching the session's own default. It was `light`,
 * and because the backend seeds a session that has never existed
 * (`reconcileSettings()`), a genuinely fresh install adopted light over the
 * app's own default - a theme flip nothing on screen explained.
 *
 * @returns {Object} settings snapshot
 */
export function defaultSettings() {
  return {
    theme: 'system',
    readingDirection: 'rtl',
    cloudEngines: 'blocked',
    engineCeiling: 'lama',
    originalView: 'hold',
    cleanAllDetectedText: false,
    retainDetectionMasks: true,
    confirmBeforeSpending: true,
    sidecarPath: '',
    fluxModel: '',
    fluxBackend: 'auto',
  }
}

/**
 * The About panel's contents: the
 * attribution list, the written offer of source, model versions and the
 * cloud provider terms statement.
 *
 * @returns {{ appVersion: string, facts: Array<{ labelKey: string, value: string }> }}
 */
export function aboutInfo() {
  return {
    appVersion: APP_VERSION,
    facts: [
      { labelKey: 'about.fact.licence', value: 'GPL-3.0-only' },
      { labelKey: 'about.fact.source', value: 'https://example.invalid/manga-cleaner' },
      {
        labelKey: 'about.fact.detector',
        value: 'Available separately: Comic Text Detector (CTD), comic_text_detector (GPL-3.0) · Ogkalu comic text & bubble detector (Small and Full), ogkalu/comic-text-and-bubble-detector (Apache-2.0) · SAM-TS-L lettering mask, mayocream/koharu-text-sam-ts-l (Apache-2.0 per its model card; setup downloads and exports it on this computer or on your own cloud GPU) · image-script-identification osd_lstm (Apache-2.0)',
      },
      {
        labelKey: 'about.fact.engines',
        value: 'lama-manga onnx opset 17 (MIT)',
      },
      {
        labelKey: 'about.fact.cloud',
        value: 'Modal or Beam, your own account, off by default, confirmed per request',
      },
      { labelKey: 'about.fact.runtime', value: 'ONNX Runtime, CPU execution provider' },
    ],
  }
}
