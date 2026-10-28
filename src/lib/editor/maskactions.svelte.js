/**
 * What the Layers panel's controls actually do: delete a mask, re-run it,
 * clean a gate-skipped region anyway, show one on the page - and undo any of
 * it.
 *
 * Every one of these goes through the backend seam and then puts the region
 * the adapter returned back into the open chapter. None of them edits a
 * region in place: a mask is the backend's to make and unmake, and the
 * interface holding a divergent copy is how "why does the panel say something
 * different from the file" starts.
 *
 * **Undo.** Each edit is recorded as a pair of `restoreRegion` calls - the
 * region as it was, and the region as it became. That is why the seam grew
 * `restoreRegion`: a mask deleted and then undone has to be findable again by
 * the *backend*, or the restored row's own actions would quietly do nothing.
 * Deleting restores the original text under the mask, and
 * undoing the delete puts the mask back.
 *
 * Two edits are not recorded here, because the backend records them itself,
 * under the lock it makes the edit in: a clean of a detected region
 * (`cleanDetected`, `cleanDetectedOnCloud`) and a rejected result
 * (`rejectResult`). Undo of the first puts the detection back with its mask,
 * and Redo puts the same result back with no new render.
 *
 * **A region edit can move its page.** Cleaning a gate-skipped region makes an
 * unclean page a cleaned one, and deleting the last mask on a page takes that
 * back; the Pages row's mark comes from the page and its track from the
 * regions, so the two disagree the moment one is applied without the other.
 * Every call here therefore carries the page's status as well - forward from
 * the adapter, and backward in each half of the snapshot pair, so undo is
 * symmetric with the edit it reverses.
 *
 * **A rejection is reported, never dropped.** The two calls here that run an
 * engine can fault, and a rejected promise that nothing catches is a control
 * the user pressed and watched do nothing. Both go through
 * `reportRegionEditFailure`, which is also what `toolapply.svelte.js` uses, so
 * a failed edit says the same thing whichever control started it.
 */

import { SvelteMap } from 'svelte/reactivity'
import { getBackend } from '../api/backend.js'
import { hasKey } from '../i18n/index.js'
import { CLOUD_ENGINE, MAX_MASK_PADDING, isCloudMask, isDetected, reRunnable, rejectable, retryWidens } from '../model/masks.js'
import { isRegionCopy, layerOf, sameLayer, sanitizeLayer } from '../model/layers.js'
import { heldStartsOutside } from '../model/review.js'
import { cloudOutcomeOf, requestCloudConsent, runCloudJob } from './cloudflow.svelte.js'
import { notify } from '../state/app.svelte.js'
import {
  adoptBackendHistory,
  applyDetectionPadding,
  applyRegionState,
  claimPageEdit,
  cleanRefusalKey,
  editor,
  pageBusy,
  holdsRegion,
  hover,
  onPendingEdit,
  paddingOf,
  recordRegionEdit,
  replaceRegion,
  pageStatusOf,
  reloadPage,
  reloadRegionPages,
  reportJobConflict,
  select,
  selectedRegion,
  setToolParam,
  setTool,
} from '../state/editor.svelte.js'

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
 * Report a region edit the backend rejected, and answer `false` so the caller
 * reads it as "nothing changed" like every other refusal here.
 *
 * **A rejection used to be a click that did nothing.** These calls run an
 * engine, and an engine can fault: the promise rejected, no caller was catching
 * it, there is no `unhandledrejection` handler in the application, and so the
 * only trace of a failed edit was a line in a console the user does not have
 * open. The region is untouched either way - that half was always true - but a
 * user cannot tell "untouched" from "ignored my click" without being told.
 *
 * Two shapes come back and both are handled. The backend names a fault it can
 * attribute with a catalogue key - `decline.reason.engineFault` is the one this
 * was written for - and that key is rendered as the reason. Anything else is
 * text: a library's own words, a disk that would not write, and on Windows an
 * ONNX Runtime string with a stack in it. None of that goes on screen; it goes
 * to the console, and the reader gets `decline.reason.unknown` plus the
 * sentence that says the region is exactly as it was.
 *
 * The key is checked against the catalogue **and** required to be a decline
 * reason. A bare `hasKey` would let the backend put any string in the app into
 * this sentence, which is a rejection choosing what the interface says.
 *
 * A chapter another Manga Cleaner process is using, or one that changed under
 * this window, is said in its own words instead (`reportJobConflict`).
 *
 * @param {unknown} error
 * @returns {false}
 */
export function reportRegionEditFailure(error) {
  const message = error instanceof Error ? error.message : String(error ?? '')
  const named = message.startsWith('decline.reason.') && hasKey(message)
  // Logged whichever shape it is: a key is a summary, and the detail behind it
  // is what anyone debugging the machine actually needs.
  console.error('a region edit was rejected', error)
  if (reportJobConflict(error)) return false
  notify({
    key: 'notice.mask.rerunFailed',
    params: { reasonKey: named ? message : 'decline.reason.unknown' },
    tone: 'warn',
  })
  return false
}

/**
 * Records one region-level edit as an undoable command. Both directions are
 * the same call with a different snapshot, so redo cannot drift from undo.
 *
 * `chapterId` is the chapter the edit was made in, read before the call went
 * out: every edit here awaits the backend, and one answered after a chapter
 * switch belongs on its own chapter's history, not the open one's.
 *
 * @param {string} label - i18n key for the undo/redo tooltip
 * @param {string} regionId
 * @param {RegionState} before
 * @param {RegionState} after
 * @param {string|null} chapterId
 */
function recordEdit(label, regionId, before, after, chapterId) {
  recordRegionEdit(label, regionId, before, after, chapterId)
}

/**
 * Whether the chapter an edit was made in is still the open one. The
 * interface's own copy is only touched when it is; an answer for a closed
 * chapter is already on disk, and its page shows it when it opens again.
 *
 * @param {string|null} chapterId
 */
function stillOpen(chapterId) {
  return chapterId !== null && editor.chapter?.id === chapterId
}

/**
 * Read the region's page again, and in a longstrip every page its layer
 * reaches across a join before or after the edit (`reloadRegionPages`).
 *
 * @param {number|null} index
 * @param {...(import('../api/backend.js').ApiRegion|null|undefined)} regions - the region before and after
 */
function refreshPage(index, ...regions) {
  if (index !== null) void reloadRegionPages(index, regions)
}

/**
 * Delete a region's mask. The original text under it comes back, **and the row
 * goes with it**.
 *
 * Not "the region with its mask taken off". A region with no mask is a row the
 * panel lists as *unexamined* and a box the canvas draws, so leaving one behind
 * answered a delete with an empty placeholder in the same place - the thing the
 * user was asking to be rid of. The backend keeps the record invisible on disk
 * so the delete can be undone; nothing lists it, and neither does this.
 *
 * Which makes the undo pair the removal pair every other disappearance uses:
 * the region as it was against nothing at all, so undo restores it through
 * `restoreRegion` and redo takes it away again.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>} whether anything was deleted
 */
export async function deleteMask(region) {
  if (!region.mask) return false
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  const before = snapshot(region)
  let result
  try {
    result = await getBackend().deleteMask({ maskId: region.mask.id })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  if (!result) return false
  const after = { region: null, pageStatus: result.pageStatus ?? before.pageStatus }
  // The selection and the hover go with it - `applyRegionState` drops both,
  // for every route into a removal rather than for this one. A selection
  // pointing at a region that is no longer on the page highlights nothing and
  // steps to nothing; the row it belonged to has gone.
  if (stillOpen(chapterId)) applyRegionState(region.id, null, after.pageStatus ?? undefined)
  // A detection has no `visible` flag to turn back on, so the backend keeps
  // its whole page as it was and records the step itself
  // (`history.rs#record_pages`); it reaches this window's index as a
  // `history-changed` event. A patch is soft-deleted, and recorded here.
  if (region.outcome !== 'detected') recordEdit('masks.command.deleteMask', region.id, before, after, chapterId)
  if (stillOpen(chapterId)) refreshPage(pageIndex, before.region)
  return true
}

/**
 * Remove a region that has no mask - a warning the user has read and decided
 * to leave alone.
 *
 * Not the same call as `deleteMask`, because there is no mask to delete - only
 * the row, and a held row has no `visible` flag either: once removed, nothing
 * on the region could bring it back. So `dismissRegion` removes it and the
 * backend records the step with its whole page as it was
 * (`history.rs#record_pages`), as it does for a deleted detection. Both end
 * up in the same place: a row the user deleted is a row that is gone.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>}
 */
export async function deleteRegion(region) {
  if (region.mask) return deleteMask(region)
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  let result
  try {
    result = await getBackend().dismissRegion({ regionId: region.id })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  if (!result) return false
  if (stillOpen(chapterId)) {
    applyRegionState(region.id, null, result.pageStatus ?? undefined)
    refreshPage(pageIndex)
  }
  return true
}

/**
 * Delete whatever a row stands for: its mask if it has one, the region itself
 * if it does not. The one call every delete control in the panel makes, so a
 * warning row and a mask row cannot end up with two different meanings of the
 * same trash icon.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>}
 */
export async function deleteRow(region) {
  return region.mask ? deleteMask(region) : deleteRegion(region)
}

const duplicating = new Set()

/** Copy one regional layer as one undoable creation, then select the copy. */
export async function duplicateRegion(region) {
  if (!isRegionCopy(region)) return false
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null || !claimPageEdit(pageIndex)) return false
  const key = JSON.stringify([chapterId, region.id])
  if (duplicating.has(key)) return false
  duplicating.add(key)
  const before = { region: null, pageStatus: pageStatusOf(region.id) }
  try {
    // Settle the source's edit first so Undo removes the copy before undoing
    // the adjustment that preceded it.
    for (const edit of [...(layerEdits.get(key) ?? [])]) await edit.commit()
    await (layerTails.get(key) ?? Promise.resolve())
    const result = await getBackend().duplicateRegion({ regionId: region.id })
    if (!result?.region) return false
    const copied = result.region
    if (stillOpen(chapterId)) {
      applyRegionState(copied.id, copied, result.pageStatus)
      select(copied.id)
    }
    recordEdit('regionCopy.duplicateUndo', copied.id, before, result, chapterId)
    if (stillOpen(chapterId)) refreshPage(pageIndex, copied)
    return true
  } catch (error) {
    return reportRegionEditFailure(error)
  } finally {
    duplicating.delete(key)
  }
}

/**
 * A changed lower-layer input has been reviewed; keep these committed pixels.
 * Not an undoable edit. Answered after a chapter switch, it is kept on disk
 * and the chapter opened meanwhile is left alone.
 */
export async function keepDependencyResult(region) {
  if (!region.mask?.dependencyReview) return false
  const chapterId = editor.chapter?.id ?? null
  let result
  try {
    result = await getBackend().keepDependencyResult({ regionId: region.id })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  if (!result) return false
  if (!stillOpen(chapterId)) return true
  if (!replaceRegion(result)) return false
  refreshPage(pageIndexOf(region.id), region, result)
  return true
}

/**
 * Re-run a mask: at a named engine, at the one it already used, one rung
 * stronger or simpler, or with the next fill mode.
 *
 * `reopenInTool` is the one kind that is *not* an edit - it hands the region
 * back to the tool that made it with the mask intact, so there is nothing to
 * undo and nothing to record.
 *
 * Two of them go to the cloud, each after its own consent (`rerunInCloud`):
 * Clean with > Cloud, and Try again on a mask the cloud rendered. The second
 * is not a local re-run of the model the endpoint happened to serve; asked
 * without a grant, the native side refuses it rather than run it here.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {'stronger'|'simpler'|'cycleFill'|'reopenInTool'|'retry'|'retryWider'|'engine'} kind
 * @param {string} [engine] - the rung `kind: 'engine'` should run at
 * @returns {Promise<boolean>}
 */
export async function rerunMask(region, kind, engine) {
  if (!region.mask) return false
  if ((kind === 'retry' || kind === 'retryWider') && !reRunnable(region.mask)) return false
  if (kind === 'engine' && engine === CLOUD_ENGINE) return rerunInCloud(region)
  if (kind === 'retry' && isCloudMask(region.mask)) return rerunInCloud(region)
  // Wider is a local re-run with a grown hole; a cloud render has no local
  // hole to grow, so it is not offered there (`retryWidens`).
  if (kind === 'retryWider' && !retryWidens(region.mask)) return false
  // A second press while the first is rendering is not a second render. It
  // queued on the page lock behind the first and snapshot the region the
  // first was about to replace, so its undo step put back a stale mask.
  // Reopen in tool edits nothing, and a run does not hold it back.
  if (rerunning.has(region.id)) return false
  if (kind !== 'reopenInTool' && !claimPageEdit(pageIndexOf(region.id))) return false
  rerunning.add(region.id)
  try {
    return await rerunLocally(region, kind, engine)
  } finally {
    rerunning.delete(region.id)
  }
}

/** The regions a local re-run is in flight for, by id. */
const rerunning = new Set()

/**
 * `rerunMask` once it is known to run here.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {'stronger'|'simpler'|'cycleFill'|'reopenInTool'|'retry'|'retryWider'|'engine'} kind
 * @param {string} [engine]
 * @returns {Promise<boolean>}
 */
async function rerunLocally(region, kind, engine) {
  const chapterId = editor.chapter?.id ?? null
  const before = snapshot(region)
  /** @type {any} */
  let result
  try {
    result = await getBackend().rerunMask({ maskId: /** @type {any} */ (region.mask).id, kind, engine })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  if (!result) return false
  const open = stillOpen(chapterId)
  if (open) replaceRegion(result.region, result.pageStatus)

  if (kind === 'reopenInTool') {
    // Not an edit, only a hand-off to a tool: with its chapter gone there is
    // nothing on screen to hand off.
    if (!open) return false
    select(region.id)
    const reopenTool =
      result.reopenTool ??
      region.tool ??
      region.mask?.provenance?.params_snapshot?.tool ??
      'aiMaskBrush'
    setTool(reopenTool)
    return true
  }
  recordEdit('masks.command.rerunMask', region.id, before, {
    region: result.region,
    pageStatus: result.pageStatus,
  }, chapterId)
  if (open) refreshPage(pageIndexOf(region.id), before.region, result.region)
  return true
}

/**
 * Clean a region the script gate skipped, on the user's say-so.
 *
 * **The engine is the user's own pick for this kind of text.** The automatic
 * pass never cleans an out-of-balloon region - it refuses to
 * burn SFX and artwork on a guess, and that ruling stands - so the tool
 * window's *Text outside bubbles* row had no automatic effect at all.
 * This is where it bites: the row says where a
 * region of that kind *starts* when a person asks for it to be cleaned, and the
 * bubble row answers the gate's other two causes, which are regions inside a
 * balloon held back for a different reason.
 *
 * A starting rung and never a ceiling, exactly as it is for a run: the ladder
 * still escalates when the quality metric declines a patch.
 *
 * **Always local.** The native side renders Clean anyway in the cloud only
 * over a patch the region already has, because consent binds to a stored
 * crop and mask, and a region the gate held back has none. Once it is
 * cleaned here it has one, and Clean with > Cloud is the way to the cloud
 * from there, with its own consent.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {string} [chosenEngine] - a model picked from Clean with, run as named
 * @returns {Promise<boolean>}
 */
export async function cleanAnyway(region, chosenEngine) {
  const chapterId = editor.chapter?.id ?? null
  const before = snapshot(region)
  const params = /** @type {any} */ (editor.toolParams.autoClean ?? {})
  // The native side's rule for the same choice (`model/review.js`
  // `heldStartsOutside`): outside-bubble text, and a candidate no balloon
  // holds, start on the outside pick.
  const engine = chosenEngine ?? (
    heldStartsOutside(region)
      ? (params.outsideEngine ?? 'lama')
      : (params.bubbleEngine ?? 'fill'))
  /** @type {any} */
  let result
  // A model picked from Clean with runs as named, even on a region the
  // quality metric declined; Clean anyway's pick is a start the ladder climbs.
  const extra = {
    ...(engine === 'solid' ? { bubbleColor: params.bubbleColor ?? '#ffffff' } : {}),
    ...(chosenEngine ? { exact: true } : {}),
  }
  try {
    result = await getBackend().cleanAnyway({ regionId: region.id, engine,
      ...(Object.keys(extra).length ? { params: extra } : {}) })
  } catch (error) {
    return reportRegionEditFailure(error)
  }
  if (!result) return false
  const open = stillOpen(chapterId)
  if (open) replaceRegion(result.region, result.pageStatus)
  recordEdit('masks.command.cleanAnyway', region.id, before, {
    region: result.region,
    pageStatus: result.pageStatus,
  }, chapterId)
  if (open) refreshPage(pageIndexOf(region.id), before.region, result.region)
  return true
}

/**
 * Approve a held region's mask: store the region the script gate held back,
 * or a held candidate, as a detected region (docs/detect-clean.md §3,
 * `approve_held_mask`). **Nothing is cleaned.** Its mask shows on the page
 * like every detection's, the Selection tool edits it, and a later Clean,
 * here or on the cloud GPU, takes it with the rest. `cleanAnyway` is the
 * other answer to the same row, and cleans it at once on this computer.
 *
 * The mask is grown by Text cleanup's Mask padding, as a Detect run grows
 * the masks beside it. The detection is a new region with its own id, so the
 * page is read again and the selection follows the mask. Like every edit of
 * a detection it is one undo step the backend records with the page as it
 * was (`history.rs#record_pages`): Undo puts the held row back.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>} whether the region became a detection
 */
export async function approveHeldMask(region) {
  if (region?.outcome !== 'gate-skipped' && region?.outcome !== 'candidate') return false
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null) return false
  /** @type {Awaited<ReturnType<import('../api/backend.js').Backend['approveHeldMask']>>} */
  let result
  try {
    result = await getBackend().approveHeldMask({
      regionId: region.id,
      paddingPx: paddingOf(/** @type {any} */ (editor.toolParams.autoClean ?? {})),
    })
  } catch (error) {
    console.error('a mask approval was rejected', error)
    if (!reportJobConflict(error)) notify({ key: 'notice.mask.approveFailed', tone: 'warn' })
    return false
  }
  if (!result) return false
  if (!stillOpen(chapterId)) return true
  try {
    await reloadPage(pageIndex)
  } catch (error) {
    console.error('the page could not be read again after a mask approval', error)
  }
  // The held row is gone and the detection has its own id: the selection
  // follows the mask, or is let go where a padding merged it into another.
  if (!stillOpen(chapterId)) return true
  if (editor.selectionId === region.id) select(holdsRegion(result.regionId) ? result.regionId : null)
  if (editor.hoverId === region.id) hover(null)
  return true
}

/**
 * Clean one detected region here, from its stored mask: nothing is detected
 * again (docs/detect-clean.md §3, `apply_tool` on a detected region).
 *
 * Sent as Auto clean with the region's id and the tool's Solid colour. With
 * no engine, the region's own stored `pick` is where the native side starts
 * it, and the ladder escalates from there as a run's does. With an engine
 * (Clean with > a local model, from the region menu) that model renders it
 * and nothing else: the user named it. The detection is replaced by what the
 * clean made, under the same id. Like a run's clean it is one undo step, and
 * the backend records it, not this: the native side keeps the detection the
 * clean replaced, so Undo puts it back with its mask and Redo puts the same
 * result back with no new render (`editor.svelte.js#applyCleanDelta`). The
 * step reaches this window's index as a `history-changed` event. Reject
 * result does what Undo does at any later time (`rejectResult`), and the
 * patch is deleted like any other.
 *
 * It is an edit of its own page and not a run: a run elsewhere in the chapter
 * does not hold it back, and only the page the run is on right now does
 * (`claimPageEdit`).
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {string} [engine] - a local rung (`ROW_ENGINES`); absent starts at the stored pick
 * @returns {Promise<boolean>} whether anything changed
 */
export async function cleanDetected(region, engine) {
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null) return false
  // A second press while the first is cleaning is not a second clean.
  if (cleaning.has(region.id) || !claimPageEdit(pageIndex)) return false
  cleaning.add(region.id)
  try {
    const params = /** @type {any} */ (editor.toolParams.autoClean ?? {})
    /** @type {any} */
    let result
    try {
      result = await getBackend().applyTool({
        tool: 'autoClean',
        chapterId,
        pageIndex,
        regionId: region.id,
        // No engine: the region's stored `pick` is the rung it starts at. A
        // named engine overrides it. The colour is what a Solid pick paints.
        params: { bubbleColor: String(params.bubbleColor ?? '#ffffff'), ...(engine ? { engine } : {}) },
      })
    } catch (error) {
      return reportRegionEditFailure(error)
    }
    if (result?.status !== 'applied' || !result.region) return false
    if (!stillOpen(chapterId)) return true
    if (result.region.id === region.id) replaceRegion(result.region, result.pageStatus)
    refreshPage(pageIndex, region, result.region)
    return true
  } finally {
    cleaning.delete(region.id)
  }
}

/** The detections a clean of their own is in flight for, by id. */
const cleaning = new Set()

/**
 * Clean one detected region on the cloud GPU: one consent for this region,
 * then Auto clean with the grant, which the native side renders on the
 * endpoint and commits over the detection (`region.rs#render_in_cloud`).
 *
 * Always the cloud, whatever the region's stored pick. A Text cleanup run
 * keeps LaMa picks on this computer (`cloudrun.js`), but here the user chose
 * the cloud for this one region, and a LaMa pick cleaned locally instead was
 * the defect this replaced. Like `cleanDetected` it is one undo step the
 * backend records when the render commits: Undo puts the detection back with
 * its mask, and Redo the same result with no new render and no new request.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>} whether the render committed
 */
export async function cleanDetectedOnCloud(region) {
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null) return false
  const where = { chapterId, pageIndex, regionId: region.id }
  const params = { engine: CLOUD_ENGINE }
  const grant = await requestCloudConsent({ ...where, intent: { action: 'applyTool', tool: 'autoClean', params } })
  if (!grant) return false
  /** @type {any} */
  const result = await runCloudJob(
    grant,
    where,
    (cloudParams) => getBackend().applyTool({ tool: 'autoClean', ...where, params: { ...params, ...cloudParams } }),
    cloudOutcomeOf,
  )
  if (result?.status !== 'applied' || !result.region) return false
  if (!stillOpen(chapterId)) return true
  if (result.region.id === region.id) replaceRegion(result.region, result.pageStatus)
  refreshPage(pageIndex, region, result.region)
  return true
}

/**
 * Reject a generated result: the region is detected again, with the mask it
 * had before the clean, ready to be cleaned again. What Undo does right after
 * a clean, offered at any later time.
 *
 * **Nothing is recorded here.** The backend takes the result off and writes
 * the undo step under one lock (`reject_result`), and answers with the
 * chapter's index after it, which is adopted. Undo of a reject puts the same
 * result back with no new render.
 *
 * A result with nothing kept to go back to is refused and said so: a chapter
 * cleaned before a clean kept its detection (`clean_not_restorable`) gets the
 * sentence that names the way out, which is deleting the layer. The region is
 * as it was either way.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>} whether the result was rejected
 */
export async function rejectResult(region) {
  if (!rejectable(region)) return false
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null) return false
  // A second press while the first is in flight is not a second reject.
  if (rejecting.has(region.id) || !claimPageEdit(pageIndex)) return false
  rejecting.add(region.id)
  try {
    /** @type {Awaited<ReturnType<import('../api/backend.js').Backend['rejectResult']>>} */
    let result
    try {
      result = await getBackend().rejectResult({ regionId: region.id })
    } catch (error) {
      console.error('a result could not be rejected', error)
      if (!reportJobConflict(error)) notify({ key: cleanRefusalKey(error), tone: 'warn' })
      return false
    }
    if (!result?.region) return false
    if (!stillOpen(chapterId)) return true
    replaceRegion(result.region, result.pageStatus)
    adoptBackendHistory(chapterId, result.history)
    refreshPage(pageIndex, region, result.region)
    return true
  } finally {
    rejecting.delete(region.id)
  }
}

/** The results a reject of their own is in flight for, by region id. */
const rejecting = new Set()

/**
 * Whether a layer is a cleaned result whose post-processing the Layers row
 * offers to change: a mask an engine made, not a detection, a paint stroke or
 * a clone. Whether it can be changed is the backend's answer
 * (`regionFinishingPreviews`): only a cloud result kept its answer, and the
 * panel says so for the rest.
 *
 * @param {import('../api/backend.js').ApiRegion|null|undefined} region
 * @returns {boolean}
 */
export function offersFinishing(region) {
  if (!region?.mask || isDetected(region)) return false
  return !['paint', 'clone'].includes(region.mask.provenance?.engine ?? '')
}

/**
 * A Post-processing panel asked for from outside its row (`⌘P`,
 * `KeyboardLayer`): the region whose row opens it. The Layers list expands
 * that row and the row opens its panel and clears this; a row the list
 * cannot show clears it too, so a request never waits to surprise a later
 * expand.
 */
export const finishingRequest = $state({ /** @type {string|null} */ id: null })

/** @param {string} regionId */
export function requestFinishingPanel(regionId) {
  finishingRequest.id = regionId
}

/**
 * The finishing a layer's record says its pixels were made by
 * (`params_snapshot.postprocess`), or null when it names none.
 *
 * @param {import('../api/backend.js').ApiRegion|null|undefined} region
 * @returns {{finishing: string, grayscale: boolean}|null}
 */
export function recordedFinishing(region) {
  const postprocess = /** @type {any} */ (region?.mask?.provenance?.params_snapshot)?.postprocess
  if (typeof postprocess?.finishing !== 'string') return null
  return { finishing: postprocess.finishing, grayscale: postprocess.grayscale === true }
}

/**
 * The notice for a refused change of finishing, by the code that leads the
 * backend's message (`refinish.rs#Unavailable`).
 *
 * @param {unknown} error
 * @returns {string}
 */
export function finishingRefusalKey(error) {
  const text = error instanceof Error ? error.message : String(error ?? '')
  if (/\brefinish_not_cloud\b/.test(text)) return 'masks.finishing.unavailable.notCloud'
  if (/\brefinish_no_result\b/.test(text)) return 'masks.finishing.unavailable.noResult'
  if (/\brefinish_page_changed\b/.test(text)) return 'masks.finishing.unavailable.pageChanged'
  if (/\brefinish_unsupported\b/.test(text)) return 'masks.finishing.unavailable.unsupported'
  return 'masks.finishing.applyFailed'
}

/**
 * Finish a cloud result again by another post-processing method, from the
 * answer its attempt kept: no request, no GPU.
 *
 * **Nothing is recorded here.** The backend replaces the patch and writes the
 * undo step under one lock (`refinish.rs#apply_at`), as `rejectResult` does,
 * and answers with the chapter's index after it, which is adopted. Undo puts
 * the patch's earlier revision back (`restoreRegion`).
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {{finishing: string, grayscale: boolean}} choice
 * @returns {Promise<boolean>} whether the result was finished again
 */
export async function setRegionFinishing(region, choice) {
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null) return false
  if (refinishing.has(region.id) || !claimPageEdit(pageIndex)) return false
  refinishing.add(region.id)
  try {
    /** @type {Awaited<ReturnType<import('../api/backend.js').Backend['setRegionFinishing']>>} */
    let result
    try {
      result = await getBackend().setRegionFinishing({ regionId: region.id, ...choice })
    } catch (error) {
      console.error('a result could not be finished again', error)
      if (!reportJobConflict(error)) notify({ key: finishingRefusalKey(error), tone: 'warn' })
      return false
    }
    if (!result?.region) return false
    if (!stillOpen(chapterId)) return true
    replaceRegion(result.region, result.pageStatus)
    if (result.history) adoptBackendHistory(chapterId, result.history)
    refreshPage(pageIndex, region, result.region)
    return true
  } finally {
    refinishing.delete(region.id)
  }
}

/** The results a change of finishing is in flight for, by region id. */
const refinishing = new Set()

/**
 * Set a detected region's text type: speech bubble text (`inside: true`) or
 * text outside bubbles. It is the kind a later Clean starts the region as, by
 * Text cleanup's row for that kind, and the colour its mask is drawn in
 * (`model/masks.js#maskColorFor`). The native side clears the balloon colour
 * it measured, and moves a stored Fill or Solid pick to LaMa when the region
 * moves outside a balloon (`library.rs#retype_detection`).
 *
 * The region menu's Text type. Picking the type the region has already does
 * nothing. Like a hand edit of a detection's mask (`drawing.svelte.js`) it is
 * one undo step, which the backend records with the page as it was
 * (`history.rs#record_pages`).
 * A region cleaned or removed since the menu opened is refused, and said so.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {boolean} inside
 * @returns {Promise<boolean>} whether the type changed
 */
export async function setDetectionType(region, inside) {
  if (!isDetected(region) || typeof inside !== 'boolean') return false
  if (region.insideBubble === inside) return false
  const chapterId = editor.chapter?.id ?? null
  const pageIndex = pageIndexOf(region.id)
  /** @type {import('../api/backend.js').ApiRegion|null} */
  let result
  try {
    result = await getBackend().setDetectionType({ regionId: region.id, inside })
  } catch (error) {
    return reportDetectionTypeFailure(error)
  }
  if (!result) return false
  if (!stillOpen(chapterId)) return true
  replaceRegion(result)
  refreshPage(pageIndex, region, result)
  return true
}

/**
 * Say a refused text type change, the way a refused mask edit is said: a
 * chapter another process holds, or one that moved under this window, in its
 * own words; anything else in the one sentence that says nothing changed,
 * with the detail on the console.
 *
 * @param {unknown} error
 * @returns {false}
 */
function reportDetectionTypeFailure(error) {
  console.error('a text type change was rejected', error)
  if (reportJobConflict(error)) return false
  notify({ key: 'notice.mask.typeFailed', tone: 'warn' })
  return false
}

/**
 * Change only this detection's total mask padding and reload its page so the
 * mask overlay and saved value move together. Like every edit of a
 * detection it is one undo step the backend records (`history.rs#record_pages`),
 * and Undo also splits a merge it made.
 *
 * A padding that runs this mask into another makes the two one. The earlier
 * in reading order is the one kept, so this row can be the one that goes: the
 * change is saved all the same, and the page read again shows what is left.
 *
 * A caller that waited before saving (the `;` and `'` keys) passes the
 * chapter and page the region was on when it began (`at`), so a chapter
 * switch while it waited does not lose the save.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {number} paddingPx
 * @param {{chapterId: string|null, pageIndex: number|null}} [at] - where the region is, instead of where the open chapter has it now
 * @returns {Promise<boolean>}
 */
export async function setDetectedMaskPadding(region, paddingPx, at) {
  if (!isDetected(region) || !Number.isInteger(paddingPx) || paddingPx < 0 || paddingPx > MAX_MASK_PADDING) return false
  const chapterId = at ? at.chapterId : editor.chapter?.id ?? null
  const pageIndex = at ? at.pageIndex : pageIndexOf(region.id)
  // Only the open chapter's run is known here; one on a chapter left since
  // is the native side's to refuse, and is said as any refusal is.
  if (!chapterId || pageIndex === null || (stillOpen(chapterId) && !claimPageEdit(pageIndex))) return false
  try {
    const result = await getBackend().setDetectionPadding({ chapterId, pageIndex, regionId: region.id, paddingPx })
    const taken = result?.removed?.includes(region.id) ?? false
    if (!result?.changed.includes(region.id) && !taken) return false
    if (result.removed?.length) notify({ key: 'notice.mask.paddingMerged', params: { count: result.removed.length } })
    if (stillOpen(chapterId)) {
      const current = editor.chapter.pages.flatMap((page) => page.regions).find((entry) => entry.id === region.id)
      if (current && !taken) replaceRegion({ ...current, paddingPx })
      await reloadPage(pageIndex).catch((error) => {
        console.error('the page could not be refreshed after saving mask padding', error)
        if (stillOpen(chapterId)) notify({ key: 'notice.mask.paddingRefreshFailed', tone: 'warn' })
      })
    }
    return true
  } catch (error) {
    console.error('a mask padding change was rejected', error)
    if (!reportJobConflict(error)) notify({ key: 'notice.mask.paddingFailed', tone: 'warn' })
    return false
  }
}

/**
 * The padding each detection is on its way to, by region id, from the first
 * request until the last one is saved. Every control that moves a padding -
 * the Layers row's slider, the region menu's, the `;` and `'` keys - asks
 * through `requestDetectedMaskPadding`, so none of them can drop a change made
 * while another is saving, and each shows the value it is going to.
 *
 * @type {Map<string, number>}
 */
const paddingTarget = new SvelteMap()
/** @type {Map<string, number>} */
const paddingWaiting = new Map()
/** The value each region's save in flight is sending. @type {Map<string, number>} */
const paddingSending = new Map()

/**
 * The padding a detection shows: the one it is on its way to, else its own.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {number}
 */
export function shownMaskPadding(region) {
  return paddingTarget.get(region.id) ?? region.paddingPx ?? 0
}

/**
 * Move a detection's padding, last request wins: a request made while one is
 * saving waits for it, and only the newest waiting value is sent. Each save
 * is one undo step (`setDetectedMaskPadding`).
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {number} paddingPx
 * @param {{chapterId: string|null, pageIndex: number|null}} [at] - where the region was when the request began (`setDetectedMaskPadding`); now, when not given
 * @returns {Promise<void>}
 */
export async function requestDetectedMaskPadding(region, paddingPx, at) {
  const value = Math.round(Math.min(MAX_MASK_PADDING, Math.max(0, paddingPx)))
  const where = at ?? { chapterId: editor.chapter?.id ?? null, pageIndex: pageIndexOf(region.id) }
  const page = `page:${where.chapterId}:${where.pageIndex}`
  // A value set by a slider or the box replaces a run of key presses still
  // waiting on the same detection, which would otherwise land after it.
  if (paddingKeyRun?.target === region.id) {
    clearTimeout(paddingKeyRun.timer)
    paddingKeyRun = null
  } else if (paddingKeyRun?.target === page) {
    flushPaddingKeys()
  }
  // A save of the whole page goes first, and lands before this one region's
  // is sent: the two in flight together could land in either order, and the
  // page's landing second would undo this region's.
  const pageFirst = pagePaddingSaving.get(page)?.done
  paddingTarget.set(region.id, value)
  paddingWaiting.set(region.id, value)
  if (paddingSending.has(region.id)) return
  let sent = value
  try {
    if (pageFirst) {
      paddingSending.set(region.id, value)
      await pageFirst
    }
    while (paddingWaiting.has(region.id)) {
      sent = /** @type {number} */ (paddingWaiting.get(region.id))
      paddingWaiting.delete(region.id)
      paddingSending.set(region.id, sent)
      await setDetectedMaskPadding(region, sent, where)
    }
  } finally {
    paddingSending.delete(region.id)
    // A run of key presses still waiting to be sent keeps showing its value.
    if (paddingTarget.get(region.id) === sent) paddingTarget.delete(region.id)
  }
}

/** How long a run of padding key presses waits before it is saved. */
const PADDING_KEY_SETTLE_MS = 250

/**
 * The run of padding key presses waiting to be saved: what it pads (a region
 * id, or `page:<index>`), the value it is at, how to save it, and how to put
 * the shown value back when Undo drops it.
 *
 * @typedef {Object} PaddingKeyRun
 * @property {string} target
 * @property {number} value
 * @property {(value: number) => void} save
 * @property {() => void} revert
 * @property {{chapterId: string|null, pageIndex: number|null}} [at] - a detection's run: where the region was at its first press
 * @property {ReturnType<typeof setTimeout>} timer
 */
/** @type {PaddingKeyRun|null} */
let paddingKeyRun = null

onPendingEdit((direction) => {
  const run = paddingKeyRun
  if (!run) return false
  clearTimeout(run.timer)
  paddingKeyRun = null
  if (direction === 'undo') run.revert()
  else run.save(run.value)
  return true
})

/**
 * The padding every detection on a page shares, or null when they differ or
 * there are none.
 *
 * @param {number} pageIndex
 * @returns {number|null}
 */
function pagePadding(pageIndex) {
  const shown = new Set((editor.chapter?.pages[pageIndex]?.regions ?? []).filter(isDetected).map(shownMaskPadding))
  return shown.size === 1 ? /** @type {number} */ ([...shown][0]) : null
}

/**
 * `;` and `'`: one pixel less or more mask padding. On the selected detection
 * when one is selected; otherwise on every detection of the page, and as the
 * Text cleanup panel's Mask padding, which the next Detect uses too. The page
 * steps from the padding its detections share, so a page padded at 6 goes to
 * 7 even while the panel says 0.
 *
 * A held key or a quick run of presses is saved once, when the presses stop,
 * so it is one undo step rather than one a pixel; Undo while it waits drops
 * it. The value moves at once: the region menu, the Layers row and the panel
 * all show it while it waits. The run saves to the page it started on.
 *
 * A page a run holds refuses the key, as it refuses any edit, before the
 * panel's value moves: a padding the panel showed and the page never got
 * would be one the next Detect used and the masks did not have.
 *
 * @param {number} direction - below zero for less
 */
export function stepMaskPadding(direction) {
  const step = direction < 0 ? -1 : 1
  const clamp = (/** @type {number} */ n) => Math.min(MAX_MASK_PADDING, Math.max(0, n))
  const region = selectedRegion()
  if (region && isDetected(region)) {
    const id = region.id
    const before = paddingTarget.get(id)
    const value = clamp(shownMaskPadding(region) + step)
    // Where the run began: a chapter switched while it waits still gets it.
    const at = paddingKeyRun?.target === id && paddingKeyRun.at ? paddingKeyRun.at
      : { chapterId: editor.chapter?.id ?? null, pageIndex: pageIndexOf(id) }
    paddingTarget.set(id, value)
    settlePaddingKeys(id, value, (px) => void requestDetectedMaskPadding(region, px, at), () => {
      // What it showed before is still right only while a save of it is
      // under way; once that save landed, the region holds the value itself.
      if (before !== undefined && (paddingSending.has(id) || paddingWaiting.has(id))) paddingTarget.set(id, before)
      else paddingTarget.delete(id)
    }, at)
    return
  }
  const chapterId = editor.chapter?.id
  if (!chapterId) return
  const pageIndex = editor.pageIndex
  const target = `page:${chapterId}:${pageIndex}`
  const running = paddingKeyRun?.target === target ? paddingKeyRun.value : null
  const params = editor.toolParams.autoClean ?? {}
  const panel = paddingOf(params)
  // A save of this page still on its way is where the next press starts:
  // the page's detections only show it once the page is read again.
  const value = clamp((running ?? pagePaddingSaving.get(target)?.px ?? pagePadding(pageIndex) ?? panel) + step)
  const before = params.maskPadding
  const ids = (editor.chapter?.pages[pageIndex]?.regions ?? []).filter(isDetected).map((detection) => detection.id)
  if (ids.length && !claimPageEdit(pageIndex)) return
  setToolParam('autoClean', 'maskPadding', value)
  // A page with nothing detected keeps the value for its next Detect, and
  // has no masks to save it into. A run waiting elsewhere is saved now, so
  // an Undo here cannot reach back to it.
  if (!ids.length) {
    flushPaddingKeys()
    return
  }
  // Every row and menu of the page shows the value while it waits.
  const shownBefore = new Map(ids.map((id) => [id, paddingTarget.get(id)]))
  for (const id of ids) paddingTarget.set(id, value)
  const settled = (/** @type {number} */ px) => {
    for (const id of ids) if (paddingTarget.get(id) === px) paddingTarget.delete(id)
  }
  // What the run's first press found, for Undo and for a refused save alike.
  const revert = running !== null && paddingKeyRun ? paddingKeyRun.revert : () => {
    setToolParam('autoClean', 'maskPadding', before ?? 0)
    for (const [id, shown] of shownBefore) {
      if (shown === undefined) paddingTarget.delete(id)
      else paddingTarget.set(id, shown)
    }
  }
  settlePaddingKeys(target, value, (px) => {
    // The run saves into the chapter it was pressed in, or nowhere.
    if (editor.chapter?.id !== chapterId) {
      settled(px)
      return
    }
    // A run that took the page while the presses waited refuses the save,
    // and the panel goes back to what the page still has.
    if (!claimPageEdit(pageIndex)) {
      revert()
      return
    }
    const done = applyDetectionPadding('page', { pageIndex, paddingPx: px }).finally(() => {
      if (pagePaddingSaving.get(target)?.done === done) pagePaddingSaving.delete(target)
      settled(px)
    })
    pagePaddingSaving.set(target, { px, done })
  }, revert)
}

/** Save a waiting run of padding key presses now. */
function flushPaddingKeys() {
  const run = paddingKeyRun
  if (!run) return
  clearTimeout(run.timer)
  paddingKeyRun = null
  run.save(run.value)
}

/**
 * Per `page:<chapter>:<index>`, the padding a page save is sending, and the
 * save, which a single detection's waits for (`requestDetectedMaskPadding`).
 *
 * @type {Map<string, {px: number, done: Promise<unknown>}>}
 */
const pagePaddingSaving = new Map()

/**
 * @param {string} target
 * @param {number} value
 * @param {(value: number) => void} save
 * @param {() => void} revert - puts back what was shown before the run's first press
 * @param {{chapterId: string|null, pageIndex: number|null}} [at] - a detection's run: where the region was at its first press
 */
function settlePaddingKeys(target, value, save, revert, at) {
  const run = paddingKeyRun
  if (run) clearTimeout(run.timer)
  // A press on another target saves the first one's run straight away.
  if (run && run.target !== target) run.save(run.value)
  paddingKeyRun = {
    target,
    value,
    save,
    revert: run?.target === target ? run.revert : revert,
    at,
    timer: setTimeout(() => {
      const pending = paddingKeyRun
      paddingKeyRun = null
      pending?.save(pending.value)
    }, PADDING_KEY_SETTLE_MS),
  }
}

/* ------------------------------------------------------------------ */
/* Layer appearance: opacity, position, rotation, lock                 */
/* ------------------------------------------------------------------ */

/**
 * Report a layer change the native side refused. A refusal names itself with
 * a `masks.refused.*` key (`LayerRefusal` in `cleaner_core::patch`); anything
 * else is a failure the reader gets the generic sentence for, and the console
 * gets the detail.
 *
 * @param {unknown} error
 * @returns {false}
 */
export function reportLayerRefusal(error) {
  const message = error instanceof Error ? error.message : String(error ?? '')
  const named = message.startsWith('masks.refused.') && hasKey(message)
  console.error('a layer change was rejected', error)
  notify({
    key: 'masks.notice.layerRefused',
    params: { reasonKey: named ? message : 'decline.reason.unknown' },
    tone: 'warn',
  })
  return false
}

/**
 * Per layer, the tail of whatever layer write is queued: a second control can
 * fire before the first one's answer has replaced the region both hold, and
 * two sessions on one layer must not interleave their writes.
 *
 * @type {Map<string, Promise<unknown>>}
 */
const layerTails = new Map()
/** Active appearance gestures, settled before a layer is duplicated. */
const layerEdits = new Map()

/**
 * @typedef {Object} LayerEdit
 * @property {boolean} closed - a settled edit cannot take another preview
 * @property {(changes: Partial<import('../model/layers.js').LayerStyle>) => void} preview - write now, coalesced; no history
 * @property {(changes?: Partial<import('../model/layers.js').LayerStyle>) => Promise<boolean>} commit - the last write, then exactly one undo entry
 * @property {() => Promise<void>} cancel - put the layer back as the edit found it; no history
 */

/**
 * One layer-appearance edit, from its first preview to its one undo entry.
 *
 * **Previews are real writes, coalesced.** An opacity slider has nothing it
 * can fake on screen: the cleaned tile is every layer composited together, so
 * the only honest preview is the native side compositing it. So `preview`
 * sends the latest value through `setLayerStyle`, at most one call in flight,
 * and a value that arrives meanwhile replaces the one waiting rather than
 * queueing behind it - a drag across the slider is a handful of writes, never
 * one per `input` event. Each answer replaces the region, which moves its
 * `appearance` digest and therefore its tile URL.
 *
 * **History is written once, by `commit`.** The undo entry pairs the region
 * as the edit found it with the region the last write returned, so undoing a
 * drag across the slider is one step back to where the drag began. An edit
 * whose writes all landed where they started records nothing.
 *
 * The native command enforces the capabilities (`Library::set_layer_style`),
 * so a refusal ends the edit with a notice and whatever did land stays
 * undoable.
 *
 * **A layer deleted mid-edit ends it with no entry.** The slider settles
 * 400 ms after its last step and a row can unmount under a trash or `⌘⌫`
 * press; by then the delete has its own entry, holding the style the
 * previews left. An entry recorded after it would pair "visible" with
 * "visible", and undoing it would put the deleted layer back.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {string} [labelKey] - the undo entry's label
 * @param {string|null} [chapterId] - the chapter the layer belongs to; the
 *   open one unless the caller knows better (a page leaving the editor on a
 *   chapter switch writes into the chapter it was showing)
 * @returns {LayerEdit}
 */
export function openLayerEdit(region, labelKey = 'masks.command.layerStyle', chapterId = editor.chapter?.id ?? null) {
  const regionId = region.id
  const key = JSON.stringify([chapterId, regionId])
  const backend = getBackend()
  /** The fields this edit sets, merged; applied over the layer as it is when each write goes out. */
  /** @type {Partial<import('../model/layers.js').LayerStyle>|null} */
  let changes = null
  /** The region and its style as this edit found them - read at its first write, behind any earlier edit. */
  /** @type {RegionState|null} */
  let before = null
  /** @type {import('../model/layers.js').LayerStyle|null} */
  let start = null
  /** @type {import('../api/backend.js').ApiRegion|null} */
  let landed = null
  let refused = false
  /** @type {Promise<void>|null} */
  let pumping = null

  /** Whether the layer was deleted from the open chapter since the edit began. */
  function deleted() {
    return stillOpen(chapterId) && !holdsRegion(regionId)
  }

  /** The region as it stands now: the open chapter's copy, or the last answer. */
  function fresh() {
    const open = stillOpen(chapterId)
      ? editor.chapter.pages.flatMap((page) => page.regions).find((item) => item.id === regionId)
      : null
    return open ?? landed ?? region
  }

  async function pump() {
    // Behind any other edit of this layer that is still writing, so this
    // one's changes land on top of that one's rather than over them.
    await (layerTails.get(key) ?? Promise.resolve())
    while (changes && !refused && !deleted()) {
      const now = fresh()
      if (!now?.mask) break
      const current = sanitizeLayer(layerOf(now))
      if (!before) {
        before = snapshot(now)
        start = current
      }
      const next = sanitizeLayer({ ...current, ...changes })
      if (sameLayer(next, current)) break
      let result
      try {
        result = await backend.setLayerStyle({ regionId, layer: next })
      } catch (error) {
        refused = true
        reportLayerRefusal(error)
        break
      }
      if (!result) break
      landed = result
      if (stillOpen(chapterId)) replaceRegion(result)
    }
  }

  function flush() {
    if (!pumping) {
      const run = pump().finally(() => {
        if (pumping === run) pumping = null
        if (layerTails.get(key) === run) layerTails.delete(key)
      })
      pumping = run
      layerTails.set(key, run)
    }
    return pumping
  }

  /** @param {Partial<import('../model/layers.js').LayerStyle>|undefined} more */
  function want(more) {
    if (more) changes = { ...(changes ?? {}), ...more }
  }

  let closed = false
  let ending = null
  const forget = () => {
    layerEdits.get(key)?.delete(edit)
    if (layerEdits.get(key)?.size === 0) layerEdits.delete(key)
  }
  const edit = {
    get closed() { return closed },
    preview(more) {
      if (closed) return
      want(more)
      void flush()
    },
    commit(more) {
      if (ending) return ending
      if (closed) return Promise.resolve(false)
      closed = true
      want(more)
      ending = (async () => {
        // A value that arrived while a write was in flight is sent by the
        // running pump; one that arrived after it finished needs a pass of its
        // own. Two passes at most, and the second is usually a no-op.
        await flush()
        await flush()
        if (!landed || !before || !start || sameLayer(sanitizeLayer(layerOf(landed)), start)) return false
        if (deleted()) return false
        recordEdit(labelKey, regionId, before, { region: landed, pageStatus: before.pageStatus }, chapterId)
        if (stillOpen(chapterId)) refreshPage(pageIndexOf(regionId), before.region, landed)
        return true
      })().finally(forget)
      return ending
    },
    async cancel() {
      if (closed) return
      closed = true
      try {
        await flush()
        if (!landed || !start) return
        changes = { ...start }
        refused = false
        await flush()
      } finally { forget() }
    },
  }
  if (!layerEdits.has(key)) layerEdits.set(key, new Set())
  layerEdits.get(key).add(edit)
  return edit
}

/**
 * One layer change as one undoable edit: a lock, a reset, a finished drag.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {Partial<import('../model/layers.js').LayerStyle>} changes
 * @param {string} [labelKey]
 * @param {string|null} [chapterId] - see `openLayerEdit`
 * @returns {Promise<boolean>} whether anything changed
 */
export function updateLayer(region, changes, labelKey, chapterId = editor.chapter?.id ?? null) {
  if (!region?.mask) return Promise.resolve(false)
  return openLayerEdit(region, labelKey, chapterId).commit(changes)
}

/** How long a run of opacity key presses on a layer waits before it is one undo step. */
const OPACITY_KEY_SETTLE_MS = 400
/** @type {{id: string, edit: LayerEdit, opacity: number, timer: ReturnType<typeof setTimeout>}|null} */
let opacityKeyRun = null

onPendingEdit((direction) => {
  const run = opacityKeyRun
  if (!run) return false
  clearTimeout(run.timer)
  opacityKeyRun = null
  if (direction === 'undo') void run.edit.cancel()
  else void run.edit.commit()
  return true
})

/**
 * `I` and `Shift+I` on a region copy being placed: its own opacity ten
 * percent less or more, which is what the bar's Opacity row shows then.
 * Each press previews; the run is committed once, when the presses stop.
 * Undo while it waits puts the layer back; Redo commits it now.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {number} direction - below zero for less
 */
export function stepLayerOpacity(region, direction) {
  let run = opacityKeyRun
  if (run) clearTimeout(run.timer)
  if (run && (run.id !== region.id || run.edit.closed)) {
    if (!run.edit.closed) void run.edit.commit()
    run = null
  }
  const now = run?.opacity ?? layerOf(region).opacity
  const opacity = Math.min(100, Math.max(0, direction < 0 ? (Math.ceil(now / 10) - 1) * 10 : (Math.floor(now / 10) + 1) * 10))
  const edit = run?.edit ?? openLayerEdit(region)
  edit.preview({ opacity })
  opacityKeyRun = {
    id: region.id,
    edit,
    opacity,
    timer: setTimeout(() => {
      opacityKeyRun = null
      void edit.commit()
    }, OPACITY_KEY_SETTLE_MS),
  }
}

/* ------------------------------------------------------------------ */
/* The cloud half                                                      */
/* ------------------------------------------------------------------ */

/**
 * The open chapter's page index for a region, or null when it is not open.
 *
 * @param {string} regionId
 * @returns {number|null}
 */
function pageIndexOf(regionId) {
  for (const page of editor.chapter?.pages ?? []) {
    if (page.regions.some((candidate) => candidate.id === regionId)) return page.index
  }
  return null
}

/**
 * Whether a run is on this region's page right now, so a control that edits
 * the region waits (`editor.svelte.js#pageBusy`).
 *
 * @param {{id: string}} region
 * @returns {boolean}
 */
export function regionPageBusy(region) {
  return pageBusy(pageIndexOf(region.id))
}

/**
 * Render a region in the cloud with one consent, and land what comes back as
 * one undoable edit. A render that does not commit has had its notice from the
 * status element; the region is as it was.
 *
 * The chapter can change while the dialog is up or the render runs. A result
 * for a chapter that is no longer open is not swapped into the one that is:
 * the native side stored it on its own page, and its undo entry goes on that
 * chapter's history.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {import('../model/types.js').OperationIntent} intent
 * @param {(params: import('../api/backend.js').CloudRunParams) => Promise<any>} call
 * @param {string} label - the undo entry's i18n key
 * @returns {Promise<boolean>}
 */
async function renderInCloud(region, intent, call, label) {
  const chapterId = editor.chapter?.id
  const pageIndex = pageIndexOf(region.id)
  if (!chapterId || pageIndex === null) return false
  const where = { chapterId, pageIndex, regionId: region.id }
  const before = snapshot(region)
  const grant = await requestCloudConsent({ ...where, intent })
  if (!grant) return false
  const result = await runCloudJob(grant, where, call, (answer) => (answer ? { phase: 'committed' } : null))
  if (!result?.region) return false
  const open = stillOpen(chapterId)
  if (open) replaceRegion(result.region, result.pageStatus)
  recordEdit(label, region.id, before, { region: result.region, pageStatus: result.pageStatus }, chapterId)
  if (open) refreshPage(pageIndex, before.region, result.region)
  return true
}

/**
 * Re-run a mask on the cloud GPU: Clean with > Cloud, or Try again on a cloud
 * mask. The same request either way, so the same consent and the same intent.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>}
 */
function rerunInCloud(region) {
  const maskId = region.mask?.id
  if (!maskId) return Promise.resolve(false)
  return renderInCloud(
    region,
    { action: 'rerunMask', mask_id: maskId, kind: 'engine', engine: CLOUD_ENGINE },
    (params) => getBackend().rerunMask({ maskId, kind: 'engine', engine: CLOUD_ENGINE, params }),
    'masks.command.rerunMask',
  )
}

/**
 * Point at a region on the canvas. A declined region has no mask and nothing
 * visibly happened to it, so this is the only way to find it.
 *
 * Selection only. `hoverId` is transient and is cleared by whoever set it (the
 * highlight contract in `state/editor.svelte.js`); this is a click, so there
 * is no matching "and now the pointer left" to clear it again, and a hover
 * left switched on would outlive the row it came from. Selection is sticky by
 * design and lights the canvas through `isHighlighted` on its own.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 */
export function showOnPage(region) {
  select(region.id)
}

/**
 * Run one of the entries the region context menu offers, by the id
 * `model/masks.js#regionMenuSections` gave it.
 *
 * Nothing new happens here: the menu is a second route to the row's own
 * controls, and it takes exactly the same calls they do. A menu that grew its
 * own idea of what Delete means is the defect `deleteRow` exists to prevent.
 *
 * Clean with on a detected region cleans it for the first time, with the
 * model picked: a local one here, Cloud on the cloud GPU. On a cleaned one it
 * replaces the layer, as the row's picker does. Text type is a detected
 * region's alone, Approve mask a held region's, and Reject result a cleaned
 * detection's, the three entries with no row control: `setDetectionType`,
 * `approveHeldMask` and `rejectResult`.
 *
 * @param {string} id
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>}
 */
export async function runRegionMenuItem(id, region) {
  if (!region) return false
  if (id === 'retry') return rerunMask(region, 'retry')
  if (id === 'retryWider') return rerunMask(region, 'retryWider')
  if (id === 'reject') return rejectResult(region)
  if (id === 'delete') return deleteRow(region)
  if (id === 'duplicate') return duplicateRegion(region)
  if (id === 'approveMask') return approveHeldMask(region)
  if (id === 'approve') return cleanAnyway(region)
  if (id.startsWith('approve:')) return cleanAnyway(region, id.slice('approve:'.length))
  if (id.startsWith('engine:')) {
    const engine = id.slice('engine:'.length)
    if (!isDetected(region)) return rerunMask(region, 'engine', engine)
    return engine === CLOUD_ENGINE ? cleanDetectedOnCloud(region) : cleanDetected(region, engine)
  }
  if (id === 'type:inside') return setDetectionType(region, true)
  if (id === 'type:outside') return setDetectionType(region, false)
  return false
}

/**
 * Run one of the actions a row offers, by the id `model/masks.js` gave it.
 *
 * @param {string} id
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {Promise<boolean>}
 */
export async function runMaskAction(id, region) {
  switch (id) {
    case 'stronger':
    case 'simpler':
    case 'cycleFill':
    case 'reopenInTool':
      return rerunMask(region, id)
    case 'cleanAnyway':
      return cleanAnyway(region)
    case 'cleanDetected':
      return cleanDetected(region)
    case 'cleanDetectedCloud':
      return cleanDetectedOnCloud(region)
    case 'showOnPage':
      showOnPage(region)
      return true
    default:
      return false
  }
}
