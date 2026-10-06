/**
 * The seam between the canvas and the tools: what happens when a region on the
 * page is clicked, or when a drawn shape turns out to be *about* a region that
 * already exists.
 *
 * The canvas owns *which* region was hit and nothing about what a tool does;
 * this module owns the one call that turns a hit into an edit - the adapter
 * call, the cloud gate in front of it, the region swap and the undo record.
 * `drawing.svelte.js` owns the gestures that make regions rather than edit
 * them, and comes back here whenever a gesture lands on an existing one.
 *
 * `maskactions.svelte.js` is the pattern, followed exactly - a pair of region
 * snapshots and one `restoreRegion` call in each direction, so redo cannot
 * drift from undo, and so a mask made here is as findable by the backend after
 * an undo as one made by the automatic pass.
 */

import { getBackend } from '../api/backend.js'
import {
  editor,
  pageStatusOf,
  recordBackgroundRegionEdit,
  recordRegionEdit,
  reloadPage,
  replaceRegion,
  select,
} from '../state/editor.svelte.js'
import { cloudOutcomeOf, cloudRefused, requestCloudConsent, runCloudJob } from './cloudflow.svelte.js'
import { toolSpendsCloud } from './tools.js'
// The reporter lives beside the actions it was written for, and this module
// already follows that one for everything else it does; a second copy of the
// same three lines is a second sentence a failed edit could start saying.
import { reportRegionEditFailure } from './maskactions.svelte.js'
import { paintParamsOf } from './paint.js'

/**
 * @typedef {Object} RegionState
 * @property {import('../api/backend.js').ApiRegion} region
 * @property {string|null} pageStatus
 */

/**
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {RegionState}
 */
function snapshot(region) {
  return {
    region: /** @type {any} */ ($state.snapshot(region)),
    pageStatus: pageStatusOf(region.id),
  }
}

/**
 * Apply the active tool to one region - the canvas's whole share of what a
 * click does, and the landing point for a gesture that turned out to be about
 * a region that already exists.
 *
 * @param {string} regionId
 * @param {Record<string, unknown>} [extraParams] - merged over the tool's own,
 *   for a gesture that carries geometry (`bbox`) the parameters cannot
 * @returns {Promise<boolean>} whether anything changed
 */
export async function applyActiveToolToRegion(regionId, extraParams) {
  const chapter = editor.chapter
  if (!chapter) return false

  // Before the call, not after it. The undo record is a pair of region
  // snapshots, so an id the open chapter does not hold cannot be recorded -
  // and asking the adapter first would let it mutate its own state for an edit
  // this side can neither swap in nor reverse. Unreachable while every id
  // comes from a rendered region; the ordering is what keeps it unreachable.
  const located = locateRegion(regionId)
  if (!located) return false
  const { region, pageIndex } = located

  const tool = editor.tool
  // Text cleanup is not a per-region tool, and there is nothing of it to apply
  // to one. A click on a region selects it and does no more
  // (`RegionLayer.svelte#onRegionClick`); a run starts from the panel's own
  // action, in the mode and over the scope the panel shows; one detected
  // region is cleaned by `maskactions.svelte.js#cleanDetected`. Sending the
  // tool's parameters from here named the stored scope and no mode, and a
  // run started that way would be a Detect & clean over that whole scope
  // rather than anything the user chose.
  if (tool === 'autoClean') return false

  const mergedParams = { ...$state.snapshot(editor.toolParams[tool] ?? {}), ...(extraParams ?? {}) }
  const points = extraParams?.points ?? (/** @type {any} */ (extraParams?.stroke)?.points)
  const paint = extraParams?.paint ?? (points ? paintParamsOf(tool, mergedParams, points) : null)
  const params = {
    ...mergedParams,
    ...(paint ? { paint } : {}),
  }

  // Refused in the interface, before anything is sent. The adapter's own block
  // is the second line of defence, not the first.
  if (cloudRefused(tool, params)) return false

  const before = snapshot(region)
  select(regionId)

  if (toolSpendsCloud(tool, params)) {
    return applyInCloud({ chapterId: chapter.id, pageIndex, regionId, tool, params, before })
  }

  // Every caller of this function fires it and walks away - a region click does
  // not await it, and neither does the gesture that lands on an existing
  // region - so a rejection here reached nothing at all: no handler, no notice,
  // and no `unhandledrejection` listener in the application to catch it last.
  // A tool that faulted was a click that did nothing. It is reported through
  // the same reporter the Layers row's own controls use, and the answer is
  // `false`, which is what this function already says for every other way of
  // changing nothing.
  /** @type {any} */
  let result
  try {
    result = await getBackend().applyTool({
      tool,
      params,
      chapterId: chapter.id,
      pageIndex,
      regionId,
    })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  return landIn(chapter.id, result, regionId, before)
}

/**
 * Render in the cloud a region the open chapter already holds, after one
 * consent: the second half of a drawn region pointed at the cloud, which
 * `createRegion` made locally first.
 *
 * @param {string} regionId
 * @param {string} tool
 * @param {Record<string, unknown>} params - the tool's own, cloud choice included
 * @returns {Promise<boolean>} whether anything changed
 */
export async function renderRegionInCloud(regionId, tool, params, creationBefore = null) {
  const chapter = editor.chapter
  const located = locateRegion(regionId)
  if (!chapter || !located) return false
  return applyInCloud({
    chapterId: chapter.id,
    pageIndex: located.pageIndex,
    regionId,
    tool,
    params,
    before: creationBefore ?? snapshot(located.region),
    label: creationBefore ? 'canvas.command.drawMask' : 'canvas.command.applyTool',
  })
}

/**
 * The cloud half of a click: one consent, then the render, watched.
 *
 * The chapter can change while the dialog is up or the render runs; `landIn`
 * puts the result on the chapter it was made in either way.
 *
 * @param {{chapterId: string, pageIndex: number, regionId: string, tool: string, params: Record<string, unknown>, before: RegionState}} spec
 * @returns {Promise<boolean>}
 */
async function applyInCloud({ chapterId, pageIndex, regionId, tool, params, before, label = 'canvas.command.applyTool' }) {
  const where = { chapterId, pageIndex, regionId }
  const intent = { action: 'applyTool', tool, params }
  const grant = await requestCloudConsent({ ...where, intent })
  if (!grant) return false

  const result = await runCloudJob(
    grant,
    where,
    (cloudParams) => getBackend().applyTool({ tool, params: { ...params, ...cloudParams }, ...where }),
    cloudOutcomeOf,
  )
  if (!result) return false
  return landIn(chapterId, result, regionId, before, label)
}

/**
 * Land an answer in the chapter the edit was made in.
 *
 * The chapter can change while the call is out - a local rung takes seconds, a
 * cloud render minutes. A result for a chapter that is no longer open is not
 * swapped into the one that is, and nothing is selected there: the native side
 * has already stored it on its own page, where the chapter shows it when it
 * opens again, and its undo entry goes on that chapter's journal.
 *
 * @param {string} chapterId - the chapter the edit was made in
 * @param {any} result
 * @param {string} regionId
 * @param {RegionState} before
 * @param {string} [label]
 * @returns {boolean|Promise<boolean>} whether anything changed
 */
function landIn(chapterId, result, regionId, before, label = 'canvas.command.applyTool') {
  if (editor.chapter?.id !== chapterId && result?.status === 'applied') {
    if (!result.region) return false
    return recordBackgroundRegionEdit(chapterId, label, regionId, before,
      { region: result.region, pageStatus: result.pageStatus ?? before.pageStatus })
  }
  return landApplied(result, regionId, before, label)
}

/**
 * Swap in what `applyTool` answered and record it for undo.
 *
 * @param {any} result
 * @param {string} regionId
 * @param {RegionState} before
 * @returns {boolean} whether anything changed
 */
function landApplied(result, regionId, before, label = 'canvas.command.applyTool') {
  switch (result?.status) {
    case 'applied': {
      if (!result.region) return false
      // The page's status comes back with the region, as it does from every
      // other region-level edit: `api/tools.js#applyToolToRegion` moves an
      // unclean page to cleaned, and a region put back without its page's
      // status is how a full track ends up under a "not cleaned" mark.
      if (!replaceRegion(result.region, result.pageStatus)) return false
      recordRegionEdit(label, regionId, before, {
        region: result.region,
        pageStatus: result.pageStatus ?? before.pageStatus,
      })
      const pageIndex = locateRegion(regionId)?.pageIndex
      if (pageIndex !== undefined) void reloadPage(pageIndex).catch(() => {})
      return true
    }
    // `'blocked'` is a path the gate above did not recognise, and the adapter
    // has already said so on the notice channel. A cloud render that stopped
    // has had its notice from the status element.
    default:
      return false
  }
}

/**
 * @param {string} regionId
 * @returns {import('../api/backend.js').ApiRegion|null}
 */
export function findRegion(regionId) {
  return locateRegion(regionId)?.region ?? null
}

/**
 * A region of the open chapter and the index of the page that holds it.
 *
 * @param {string} regionId
 * @returns {{region: import('../api/backend.js').ApiRegion, pageIndex: number}|null}
 */
export function locateRegion(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    const region = page.regions.find((candidate) => candidate.id === regionId)
    if (region) return { region, pageIndex: page.index }
  }
  return null
}
