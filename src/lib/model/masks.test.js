/**
 * The Masks model's own decisions. `maskrows.test.js` covers the rows built on
 * top of these; this is the layer underneath - the fill-mode vocabulary, the
 * engines a machine may be asked for, and what the region context menu offers
 * for one region.
 */

import { describe, expect, it } from 'vitest'
import {
  BETA_ENGINES,
  CLOUD_ENGINE,
  FILL_MODES,
  ROW_ENGINES,
  isCloudMask,
  isFailedClean,
  maskEngine,
  nextFillMode,
  provenanceFacts,
  reRunnable,
  rejectable,
  retriesAgain,
  listsVariations,
  retryWidens,
  regionMenuSections,
  reviewEntryActions,
  variesEachTime,
  rowEngines,
  draftMaskColor,
  maskColorFor,
  regionInsideBubble,
  generationsOf,
  generationsReasonKey,
} from './masks.js'
import { rungLabel } from './ladder.js'

/**
 * @param {string} engine
 * @returns {any} a region carrying a mask from that engine
 */
function masked(engine) {
  return {
    id: 'c1-p001-r0',
    mask: { id: 'c1-p001-r0-m1', fillMode: 'match-surround', provenance: { engine } },
  }
}

/** @param {any} sections @returns {string[]} every item id, in order */
function ids(sections) {
  return sections.flatMap((section) => section.items.map((item) => item.id))
}

describe('the fill-mode cycle', () => {
  it('comes back to where it started', () => {
    let mode = FILL_MODES[0]
    for (const _ of FILL_MODES) mode = nextFillMode(mode)
    expect(mode).toBe(FILL_MODES[0])
  })
})

describe('rowEngines', () => {
  it('offers rung 3a only where the sidecar is installed', () => {
    expect(rowEngines({ flux: true })).toEqual(['fill', 'lama', 'flux'])
    expect(rowEngines({ flux: false })).not.toContain('flux')
    expect(rowEngines({ flux: false })).toContain('lama')
  })

  /**
   * The weights are downloaded after install, so a
   * fresh machine has no inpainter - and until this, the picker offered one.
   */
  it('drops a rung whose weights are not on this machine', () => {
    const offered = rowEngines({ flux: false, lama: false })
    // the two arithmetic rungs need no weights
    expect(offered).toEqual(['fill'])
  })

  it('offers a rung the map says nothing about', () => {
    // The map records what is *missing*. A rung nobody has taught it about
    // must not vanish from every picker in the app.
    expect(rowEngines({ flux: true })).toContain('lama')
  })

  it('withholds only rung 3a before anything has asked the machine', () => {
    expect(rowEngines()).toEqual(['fill', 'lama'])
  })

  it('offers Nano Banana only on a yes, because it needs the user\'s own key', () => {
    expect(rowEngines({ nanobanana: true })).toContain('nanobanana')
    expect(rowEngines({ nanobanana: false })).not.toContain('nanobanana')
    expect(rowEngines({})).not.toContain('nanobanana')
    expect(rowEngines(undefined)).not.toContain('nanobanana')
  })

  it('offers the cloud last, and only when an endpoint is ready', () => {
    expect(rowEngines({ flux: true })).not.toContain(CLOUD_ENGINE)
    expect(rowEngines({ flux: true }, { cloud: false })).not.toContain(CLOUD_ENGINE)
    expect(rowEngines({ flux: true, nanobanana: true }, { cloud: true, beta: true })).toEqual([...ROW_ENGINES, CLOUD_ENGINE])
    expect(rowEngines(undefined, { cloud: true })).toEqual(['fill', 'lama', CLOUD_ENGINE])
  })

  it('offers the Screentone fill only while Settings > Beta has it on', () => {
    expect(BETA_ENGINES).toEqual(['screentone'])
    expect(rowEngines({ flux: true })).not.toContain('screentone')
    expect(rowEngines({ flux: true }, { beta: false })).not.toContain('screentone')
    expect(rowEngines({ flux: true, nanobanana: true }, { beta: true })).toEqual([...ROW_ENGINES])
    // A beta rung whose weights are missing stays out with the setting on.
    expect(rowEngines({ screentone: false }, { beta: true })).not.toContain('screentone')
  })

  it('keeps the cloud out of the shared rung list, which automatic runs read', () => {
    // `editor/tools.js` builds Auto clean's engine rows and the AI mask
    // brush's from ROW_ENGINES; an automatic run never reaches the cloud.
    expect(ROW_ENGINES).not.toContain(CLOUD_ENGINE)
  })
})

describe('cloud masks', () => {
  const remote = {
    id: 'c1-p001-r0',
    mask: {
      id: 'c1-p001-r0-m1',
      fillMode: 'reconstruct',
      provenance: { engine: 'flux', cloud: { provider: 'modal', request_id: 'req-123', cost: null } },
    },
  }

  it('are the legacy cloud engine, or a patch with a cloud record, and read as Cloud', () => {
    expect(isCloudMask(masked('cloud').mask)).toBe(true)
    expect(isCloudMask(remote.mask)).toBe(true)
    expect(maskEngine(remote.mask)).toBe(CLOUD_ENGINE)
    expect(isCloudMask(masked('flux').mask)).toBe(false)
    expect(maskEngine(masked('flux').mask)).toBe('flux')
    expect(maskEngine(null)).toBe(null)
  })

  // Denoise fill is gone; a patch saved by it is a fill now, as the native
  // side reads it, and the picker and the menu name it Fill.
  it('read a patch saved as the retired denoise rung as Fill', () => {
    const legacy = masked('denoise').mask
    expect(maskEngine(legacy)).toBe('fill')
    expect(ROW_ENGINES).not.toContain('denoise')
    const engine = regionMenuSections(masked('denoise')).find((section) => section.id === 'engine')
    expect(engine?.items.map((item) => item.id)).toEqual(['engine:fill', 'engine:lama'])
    expect(engine?.items.find((item) => item.selected)?.labelKey).toBe('masks.engineChoice.fill')
  })

  it('may be tried again, which asks for consent again', () => {
    expect(reRunnable(remote.mask)).toBe(true)
    expect(reRunnable(masked('cloud').mask)).toBe(true)
    expect(reRunnable(null)).toBe(false)
  })

  it('keep Cloud checked in the menu, leading the list while the cloud is not ready', () => {
    const sections = regionMenuSections(remote, { engines: { flux: false } })
    expect(ids(sections)).toEqual(['retry', 'reject', 'variationCount', 'makeVariations',
      'engine:cloud', 'engine:fill', 'engine:lama', 'delete'])
    const engines = sections.find((section) => section.id === 'engine')
    expect(engines.items.filter((item) => item.selected).map((item) => item.id)).toEqual(['engine:cloud'])
  })

  it('take Cloud in its own place once the cloud is ready', () => {
    const sections = regionMenuSections(remote, { engines: { flux: false }, cloud: true })
    expect(ids(sections)).toEqual(['retry', 'reject', 'variationCount', 'makeVariations',
      'engine:fill', 'engine:lama', 'engine:cloud', 'delete'])
  })
})

describe('the region menu\'s Variations', () => {
  const cloudResult = {
    id: 'c1-p001-r0',
    mask: { id: 'c1-p001-r0-m1', fillMode: 'reconstruct', provenance: { engine: 'flux', cloud: { provider: 'modal', request_id: 'r' } } },
  }
  /** @param {any} region @param {any} [options] */
  const variations = (region, options) => regionMenuSections(region, options).find((section) => section.id === 'variations')

  it('come right after Try again for a FLUX, cloud or Nano Banana result', () => {
    for (const region of [masked('flux'), masked('nanobanana'), cloudResult]) {
      const sections = regionMenuSections(region, { engines: { flux: true } })
      expect(sections.map((section) => section.id).slice(0, 2)).toEqual(['rerun', 'variations'])
      expect(variesEachTime(region.mask)).toBe(true)
    }
  })

  it('are absent for fill, screentone, paint and clone results, and Try again wider stays LaMa\'s', () => {
    for (const engine of ['fill', 'screentone', 'paint', 'clone']) {
      expect(variations(masked(engine), { beta: true, variations: { current: 2, count: 3 } })).toBeUndefined()
      expect(variesEachTime(masked(engine).mask)).toBe(false)
      expect(listsVariations(masked(engine).mask)).toBe(false)
    }
    expect(ids(regionMenuSections(masked('flux'))).includes('retryWider')).toBe(false)
    expect(ids(regionMenuSections(masked('lama'))).includes('retryWider')).toBe(true)
  })

  // LaMa answers one hole one way: its variations are the first clean and
  // each Try again wider, stepped through, with nothing to make n of.
  it('are a LaMa result\'s wider tries: the position row once there are two, and no Make n', () => {
    expect(variesEachTime(masked('lama').mask)).toBe(false)
    expect(listsVariations(masked('lama').mask)).toBe(true)
    expect(variations(masked('lama'))).toBeUndefined()
    expect(variations(masked('lama'), { variations: { current: 1, count: 1 } })).toBeUndefined()
    const section = variations(masked('lama'), { variations: { current: 2, count: 3 }, variationCount: 5 })
    expect(ids([section])).toEqual(['variation'])
    expect(section.items[0]).toMatchObject({ labelKey: 'masks.variations.position', stepper: { current: 2, count: 3 } })
    const all = ids(regionMenuSections(masked('lama'), { variations: { current: 2, count: 3 } }))
    expect(all.indexOf('variation')).toBe(all.indexOf('reject') + 1)
  })

  // Plain Try again on LaMa showed the same result again, so it is not offered.
  it('leave Try again to every re-runnable engine but LaMa', () => {
    expect(retriesAgain(masked('lama').mask)).toBe(false)
    expect(ids(regionMenuSections(masked('lama'))).includes('retry')).toBe(false)
    for (const engine of ['fill', 'screentone', 'flux', 'nanobanana']) {
      expect(retriesAgain(masked(engine).mask)).toBe(true)
      expect(ids(regionMenuSections(masked(engine), { beta: true }))[0]).toBe('retry')
    }
    for (const engine of ['paint', 'clone']) expect(retriesAgain(masked(engine).mask)).toBe(false)
    expect(retriesAgain(null)).toBe(false)
  })

  it('offer the count and Make n variations, and the position row once there are two', () => {
    expect(ids([variations(masked('nanobanana'))])).toEqual(['variationCount', 'makeVariations'])
    expect(ids([variations(masked('nanobanana'), { variations: { current: 1, count: 1 } })]))
      .toEqual(['variationCount', 'makeVariations'])
    const section = variations(masked('nanobanana'), { variations: { current: 2, count: 3 }, variationCount: 5 })
    expect(ids([section])).toEqual(['variation', 'variationCount', 'makeVariations'])
    const [row, count, make] = section.items
    expect(row).toMatchObject({ labelKey: 'masks.variations.position', labelParams: { current: 2, count: 3 },
      stepper: { current: 2, count: 3 } })
    expect(count.range).toEqual({ min: 2, max: 8, step: 1, value: 5, unit: '' })
    expect(make).toMatchObject({ labelKey: 'masks.variations.make', labelParams: { n: 5 } })
  })

  it('make 3 until a count is chosen, and keep the count between 2 and 8', () => {
    expect(variations(masked('flux')).items.at(-1).labelParams).toEqual({ n: 3 })
    expect(variations(masked('flux'), { variationCount: 1 }).items.at(-1).labelParams).toEqual({ n: 2 })
    expect(variations(masked('flux'), { variationCount: 20 }).items.at(-1).labelParams).toEqual({ n: 8 })
  })
})

describe('regionMenuSections', () => {
  it('offers Duplicate for regional copies only', () => {
    const region = masked('paint')
    region.mask.provenance.params_snapshot = { mode: 'region-copy' }
    expect(ids(regionMenuSections(region))).toEqual(['duplicate', 'delete'])
    expect(ids(regionMenuSections(masked('paint')))).toEqual(['delete'])
    expect(ids(regionMenuSections(masked('clone')))).toEqual(['delete'])
  })
  it('offers a detection the model list a cleaned region has, none checked, and no Clean shortcuts', () => {
    const region = { id: 'c1-p001-d0', outcome: 'detected', pick: 'lama', mask: masked('lama').mask }
    expect(ids(regionMenuSections(region, { engines: { flux: true }, cloud: true }))).toEqual([
      'engine:fill', 'engine:lama', 'engine:flux', 'engine:cloud', 'type:inside', 'type:outside', 'padding', 'splitPart', 'splitPieces', 'delete',
    ])
    expect(ids(regionMenuSections(region, { engines: { flux: false }, cloud: false }))).toEqual([
      'engine:fill', 'engine:lama', 'type:inside', 'type:outside', 'padding', 'splitPart', 'splitPieces', 'delete',
    ])
    const engine = regionMenuSections(region, { cloud: true }).find((section) => section.id === 'engine')
    expect(engine?.labelKey).toBe('masks.action.engine')
    expect(engine?.items.some((item) => item.selected)).toBe(false)
  })

  describe('a detection\'s Text type', () => {
    /** @param {boolean|null|undefined} insideBubble */
    const detection = (insideBubble) => ({
      id: 'c1-p001-d0', outcome: 'detected', pick: 'fill', insideBubble, mask: masked('fill').mask,
    })
    /** @param {any} region */
    const typeOf = (region) => regionMenuSections(region, { engines: { flux: false } })
      .find((section) => section.id === 'type')

    it('offers Split off part and Split into pieces on a detection, just above Delete', () => {
      const sections = regionMenuSections(detection(true), { engines: {} })
      const split = sections.find((section) => section.id === 'split')
      expect(split?.items).toEqual([
        { id: 'splitPart', labelKey: 'masks.action.splitPart', icon: 'mask-split' },
        { id: 'splitPieces', labelKey: 'masks.action.splitPieces', icon: 'mask-pieces' },
      ])
      expect(sections.at(-2)?.id).toBe('split')
    })

    it('sits between Clean with and Delete, named as Text cleanup names the two kinds', () => {
      const sections = regionMenuSections(detection(true), { engines: { flux: false } })
      expect(sections.map((section) => section.id)).toEqual(['engine', 'type', 'padding', 'split', 'remove'])
      const type = typeOf(detection(true))
      expect(type.labelKey).toBe('masks.action.textType')
      expect(type.items.map((item) => [item.id, item.labelKey])).toEqual([
        ['type:inside', 'tools.param.bubbleText'],
        ['type:outside', 'tools.param.outsideText'],
      ])
    })

    it('checks speech bubble text for a detection inside a bubble', () => {
      expect(typeOf(detection(true)).items.map((item) => item.selected)).toEqual([true, false])
    })

    it('checks text outside bubbles for a detection outside one', () => {
      expect(typeOf(detection(false)).items.map((item) => item.selected)).toEqual([false, true])
    })

    it('checks neither when the detection has no answer, and offers both', () => {
      for (const unknown of [null, undefined]) {
        const type = typeOf(detection(unknown))
        expect(type.items.map((item) => item.id)).toEqual(['type:inside', 'type:outside'])
        expect(type.items.map((item) => item.selected)).toEqual([false, false])
      }
    })

    it('is offered to nothing but a detection', () => {
      const others = [
        masked('lama'),
        masked('cloud'),
        { id: 'c1-p001-r1', outcome: 'declined', declineReason: 'decline.reason.histogram', mask: null },
        { id: 'c1-p001-r1', outcome: 'gate-skipped', mask: null },
        { id: 'c1-p001-r2', outcome: 'candidate', candidateInsideBubble: true, mask: null },
        { id: 'c1-p001-r3', mask: null },
      ]
      for (const region of others) {
        const offered = ids(regionMenuSections(/** @type {any} */ (region), { engines: { flux: true }, cloud: true }))
        expect(offered.filter((id) => id.startsWith('type:')), region.id).toEqual([])
      }
    })
  })

  it('offers a region the quality metric declined its models, and no Approve that would repeat the decline', () => {
    const region = { id: 'c1-p001-r1', outcome: 'declined', declineReason: 'decline.reason.histogram', mask: null }
    // Nor Approve mask: nothing held it, an engine failed on it. Restore
    // mask leads instead, the failed clean's way back to its mask.
    expect(ids(regionMenuSections(region, { engines: { flux: false }, cloud: true }))).toEqual([
      'restoreMask', 'approve:fill', 'approve:lama', 'delete',
    ])
  })

  it('offers a failed clean Restore mask, named and drawn as a cleaned result\'s is', () => {
    const region = { id: 'c1-p001-d1', outcome: 'declined', declineReason: 'decline.reason.rungUnavailable', mask: null }
    const [first] = regionMenuSections(region, { engines: { flux: false } })
    expect(first).toEqual({ id: 'restore', labelKey: null,
      items: [{ id: 'restoreMask', labelKey: 'masks.action.rejectResult', icon: 'undo' }] })
    // Only a failed clean: a held row, a detection and a cleaned layer have their own ways.
    for (const outcome of ['gate-skipped', 'candidate', 'detected']) {
      expect(ids(regionMenuSections({ ...region, outcome }, { engines: { flux: false } }))).not.toContain('restoreMask')
    }
  })

  it('does not offer Restore mask on a mask-needs-correction note, which has no failed clean behind it', () => {
    // It reads as declined, but the detection still has its mask: the
    // backend has no held row for it and would answer nothing.
    const note = { id: 'c1-p001-d1', outcome: 'declined', declineReason: 'review.reason.maskNeedsCorrection', mask: null }
    expect(isFailedClean(note)).toBe(false)
    expect(ids(regionMenuSections(note, { engines: { flux: false } }))).not.toContain('restoreMask')
    expect(reviewEntryActions(/** @type {any} */ (note)).map((action) => action.id)).not.toContain('restoreMask')
    expect(isFailedClean({ ...note, declineReason: 'decline.reason.histogram' })).toBe(true)
  })

  it('offers both approvals and available local models for a gated review item', () => {
    const region = { id: 'c1-p001-r1', outcome: 'gate-skipped', mask: null }
    expect(ids(regionMenuSections(region, { engines: { flux: false }, cloud: true }))).toEqual([
      'approveMask', 'approve', 'approve:fill', 'approve:lama', 'delete',
    ])
  })

  it('offers Approve mask to a held candidate as to a gated region, leading the menu', () => {
    const region = { id: 'c1-p001-u1-2-3-4', outcome: 'candidate', candidateInsideBubble: false, mask: null }
    const [first] = regionMenuSections(region, { engines: { flux: false } })
    expect(first.items).toEqual([
      { id: 'approveMask', labelKey: 'masks.action.approveMask' },
      { id: 'approve', labelKey: 'masks.action.approve' },
    ])
  })

  it('offers only Delete for an approved component whose sidecar cannot be replaced safely', () => {
    const region = masked('fill')
    region.id = 'c1-p001-hreview-sam-00001-deadbeef'
    region.mask.id = `${region.id}-m1`
    expect(reRunnable(region.mask)).toBe(false)
    expect(ids(regionMenuSections(region))).toEqual(['delete'])
  })

  it('offers Try again wider for a LaMa mask only, never another engine, a cloud or a text-shaped one', () => {
    const local = masked('lama')
    expect(retryWidens(local.mask)).toBe(true)
    for (const engine of ['fill', 'screentone', 'flux', 'nanobanana']) {
      expect(retryWidens(masked(engine).mask)).toBe(false)
      expect(ids(regionMenuSections(masked(engine), { engines: { flux: true, nanobanana: true } }))).not.toContain('retryWider')
    }
    expect(retryWidens(masked('cloud').mask)).toBe(false)
    expect(retryWidens({ ...local.mask, provenance: { engine: 'lama', cloud: { provider: 'modal' } } })).toBe(false)
    expect(retryWidens({ ...local.mask, geometryPolicy: 'text_shape' })).toBe(false)
    expect(retryWidens(masked('paint').mask)).toBe(false)
    expect(retryWidens(null)).toBe(false)
  })

  it('offers Try again wider, the engines and Delete for a LaMa mask, and no Try again', () => {
    const sections = regionMenuSections(masked('lama'), { engines: { flux: false } })
    expect(ids(sections)).toEqual([
      'retryWider',
      'reject',
      'engine:fill',
      'engine:lama',
      'delete',
    ])
  })

  it('checks the engine the mask actually used, and only that one', () => {
    const sections = regionMenuSections(masked('lama'), { engines: { flux: false } })
    const engines = sections.find((section) => section.id === 'engine')
    expect(engines.items.filter((item) => item.selected).map((item) => item.id)).toEqual([
      'engine:lama',
    ])
  })

  it('heads the engines with the label the row picker uses', () => {
    const sections = regionMenuSections(masked('lama'))
    expect(sections.find((section) => section.id === 'engine').labelKey).toBe('masks.action.engine')
    // The two action sections carry no heading: one entry each, and a heading
    // over a single item is a label for nothing.
    expect(sections.find((section) => section.id === 'rerun').labelKey).toBe(null)
    expect(sections.find((section) => section.id === 'remove').labelKey).toBe(null)
  })

  it('offers rung 3a only where the sidecar is installed', () => {
    expect(ids(regionMenuSections(masked('lama'), { engines: { flux: true } }))).toContain('engine:flux')
    expect(ids(regionMenuSections(masked('lama'), { engines: { flux: false } }))).not.toContain('engine:flux')
    // The default is the safe one: a machine that has not answered yet is not
    // offered a rung that cannot run.
    expect(ids(regionMenuSections(masked('lama')))).not.toContain('engine:flux')
  })

  it('leaves out a rung whose weights are gone, and still names the one in use', () => {
    // A mask cleaned with LaMa on a machine that has since deleted LaMa's
    // weights: the entry has to lead the list so the menu is not claiming the
    // region has no engine, and it must not be offered to anything else.
    const sections = regionMenuSections(masked('lama'), {
      engines: { flux: false, lama: false },
    })
    expect(ids(sections)).toEqual([
      'retryWider',
      'reject',
      'engine:lama',
      'engine:fill',
      'delete',
    ])
  })

  it('leads with the rung in use even when the picker does not offer it', () => {
    // A build that retires a rung must still name what a mask already ran on.
    const sections = regionMenuSections(masked('retired'), { engines: { flux: false } })
    const engines = sections.find((section) => section.id === 'engine')
    expect(engines.items[0]).toMatchObject({ id: 'engine:retired', selected: true })
  })

  it('still names and checks the Screentone fill on a layer it made, with the setting off', () => {
    const engines = regionMenuSections(masked('screentone'), { engines: { flux: false } })
      .find((section) => section.id === 'engine')
    expect(engines.items.map((item) => item.id)).toEqual(['engine:screentone', 'engine:fill', 'engine:lama'])
    expect(engines.items[0].selected).toBe(true)
    const on = regionMenuSections(masked('lama'), { engines: { flux: false }, beta: true })
    expect(ids(on)).toContain('engine:screentone')
  })

  it('offers Post-processing… on a layer whose post-processing can change, with its shortcut', () => {
    const sections = regionMenuSections(masked('lama'), { engines: { flux: false }, finishing: true })
    expect(ids(sections)).toEqual(['retryWider', 'reject', 'engine:fill', 'engine:lama', 'finishing', 'delete'])
    const item = sections.find((section) => section.id === 'finishing').items[0]
    expect(item).toMatchObject({ labelKey: 'masks.finishing.menu', shortcut: 'layers.finishingSelected' })
    expect(ids(regionMenuSections(masked('lama'), { engines: { flux: false } }))).not.toContain('finishing')
    // No mask, nothing to finish, whatever the caller says.
    expect(ids(regionMenuSections({ id: 'c1-p001-r1', mask: null }, { finishing: true }))).toEqual(['delete'])
  })

  it('offers Edit gradient on a gradient layer, and on no other', () => {
    const gradient = { id: 'g1', outcome: 'cleaned', mask: { provenance: { engine: 'paint', params_snapshot: { mode: 'gradient' } } } }
    expect(ids(regionMenuSections(gradient))).toEqual(['editGradient', 'delete'])
    expect(ids(regionMenuSections(masked('lama'), { engines: { flux: false } }))).not.toContain('editGradient')
  })

  it('offers Delete alone for a region with no mask, and names the region', () => {
    const sections = regionMenuSections({ id: 'c1-p001-r1', mask: null })
    expect(ids(sections)).toEqual(['delete'])
    expect(sections[0].items[0].labelKey).toBe('masks.action.deleteRegion')
  })

  it('offers Cloud last for a local mask when the cloud is ready, and not otherwise', () => {
    expect(ids(regionMenuSections(masked('lama'), { engines: { flux: false }, cloud: true }))).toEqual([
      'retryWider',
      'reject',
      'engine:fill',
      'engine:lama',
      'engine:cloud',
      'delete',
    ])
    expect(ids(regionMenuSections(masked('lama'), { engines: { flux: false }, cloud: false }))).not.toContain(
      'engine:cloud',
    )
    const engines = regionMenuSections(masked('lama'), { cloud: true }).find((section) => section.id === 'engine')
    expect(engines.items.find((item) => item.id === 'engine:cloud')).toMatchObject({
      labelKey: 'masks.engineChoice.cloud',
      selected: false,
    })
  })

  it('allows reRunnable for local FLUX masks without cloud provenance', () => {
    const localFlux = {
      id: 'c1-p001-r0',
      mask: {
        id: 'c1-p001-r0-m1',
        fillMode: 'reconstruct',
        provenance: {
          engine: 'flux',
          cloud: null,
        },
      },
    }
    expect(reRunnable(localFlux.mask)).toBe(true)
  })

  it('gives every item a key the catalogue can answer, and a unique id', () => {
    const sections = regionMenuSections(masked('lama'), { engines: { flux: true } })
    const all = ids(sections)
    expect(new Set(all).size).toBe(all.length)
    for (const section of sections) {
      for (const item of section.items) {
        expect(item.labelKey, item.id).toMatch(/^masks\.[a-zA-Z]+\.[a-zA-Z]+$/)
      }
    }
  })
})

/**
 * Reject result puts a cleaned detection back as detected, with its mask
 * (docs/fixes-2026-10-12/06). It is offered for a result an automatic clean
 * made and for nothing else, and the test reads only what every region
 * carries, so a chapter cleaned by an older build is offered it too and is
 * told why it cannot be done.
 */
describe('Reject result', () => {
  /** A result as a clean of a detection leaves it: `source: 'auto'`, the detection's id. */
  const cleaned = (engine = 'lama') => ({ ...masked(engine), id: 'c1-p001-d3', source: 'auto', detected: true, outcome: 'cleaned' })

  it('is offered beside the re-runs for a result an automatic clean made', () => {
    expect(rejectable(cleaned())).toBe(true)
    const rerun = regionMenuSections(cleaned(), { engines: { flux: false } }).find((section) => section.id === 'rerun')
    expect(rerun.items.map((item) => item.id)).toEqual(['retryWider', 'reject'])
    expect(rerun.items.at(-1)).toEqual({ id: 'reject', labelKey: 'masks.action.rejectResult', icon: 'undo' })
  })

  it('is offered for a cloud result as for a local one', () => {
    const cloud = { ...cleaned('flux') }
    cloud.mask = { ...cloud.mask, provenance: { engine: 'flux', cloud: { provider: 'modal', request_id: 'req-1', cost: null } } }
    expect(ids(regionMenuSections(cloud, { engines: { flux: false }, cloud: true }))).toContain('reject')
  })

  // Nothing on a region says whether its detection was kept, so an older
  // chapter's result is offered the entry: the refusal is what explains it.
  it('is offered for a result from before cleans kept their detection', () => {
    expect(rejectable({ ...cleaned(), id: 'c1-p001-r2' })).toBe(true)
  })

  // Restore mask makes a hand mask's result a detection from its own mask.
  it('is offered for a hand-drawn mask, and never for a paint or clone stroke', () => {
    const hand = { ...cleaned(), id: 'c1-p001-h1', source: 'hand' }
    expect(rejectable(hand)).toBe(true)
    expect(ids(regionMenuSections(hand, { engines: { flux: false } }))).toEqual([
      'retryWider', 'reject', 'engine:fill', 'engine:lama', 'delete',
    ])
    for (const engine of ['paint', 'clone']) {
      expect(rejectable({ ...cleaned(engine), source: 'auto' })).toBe(false)
      expect(ids(regionMenuSections({ ...cleaned(engine), source: 'auto' }))).not.toContain('reject')
    }
  })

  it('is not offered for a detected region, which has no result yet', () => {
    const detected = { ...cleaned(), outcome: 'detected', insideBubble: true }
    expect(rejectable(detected)).toBe(false)
    expect(ids(regionMenuSections(detected, { engines: { flux: false } }))).not.toContain('reject')
  })

  it('is not offered for a region with no mask', () => {
    for (const outcome of ['pending', 'declined', 'gate-skipped', 'candidate']) {
      const bare = { id: 'c1-p001-d3', source: 'auto', outcome, mask: null }
      expect(rejectable(bare), outcome).toBe(false)
      expect(ids(regionMenuSections(bare, { engines: { flux: false } })), outcome).not.toContain('reject')
    }
    expect(rejectable(null)).toBe(false)
  })

  it('is not offered for an approved review component, which has no detection behind it', () => {
    const component = cleaned('fill')
    component.id = 'c1-p001-hreview-sam-00001-deadbeef'
    component.mask = { ...component.mask, id: `${component.id}-m1` }
    expect(rejectable(component)).toBe(false)
  })
})

/**
 * A paint or clone stroke is copied pixels, not a cleaning: there is nothing
 * to run again from the layers below it (docs/repeated-inpaint-plan.md, M1
 * item 5). Try again and Clean with are refused for it everywhere they are
 * offered, and the layer keeps the name of the tool that made it rather than
 * borrowing an engine's.
 */
describe('paint and clone masks', () => {
  for (const engine of ['paint', 'clone']) {
    it(`refuse Try again and every engine for a ${engine} stroke, and keep Delete`, () => {
      const region = masked(engine)
      expect(reRunnable(region.mask)).toBe(false)
      // Whatever the machine could run and whether the cloud is ready: an
      // engine list would be an offer to re-clean something never cleaned.
      for (const options of [{}, { engines: { flux: true } }, { engines: { flux: true }, cloud: true }]) {
        const sections = regionMenuSections(region, options)
        expect(ids(sections)).toEqual(['delete'])
        expect(sections.map((section) => section.id)).toEqual(['remove'])
      }
    })

    it(`deletes a ${engine} stroke as a layer, not as a region`, () => {
      const [remove] = regionMenuSections(masked(engine))
      expect(remove.items[0]).toMatchObject({ id: 'delete', labelKey: 'masks.action.delete' })
    })

    it(`names a ${engine} stroke by the tool that made it, and never as Cloud`, () => {
      const { mask } = masked(engine)
      expect(maskEngine(mask)).toBe(engine)
      expect(isCloudMask(mask)).toBe(false)
      expect(rungLabel(/** @type {string} */ (maskEngine(mask)))).toBe(`ladder.rung.${engine}`)
    })
  }
})

describe('provenanceFacts', () => {
  it('retains cloudCost as null when cost is null so UI displays unknown dash', () => {
    const maskWithNullCost = {
      fillMode: 'reconstruct',
      elapsedMs: 2500,
      provenance: {
        engine: 'flux',
        engine_version: 'flux-sdnq-v1',
        cloud: {
          provider: 'beam',
          request_id: 'req-456',
          cost: null,
        },
      },
    }
    const facts = provenanceFacts(maskWithNullCost)
    expect(facts).toContainEqual({ key: 'masks.provenance.cloudCost', value: null })
    expect(facts).toContainEqual({ key: 'masks.provenance.cloudRequestId', value: 'req-456' })
  })

  it('includes cloudCost when cost is a valid positive number', () => {
    const maskWithCost = {
      fillMode: 'reconstruct',
      elapsedMs: 1800,
      provenance: {
        engine: 'flux',
        engine_version: 'flux-sdnq-v1',
        cloud: {
          provider: 'modal',
          request_id: 'req-789',
          cost: 0.025,
        },
      },
    }
    const facts = provenanceFacts(maskWithCost)
    expect(facts).toContainEqual({ key: 'masks.provenance.cloudCost', value: 0.025 })
    expect(facts).toContainEqual({ key: 'masks.provenance.cloudRequestId', value: 'req-789' })
  })

  it('omits cloud facts entirely for local masks', () => {
    const localMask = {
      fillMode: 'match-surround',
      elapsedMs: 30,
      provenance: {
        engine: 'fill',
        engine_version: 'planar-1.4',
        cloud: null,
      },
    }
    const facts = provenanceFacts(localMask)
    expect(facts.map((f) => f.key)).not.toContain('masks.provenance.cloudCost')
    expect(facts.map((f) => f.key)).not.toContain('masks.provenance.cloudRequestId')
  })
})

describe('the colour a mask is drawn in', () => {
  const colors = { maskColor: '#0284c7', outsideMaskColor: '#c2410c' }
  /** @returns {any} */
  const detection = (inside, bbox = { x: 10, y: 10, w: 10, h: 10 }) => ({
    id: `d-${inside}-${bbox.x}`,
    outcome: 'detected',
    bbox,
    insideBubble: inside,
  })

  it('draws a detection in the colour of where its text sits', () => {
    expect(maskColorFor(detection(true), colors)).toBe('#0284c7')
    expect(maskColorFor(detection(false), colors)).toBe('#c2410c')
  })

  it('reads a held candidate by its own balloon answer', () => {
    expect(maskColorFor(/** @type {any} */ ({ outcome: 'candidate', candidateInsideBubble: true }), colors)).toBe('#0284c7')
    expect(maskColorFor(/** @type {any} */ ({ outcome: 'candidate', candidateInsideBubble: false }), colors)).toBe('#c2410c')
  })

  it('draws a region of unknown place in the speech bubble colour, as the single colour was', () => {
    for (const region of [
      null,
      undefined,
      { outcome: 'cleaned' },
      { outcome: 'detected' },
      { outcome: 'detected', insideBubble: null },
      { outcome: 'candidate', candidateInsideBubble: null },
      { outcome: 'detected', insideBubble: 'no' },
    ]) {
      expect(maskColorFor(/** @type {any} */ (region), colors), JSON.stringify(region)).toBe('#0284c7')
      if (region) expect(regionInsideBubble(/** @type {any} */ (region))).toBeNull()
    }
  })

  it('previews a selection gesture in the colour of the detection it overlaps most', () => {
    const inBubble = detection(true, { x: 10, y: 10, w: 10, h: 10 })
    const outside = detection(false, { x: 30, y: 10, w: 10, h: 10 })
    const regions = [inBubble, outside]
    // Mostly over the outside one, a little over the bubble one.
    expect(draftMaskColor(regions, { x: 18, y: 12, w: 20, h: 4 }, colors)).toBe('#c2410c')
    expect(draftMaskColor(regions, { x: 12, y: 12, w: 4, h: 4 }, colors)).toBe('#0284c7')
    // Over no detection: a new hand area, stored as outside text.
    expect(draftMaskColor(regions, { x: 60, y: 60, w: 5, h: 5 }, colors)).toBe('#c2410c')
    expect(draftMaskColor(regions, null, colors)).toBe('#c2410c')
    // Only a detection is joined: a cleaned layer under the gesture is not.
    const layer = /** @type {any} */ ({ id: 'l1', outcome: 'cleaned', bbox: { x: 60, y: 60, w: 10, h: 10 }, insideBubble: true })
    expect(draftMaskColor([layer], { x: 60, y: 60, w: 5, h: 5 }, colors)).toBe('#c2410c')
  })
})

describe('Generations per region', () => {
  it('is a whole number from 1 to 8, and 1 for anything else', () => {
    for (const count of [1, 2, 8]) expect(generationsOf(count)).toBe(count)
    for (const bad of [0, 9, 2.5, '3', null, undefined, NaN]) expect(generationsOf(bad), String(bad)).toBe(1)
  })

  it('is made by a Nano Banana batch and a run here with a FLUX pick, and says why not elsewhere', () => {
    expect(generationsReasonKey('nanoBanana', ['fill', 'lama'])).toBeNull()
    expect(generationsReasonKey('local', ['fill', 'flux'])).toBeNull()
    expect(generationsReasonKey('local', ['ladder.rung.flux', 'lama'])).toBeNull()
    expect(generationsReasonKey('local', ['fill', 'lama'])).toBe('tools.target.generationsOneWay')
    expect(generationsReasonKey('local', ['solid'])).toBe('tools.target.generationsOneWay')
    expect(generationsReasonKey('cloud', ['flux', 'flux'])).toBeNull()
    expect(generationsReasonKey('cloud', ['fill', 'lama'])).toBeNull()
  })
})
