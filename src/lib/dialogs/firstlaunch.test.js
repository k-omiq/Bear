/**
 * The pure half of onboarding: what is on disk, and what a set of choices
 * needs from it. The store and the run are pinned in
 * `OnboardingScreen.dom.test.js`.
 */

import { describe, expect, it } from 'vitest'
import {
  DEFAULT_FLUX_MODEL,
  FIRST_LAUNCH_STEPS,
  RUNTIME_ID,
  defaultFluxModel,
  firstLaunchPlan,
  labelKeyFor,
  missingBytes,
  neededFiles,
  runtimeNotice,
  runtimeReady,
} from './firstlaunch.js'
import { filesFor } from '../model/pipelines.js'

/** @param {Record<string, boolean>} installed */
function view(installed = {}, runtime = { installed: false, bytes: 30, available: true }) {
  if (runtime === null) return { hasToken: false, models: [] }
  const row = (id, bytes) => ({ id, kindKey: `models.kind.${id}`, bytes, installed: installed[id] === true })
  return {
    hasToken: false,
    models: [
      row('textDetector', 95),
      row('inpainter', 207),
      row('scriptGate', 4),
      row('scriptGateLabels', 1),
      row('balloonDetector', 11),
      row('ocrEncoder', 343),
      row('ocrDecoder', 117),
      row('ocrVocab', 1),
      row('hayaiVision', 344),
      row('hayaiDecoder', 256),
      row('hayaiTokenizer', 2),
    ],
    runtime: { version: '1.28.0', flavour: 'stock', ...runtime },
  }
}

const ALL = { ja: 'ctd-rtdetr', zh: 'ctd-rtdetr', ko: 'ctd-rtdetr' }

describe('the plan', () => {
  it('holds every catalogue row and the runtime', () => {
    const plan = firstLaunchPlan(view())
    expect(Object.keys(plan.files)).toContain(RUNTIME_ID)
    expect(plan.files.inpainter).toEqual({ id: 'inpainter', labelKey: 'models.kind.inpainter', bytes: 207, installed: false })
  })

  it('leaves out a runtime this platform has no build for, and says so', () => {
    const plan = firstLaunchPlan(view({}, { available: false, installed: false, bytes: null }))
    expect(plan.files[RUNTIME_ID]).toBeUndefined()
    expect(plan.runtimeUnavailable).toBe(true)
  })

  it('survives a view that answered nothing', () => {
    const plan = firstLaunchPlan(null)
    expect(plan.files).toEqual({})
    expect(plan.runtimeUnavailable).toBe(false)
  })

  it('charges nothing for a size the view cannot state', () => {
    const plan = firstLaunchPlan(view({}, { available: true, installed: false, bytes: null }))
    expect(plan.files[RUNTIME_ID].bytes).toBe(0)
  })

  it('names a file so a failure can say which one it was', () => {
    const plan = firstLaunchPlan(view())
    expect(labelKeyFor(plan, 'scriptGate')).toBe('models.kind.scriptGate')
    expect(labelKeyFor(plan, 'nope')).toBeNull()
  })
})

describe('what the choices need', () => {
  it('fetches the runtime first, then the detection files, then the cleaner', () => {
    const plan = firstLaunchPlan(view())
    expect(neededFiles(plan, ALL, { 'lama-manga': true })).toEqual([
      RUNTIME_ID, 'textDetector', 'balloonDetector', 'scriptGate', 'scriptGateLabels', 'inpainter',
    ])
  })

  it('adds the text reader only when a stored choice asks for it', () => {
    const files = filesFor({ ...ALL, ja: 'ctd-rtdetr-ocr' }, {})
    expect(files).toEqual(expect.arrayContaining(['hayaiVision', 'hayaiDecoder', 'hayaiTokenizer']))
    expect(files).not.toContain('ocrEncoder')
    expect(filesFor(ALL, {})).not.toContain('hayaiVision')
  })

  // Setup's Detect on: Cloud GPU. That run uses CTD, Full, SAM-TS-L and the
  // reader whatever this computer selected; CTD and the reader run here, so
  // they and the gate (legacy only) are fetched, never Small, Full or SAM-TS-L.
  it('fetches only CTD, the gate under legacy, and the text reader for detection on the cloud GPU', () => {
    const plan = firstLaunchPlan(view())
    const cloud = { rtFull: 'cloud', samTs: 'cloud' }
    for (const detectorModels of [['ctd', 'rtSmall'], ['rtSmall', 'samTs'], ['ctd', 'rtFull', 'samTs']]) {
      expect(neededFiles(plan, ALL, { 'lama-manga': true }, { detectorModels, analysisTargets: cloud, ocrRescue: false })).toEqual([
        RUNTIME_ID, 'textDetector', 'scriptGate', 'scriptGateLabels', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer', 'inpainter',
      ])
      expect(neededFiles(plan, ALL, {}, { textPolicy: 'all_text', detectorModels, analysisTargets: cloud })).toEqual([
        RUNTIME_ID, 'textDetector', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer',
      ])
    }
    // This computer again: the user's own models, and no reader unless asked.
    expect(neededFiles(plan, ALL, {}, { detectorModels: ['ctd', 'rtSmall'], analysisTargets: { rtFull: 'local', samTs: 'local' } }))
      .toEqual([RUNTIME_ID, 'textDetector', 'balloonDetector', 'scriptGate', 'scriptGateLabels'])
  })

  it('skips every detection file when every language is skipped', () => {
    expect(filesFor({ ja: null, zh: null, ko: null }, { 'lama-manga': true })).toEqual(['inpainter'])
  })

  it('never fetches for an engine that is not ready', () => {
    expect(filesFor({ ja: 'rtdetr' }, { 'big-lama': true, 'flux2-klein-4b': true })).toEqual([])
  })

  it('costs only what is missing and has not arrived since', () => {
    const plan = firstLaunchPlan(view({ textDetector: true }))
    const ids = neededFiles(plan, ALL, { 'lama-manga': true })
    expect(missingBytes(plan, ids)).toBe(30 + 11 + 4 + 1 + 207)
    expect(missingBytes(plan, ids, { inpainter: true })).toBe(30 + 11 + 4 + 1)
  })
})

describe('the setup around the plan', () => {
  it('walks eleven steps, with denoise right before cloud, then the community, and downloads last', () => {
    expect(FIRST_LAUNCH_STEPS).toEqual([
      'welcome', 'theme', 'token', 'background', 'detection', 'cleaning', 'denoise', 'cloud', 'community', 'dependencies', 'downloads',
    ])
  })

  it('carries the platform the backend reported, and what the runtime build needs by hand', () => {
    const plan = firstLaunchPlan(view({}, {
      installed: false,
      bytes: 30,
      available: true,
      platform: 'windows-x64',
      flavour: 'cuda12',
      flavours: [{ id: 'cuda12', userInstalled: ['CUDA 12', 'cuDNN 9'] }, { id: 'directml', userInstalled: [] }],
    }))
    expect(plan.platform).toBe('windows-x64')
    expect(plan.runtime.needs).toEqual(['CUDA 12', 'cuDNN 9'])
    expect(firstLaunchPlan(view()).runtime.needs).toEqual([])
    expect(firstLaunchPlan(null).platform).toBeNull()
  })

  it('asks the runtime what it can run on only once it is here and not being replaced', () => {
    const plan = firstLaunchPlan(view())
    expect(runtimeReady(plan, {}, null)).toBe(false)
    expect(runtimeReady(plan, { [RUNTIME_ID]: true }, 'textDetector')).toBe(true)
    const here = firstLaunchPlan(view({}, { installed: true, bytes: 30, available: true }))
    expect(runtimeReady(here, {}, null)).toBe(true)
    expect(runtimeReady(here, {}, RUNTIME_ID)).toBe(false)
    expect(runtimeReady(firstLaunchPlan(view({}, { installed: false, bytes: null, available: false })), {}, null)).toBe(false)
    // An adapter older than the runtime row is answered the way capabilities answers it.
    expect(runtimeReady(firstLaunchPlan(view({}, null)), {}, null)).toBe(true)
    expect(runtimeReady(null, {}, null)).toBe(false)
  })

  it('reads the runtime\'s refusals and nothing that merely looks like a key', () => {
    expect(runtimeNotice('notice.runtime.noSpace needed=253000000 free=1200000 junk =3 bad=x')).toEqual({
      key: 'notice.runtime.noSpace',
      params: { needed: 253_000_000, free: 1_200_000 },
    })
    expect(runtimeNotice('notice.runtime.inUse')).toEqual({ key: 'notice.runtime.inUse', params: {} })
    expect(runtimeNotice('settings.models.status.failed')).toBe(null)
    expect(runtimeNotice('connection reset')).toBe(null)
    expect(runtimeNotice(undefined)).toBe(null)
  })

  it('lands the AI redraw model on Settings\' first choice', () => {
    expect(defaultFluxModel([{ id: 'other' }, { id: DEFAULT_FLUX_MODEL }])).toBe(DEFAULT_FLUX_MODEL)
    expect(defaultFluxModel([{ id: 'other' }])).toBe('other')
    expect(defaultFluxModel([])).toBe(null)
    expect(defaultFluxModel(undefined)).toBe(null)
  })
})
