/**
 * The gating rule, and only the rule.
 *
 * `featuresFrom` is the whole of what decides which engines the editor offers
 * on a machine, and it is pure - the catalogue's `requiredBy` in, a
 * `{feature: boolean}` map out. `loadCapabilities` around it is two awaits and
 * a spread; what could actually be wrong is the intersection.
 */

import { describe, expect, it } from 'vitest'

import {
  capabilities, featuresFrom, loadCapabilities, selectedWorkflowAvailable, selectedWorkflowMissing,
} from './capabilities.svelte.js'

/** @param {Partial<import('../api/backend.js').Backend>} answers */
function backendThat(answers) {
  return /** @type {any} */ ({
    sidecarAvailable: async () => ({ available: false, reasonKey: null }),
    listModels: async () => ({ models: [], runtime: { installed: true } }),
    ...answers,
  })
}

const CATALOGUE = [
  { id: 'textDetector', requiredBy: ['autoClean'], installed: true },
  { id: 'inpainter', requiredBy: ['lama'], installed: true },
  { id: 'scriptGate', requiredBy: ['autoClean'], installed: true },
  { id: 'scriptGateLabels', requiredBy: ['autoClean'], installed: true },
  { id: 'balloonDetector', requiredBy: ['autoClean'], installed: true },
]

/** @param {string[]} missing */
function without(missing) {
  return CATALOGUE.map((row) => ({ ...row, installed: !missing.includes(row.id) }))
}

describe('featuresFrom', () => {
  it('says yes to a feature whose every file is present', () => {
    expect(featuresFrom(CATALOGUE)).toEqual({ autoClean: true, lama: true })
  })

  /**
   * The gate model and its labels must match, so one without the other is not
   * a gate - and Auto clean needs three separate downloads, any one of which
   * takes it down.
   */
  it('takes a feature down when any one of its files is missing', () => {
    for (const id of ['textDetector', 'scriptGate', 'scriptGateLabels', 'balloonDetector']) {
      expect(featuresFrom(without([id])).autoClean, id).toBe(false)
    }
    expect(featuresFrom(without(['inpainter'])).lama).toBe(false)
    // And it takes down only what it names.
    expect(featuresFrom(without(['inpainter'])).autoClean).toBe(true)
    expect(featuresFrom(without(['textDetector'])).lama).toBe(true)
  })

  it('names no feature no row asks for - fill needs nothing', () => {
    const features = featuresFrom(CATALOGUE)
    expect(features).not.toHaveProperty('fill')
    expect(features).not.toHaveProperty('flux')
  })

  it('answers an empty catalogue with an empty map rather than throwing', () => {
    expect(featuresFrom([])).toEqual({})
    expect(featuresFrom(undefined)).toEqual({})
  })
})

describe('selected workflow readiness', () => {
  const rows = [
    ...CATALOGUE,
    { id: 'fullRt', installed: false, requiredBy: ['review'] },
  ]
  const choices = (detectorModels, textPolicy = 'legacy_gate') => ({
    detectorModels, textPolicy,
    detection: { ja: 'ctd-rtdetr', zh: null, ko: null }, ocrRescue: false,
  })

  it('does not require unselected full RT, CTD, small RT, or gate files', () => {
    expect(selectedWorkflowAvailable(rows, { fullRtInstalled: false }, choices(['ctd', 'rtSmall']))).toBe(true)
    expect(selectedWorkflowAvailable(without(['textDetector', 'balloonDetector', 'scriptGate', 'scriptGateLabels']),
      { fullRtInstalled: true }, choices(['rtFull'], 'all_text'))).toBe(true)
    expect(selectedWorkflowAvailable(without(['textDetector', 'balloonDetector']),
      { samInstalled: true }, choices(['samTs'], 'all_text'))).toBe(true)
  })

  it('requires every selected graph and the legacy gate only under legacy policy', () => {
    expect(selectedWorkflowAvailable(rows, { fullRtInstalled: false }, choices(['rtFull'], 'all_text'))).toBe(false)
    expect(selectedWorkflowAvailable(rows, { samInstalled: false }, choices(['samTs'], 'all_text'))).toBe(false)
    expect(selectedWorkflowAvailable(without(['scriptGate']), null, choices(['ctd']))).toBe(false)
    expect(selectedWorkflowAvailable(without(['scriptGate']), null, choices(['ctd'], 'all_text'))).toBe(true)
  })

  // The panel names what is missing, so a run blocked on SAM-TS-L never
  // reads as one blocked on the LaMa a clean uses.
  it('names each missing model, and none that runs on the cloud GPU', () => {
    const both = { ...choices(['rtFull', 'samTs'], 'all_text') }
    const caps = { fullRtInstalled: false, samInstalled: false }
    expect(selectedWorkflowMissing(rows, caps, both)).toEqual(['rtFull', 'samTs'])
    // A cloud run needs only CTD and the text reader here, whatever this
    // computer selected; the reader is not installed in this catalogue.
    const cloud = { analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } }
    expect(selectedWorkflowMissing(rows, caps, { ...both, ...cloud })).toEqual(['hayaiOcr'])
    expect(selectedWorkflowMissing(without(['textDetector']), caps, { ...choices(['rtSmall'], 'all_text'), ...cloud }))
      .toEqual(['ctd', 'hayaiOcr'])
    // A split record settles on this computer.
    expect(selectedWorkflowMissing(rows, caps,
      { ...both, analysisTargets: { rtFull: 'local', samTs: 'cloud' } })).toEqual(['rtFull', 'samTs'])
  })
})

describe('loadCapabilities', () => {
  it('offers the rungs whose weights are here and no others', async () => {
    await loadCapabilities(
      backendThat({
        listModels: async () => ({
          models: without(['inpainter']),
          runtime: { installed: true },
        }),
      }),
    )
    expect(capabilities.engines).toEqual({
      fill: true,
      lama: false,
      flux: false,
    })
    expect(capabilities.autoClean).toBe(true)
    expect(capabilities.runtime).toBe(true)
  })

  it('offers rung 3a only where the sidecar answers yes', async () => {
    await loadCapabilities(
      backendThat({ sidecarAvailable: async () => ({ available: true, reasonKey: null }) }),
    )
    expect(capabilities.sidecar).toBe(true)
    expect(capabilities.engines.flux).toBe(true)
  })

  it('reports a missing runtime and a missing Auto clean separately', async () => {
    await loadCapabilities(
      backendThat({
        listModels: async () => ({
          models: without(['textDetector']),
          runtime: { installed: false },
        }),
      }),
    )
    expect(capabilities.autoClean).toBe(false)
    expect(capabilities.runtime).toBe(false)
    // The inpainter is still here, so a per-region clean is still offered.
    expect(capabilities.engines.lama).toBe(true)
  })

  /**
   * A backend too old to answer `listModels` is far more likely than a machine
   * with no models at all, and hiding every engine on a failed call would be a
   * worse failure than offering one that reports a missing file.
   */
  it('offers everything but rung 3a when the catalogue cannot be read', async () => {
    await loadCapabilities(
      backendThat({
        listModels: async () => {
          throw new Error('no such command')
        },
      }),
    )
    expect(capabilities.engines).toEqual({
      fill: true,
      lama: true,
      flux: false,
    })
    expect(capabilities.autoClean).toBe(true)
    expect(capabilities.runtime).toBe(true)
  })
})
