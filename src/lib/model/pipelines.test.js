/**
 * The pure half of the capability graph: what a set of choices needs, what a
 * retired choice meant, and what a removal stops. The mounted halves are
 * `SettingsDialog.models.dom.test.js` and `FirstLaunchDialog.dom.test.js`.
 */

import { describe, expect, it } from 'vitest'
import {
  ALL_TEXT_POLICY,
  CAPABILITIES,
  DETECTORS,
  LEGACY_POLICY,
  MODELS,
  HAYAI_FILES,
  OCR_FILES,
  SCRIPT_GATE_FILES,
  detectorsFor,
  filesFor,
  migrateDetectorChoice,
  model,
  modelOfFile,
  removalImpact,
  rescueRuns,
  runtimeState,
  usedNow,
  workflowNeeds,
  workflowForDetectorModels,
  cloudCapabilitiesFor,
  localDetectorModels,
  normalizeAnalysisTargets,
  DEFAULT_DETECTOR_MODELS,
  DETECTOR_CHOICES,
  detectionPlacement,
  detectsOnCloud,
  CLOUD_DETECTOR_MODELS,
  runDetection,
  groupOfModel,
  normalizeDetectorModels,
  runsOnCloud,
  toggleDetectorModel,
  unifyAnalysisTargets,
} from './pipelines.js'
import { DETECTOR_MODEL_NAMES } from './model-names.js'

const ALL = { ja: 'ctd-rtdetr', zh: 'ctd-rtdetr', ko: 'ctd-rtdetr' }
const NONE = { ja: null, zh: null, ko: null }

describe('the per-language detector choices', () => {
  it('offer no OCR variant: the rescue is its own switch', () => {
    for (const language of ['ja', 'zh', 'ko']) {
      expect(detectorsFor(language).map((engine) => engine.id)).toEqual(['ctd-rtdetr'])
    }
    expect(DETECTORS.flatMap((engine) => engine.files)).not.toEqual(expect.arrayContaining([...OCR_FILES]))
  })

  it('never carry the script gate or OCR inside a detector', () => {
    for (const engine of DETECTORS) {
      for (const file of [...SCRIPT_GATE_FILES, ...OCR_FILES]) expect(engine.files).not.toContain(file)
    }
  })

  it('read a retired ctd-rtdetr-ocr row as the plain detector with rescue on, for Japanese only', () => {
    expect(migrateDetectorChoice('ja', 'ctd-rtdetr-ocr')).toEqual({ detector: 'ctd-rtdetr', ocrRescue: true })
    expect(migrateDetectorChoice('ko', 'ctd-rtdetr-ocr')).toEqual({ detector: undefined, ocrRescue: false })
    expect(migrateDetectorChoice('ja', 'ctd-rtdetr')).toEqual({ detector: 'ctd-rtdetr', ocrRescue: false })
    expect(migrateDetectorChoice('ja', null)).toEqual({ detector: null, ocrRescue: false })
    expect(migrateDetectorChoice('ja', 'nonsense')).toEqual({ detector: undefined, ocrRescue: false })
    expect(migrateDetectorChoice('ja', 42)).toEqual({ detector: undefined, ocrRescue: false })
  })
})

describe('what legacy filtering downloads', () => {
  it('is the detector pair and the script gate, in download order, and no OCR by default', () => {
    expect(filesFor(ALL, { 'lama-manga': true })).toEqual([
      'textDetector', 'balloonDetector', 'scriptGate', 'scriptGateLabels', 'inpainter',
    ])
    expect(filesFor(ALL, {}, { textPolicy: LEGACY_POLICY, ocrRescue: false })).not.toContain('ocrEncoder')
  })

  it('adds the three text reader files when the switch is on, for any cleaned language, and never manga-ocr', () => {
    expect(filesFor(ALL, {}, { ocrRescue: true })).toEqual([
      'textDetector', 'balloonDetector', 'scriptGate', 'scriptGateLabels', ...HAYAI_FILES,
    ])
    // The reader reads Chinese and Korean too, so skipping Japanese keeps it.
    expect(filesFor({ ...ALL, ja: null }, {}, { ocrRescue: true })).toEqual(expect.arrayContaining([...HAYAI_FILES]))
    expect(filesFor(ALL, {}, { ocrRescue: true })).not.toContain('ocrEncoder')
    expect(rescueRuns({ ...ALL, ja: null }, true)).toBe(true)
    expect(rescueRuns(ALL, true)).toBe(true)
    expect(rescueRuns(ALL, false)).toBe(false)
  })

  it('still honours a stored retired row until it is migrated', () => {
    expect(filesFor({ ...ALL, ja: 'ctd-rtdetr-ocr' }, {})).toEqual(expect.arrayContaining([...HAYAI_FILES]))
  })

  it('needs nothing for detection once every language is skipped, rescue or not', () => {
    expect(filesFor(NONE, { 'lama-manga': true }, { ocrRescue: true })).toEqual(['inpainter'])
    expect(workflowNeeds({ textPolicy: LEGACY_POLICY, detection: NONE, ocrRescue: true })).toEqual([])
  })
})

describe('what all-text review downloads', () => {
  it('maps each valid model combination to the matching review preset', () => {
    expect(workflowForDetectorModels(['ctd'])).toBe('ctd')
    expect(workflowForDetectorModels(['rtFull'])).toBe('regions')
    expect(workflowForDetectorModels(['samTs'])).toBe('mask')
    expect(workflowForDetectorModels(['ctd', 'samTs'])).toBe('ctd_mask')
    expect(workflowForDetectorModels(['ctd', 'rtFull', 'samTs'])).toBe('ctd_text_shape')
  })
  it('downloads only the selected detector models for automatic cleaning', () => {
    expect(filesFor(ALL, {}, { textPolicy: ALL_TEXT_POLICY, detectorModels: ['ctd'] })).toEqual(['textDetector'])
    expect(filesFor(ALL, {}, { textPolicy: ALL_TEXT_POLICY, detectorModels: ['rtFull'] })).toEqual(['fullRt'])
    expect(workflowNeeds({ textPolicy: ALL_TEXT_POLICY, detectorModels: ['samTs'] })).toEqual(['samTs'])
  })
  it('is the speech bubble finder, plus the text reader when it is on: never the gate labels, never any manga-ocr file', () => {
    for (const detection of [ALL, NONE]) {
      expect(filesFor(detection, { 'lama-manga': true }, { textPolicy: ALL_TEXT_POLICY, ocrRescue: false }))
        .toEqual(['balloonDetector', 'inpainter'])
      expect(filesFor(detection, { 'lama-manga': true }, { textPolicy: ALL_TEXT_POLICY, ocrRescue: true }))
        .toEqual(['balloonDetector', ...HAYAI_FILES, 'inpainter'])
    }
    expect(filesFor({ ...ALL, ja: 'ctd-rtdetr-ocr' }, {}, { textPolicy: ALL_TEXT_POLICY }))
      .toEqual(['balloonDetector', ...HAYAI_FILES])
  })

  it('needs the SAM-TS-L graphs as an import, not a download', () => {
    expect(workflowNeeds({ textPolicy: ALL_TEXT_POLICY })).toEqual(['rtSmall', 'samTs'])
    expect(model('samTs')?.source).toBe('import')
    expect(model('rtFull')?.source).toBe('download')
    expect(model('samTs')?.files).toEqual([])
  })
})

describe('the engine runtime a workflow runs on', () => {
  const RUNTIME = { installed: true, available: true, downloading: false }

  it('decides nothing for a workflow that runs nothing', () => {
    const needs = workflowNeeds({ detection: NONE })
    expect(runtimeState({ ...RUNTIME, installed: false }, needs)).toBe('notNeeded')
    expect(runtimeState(RUNTIME, needs, 'failed')).toBe('notNeeded')
  })

  it('is needed by every workflow that runs a model, legacy or all-text', () => {
    for (const choices of [{ detection: ALL }, { textPolicy: ALL_TEXT_POLICY }]) {
      const needs = workflowNeeds(choices)
      expect(runtimeState(RUNTIME, needs, 'loaded')).toBe('installed')
      expect(runtimeState({ ...RUNTIME, installed: false }, needs, 'loaded')).toBe('missing')
    }
  })

  it('tells a download in flight and a computer with no build apart from plain missing', () => {
    const needs = workflowNeeds({ detection: ALL })
    expect(runtimeState({ ...RUNTIME, installed: false, downloading: true }, needs)).toBe('downloading')
    expect(runtimeState({ ...RUNTIME, installed: false, available: false }, needs)).toBe('unavailable')
    // An installed runtime runs whether or not a newer build is offered.
    expect(runtimeState({ ...RUNTIME, available: false }, needs, 'loaded')).toBe('installed')
    // No row at all is not a runtime that is here.
    expect(runtimeState(null, needs)).toBe('missing')
  })

  // The catalogue's `installed` is a file found. A run also loads it, and a
  // CUDA build without CUDA, or a quarantined library, is found and refused.
  it('counts an installed runtime as installed only once it has loaded', () => {
    const needs = workflowNeeds({ detection: ALL })
    expect(runtimeState(RUNTIME, needs, 'failed')).toBe('unloadable')
    expect(runtimeState(RUNTIME, needs, 'checking')).toBe('checking')
    expect(runtimeState(RUNTIME, needs, 'unchecked')).toBe('unchecked')
    // No answer at all is a question not yet asked, never a pass.
    expect(runtimeState(RUNTIME, needs)).toBe('checking')
    // A load answer about a runtime that is not here changes nothing.
    expect(runtimeState({ ...RUNTIME, installed: false }, needs, 'failed')).toBe('missing')
    expect(runtimeState({ ...RUNTIME, installed: false, downloading: true }, needs, 'failed')).toBe('downloading')
  })
})

describe('the capability graph', () => {
  it('draws the detection choices, the collapsed filtering and the redraw, every model once', () => {
    expect(CAPABILITIES.map((section) => section.id)).toEqual(['detect', 'filtering', 'rebuild'])
    expect(CAPABILITIES.map((section) => section.group)).toEqual(['detection', 'filtering', 'cleaning'])
    expect(CAPABILITIES.find((section) => section.id === 'detect')?.models).toEqual([...DETECTOR_CHOICES])
    const drawn = CAPABILITIES.flatMap((section) => section.models)
    expect([...drawn].sort()).toEqual(MODELS.map((entry) => entry.id).sort())
    expect(groupOfModel('samTs')).toBe('detection')
    expect(groupOfModel('mangaOcr')).toBe('filtering')
    expect(groupOfModel('lama')).toBe('cleaning')
    expect(groupOfModel('coo')).toBeNull()
  })

  it('names each detection choice by its product, the same name every screen uses', () => {
    for (const id of DETECTOR_CHOICES) expect(model(id)?.product).toBe(DETECTOR_MODEL_NAMES[/** @type {keyof typeof DETECTOR_MODEL_NAMES} */ (id)])
    expect(Object.values(DETECTOR_MODEL_NAMES).join(' ')).not.toMatch(/RT-DETR|\u2014/)
  })

  it('claims every catalogue file exactly once', () => {
    const files = MODELS.flatMap((entry) => entry.files)
    expect(new Set(files).size).toBe(files.length)
    expect(modelOfFile('scriptGateLabels')?.id).toBe('scriptGate')
    expect(modelOfFile('ocrVocab')?.id).toBe('mangaOcr')
    expect(modelOfFile('somethingNew')).toBeNull()
  })

  it('has no row for COO: a choice that can never run is not drawn as a model', () => {
    expect(model('coo')).toBeNull()
    expect(removalImpact('coo')).toBeNull()
    expect(workflowNeeds({ textPolicy: ALL_TEXT_POLICY })).not.toContain('coo')
  })
})

describe('what a removal stops', () => {
  it('names legacy cleaning and the review small profile for the small RT-DETR', () => {
    expect(removalImpact('rtSmall')?.disables).toEqual(['legacy', 'reviewSmall'])
    expect(removalImpact('balloonDetector')?.model.id).toBe('rtSmall')
  })

  it('answers for the whole group from any member file', () => {
    expect(removalImpact('scriptGateLabels')).toEqual({
      model: model('scriptGate'),
      files: ['scriptGate', 'scriptGateLabels'],
      disables: ['legacy'],
    })
    expect(removalImpact('ocrDecoder')?.files).toEqual([...OCR_FILES])
  })

  it('never includes the shared RT-DETR weights in another group', () => {
    for (const id of ['scriptGate', 'mangaOcr', 'ctd', 'samTs', 'rtFull', 'lama']) {
      expect(removalImpact(id)?.files).not.toContain('balloonDetector')
    }
  })

  it('knows when the model is in use by the cleaning in force', () => {
    expect(usedNow('rtSmall', { detection: ALL })).toBe(true)
    expect(usedNow('mangaOcr', { detection: ALL, ocrRescue: false })).toBe(false)
    expect(usedNow('scriptGate', { textPolicy: ALL_TEXT_POLICY, detection: ALL })).toBe(false)
    expect(usedNow('samTs', { textPolicy: ALL_TEXT_POLICY })).toBe(true)
  })
})

describe('where detection runs', () => {
  it('reads anything but an explicit cloud choice as this computer', () => {
    expect(normalizeAnalysisTargets(undefined)).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(normalizeAnalysisTargets({ rtFull: 'cloud', samTs: 'remote', ctd: 'cloud' })).toEqual({ rtFull: 'cloud', samTs: 'local' })
    expect(normalizeAnalysisTargets(['cloud'])).toEqual({ rtFull: 'local', samTs: 'local' })
  })

  it('sends both cloud stages on the cloud GPU, whatever this computer selected, in a stable order', () => {
    const both = { rtFull: 'cloud', samTs: 'cloud' }
    expect(cloudCapabilitiesFor({ detectorModels: ['ctd', 'rtFull', 'samTs'], analysisTargets: both })).toEqual(['text_mask_sam_ts@1', 'text_regions_rt@1'])
    expect(cloudCapabilitiesFor({ detectorModels: ['ctd', 'rtSmall'], analysisTargets: both })).toEqual(['text_mask_sam_ts@1', 'text_regions_rt@1'])
    expect(cloudCapabilitiesFor({ detectorModels: ['ctd', 'rtFull', 'samTs'], analysisTargets: { rtFull: 'local', samTs: 'local' } })).toEqual([])
    // A split record settles on this computer: nothing is sent.
    expect(cloudCapabilitiesFor({ detectorModels: ['samTs'], analysisTargets: { samTs: 'cloud' } })).toEqual([])
  })

  it('stops asking for local files of a model the cloud runs', () => {
    const cloud = { rtFull: 'cloud', samTs: 'cloud' }
    expect(localDetectorModels({ detectorModels: ['ctd', 'rtFull', 'samTs'], analysisTargets: cloud })).toEqual(['ctd'])
    expect(localDetectorModels({ detectorModels: ['rtSmall', 'samTs'], analysisTargets: cloud })).toEqual(['ctd'])
    expect(localDetectorModels({ detectorModels: ['rtSmall', 'samTs'] })).toEqual(['rtSmall', 'samTs'])
    const needs = workflowNeeds({ textPolicy: ALL_TEXT_POLICY, detectorModels: ['rtFull', 'samTs'], analysisTargets: cloud })
    expect(needs).not.toContain('samTs')
    expect(needs).not.toContain('rtFull')
  })
})

describe('the selection a run uses', () => {
  const cloud = { rtFull: 'cloud', samTs: 'cloud' }

  it('is the user’s own on this computer, as stored', () => {
    expect(runDetection({ detectorModels: ['ctd', 'rtSmall'], analysisTargets: { rtFull: 'local', samTs: 'local' }, ocrRescue: false }))
      .toEqual({ target: 'local', detectorModels: ['ctd', 'rtSmall'], ocrRescue: false })
    expect(runDetection({ detectorModels: ['samTs'], ocrRescue: true }))
      .toEqual({ target: 'local', detectorModels: ['samTs'], ocrRescue: true })
    expect(runDetection()).toEqual({ target: 'local', detectorModels: [...DEFAULT_DETECTOR_MODELS], ocrRescue: false })
  })

  it('is all four on the cloud GPU, whatever this computer holds, and leaves the stored choice alone', () => {
    const stored = { detectorModels: ['rtSmall'], analysisTargets: cloud, ocrRescue: false }
    expect(runDetection(stored)).toEqual({ target: 'cloud', detectorModels: ['ctd', 'rtFull', 'samTs'], ocrRescue: true })
    expect(CLOUD_DETECTOR_MODELS).toEqual(['ctd', 'rtFull', 'samTs'])
    expect(stored).toEqual({ detectorModels: ['rtSmall'], analysisTargets: cloud, ocrRescue: false })
    expect(detectsOnCloud(cloud)).toBe(true)
    expect(detectsOnCloud({ rtFull: 'cloud', samTs: 'local' })).toBe(false)
    expect(detectsOnCloud(undefined)).toBe(false)
  })

  it('downloads only CTD and the text reader for the cloud GPU (and the gate under legacy), never a cloud stage or Small', () => {
    for (const detectorModels of [['ctd', 'rtSmall'], ['rtSmall', 'samTs'], ['ctd', 'rtFull', 'samTs']]) {
      expect(workflowNeeds({ textPolicy: LEGACY_POLICY, detection: ALL, detectorModels, analysisTargets: cloud }))
        .toEqual(['ctd', 'scriptGate', 'hayaiOcr'])
      expect(workflowNeeds({ textPolicy: ALL_TEXT_POLICY, detectorModels, analysisTargets: cloud }))
        .toEqual(['ctd', 'hayaiOcr'])
      expect(filesFor(ALL, {}, { textPolicy: LEGACY_POLICY, detectorModels, analysisTargets: cloud }))
        .toEqual(['textDetector', ...SCRIPT_GATE_FILES, ...HAYAI_FILES])
      expect(filesFor(NONE, {}, { textPolicy: ALL_TEXT_POLICY, detectorModels, analysisTargets: cloud }))
        .toEqual(['textDetector', ...HAYAI_FILES])
    }
    // Switching back gives the user’s own combination and switch back.
    expect(workflowNeeds({ textPolicy: ALL_TEXT_POLICY, detectorModels: ['rtSmall', 'samTs'], ocrRescue: false }))
      .toEqual(['rtSmall', 'samTs'])
    // Every language skipped under legacy still detects nothing, on either side.
    expect(workflowNeeds({ textPolicy: LEGACY_POLICY, detection: NONE, detectorModels: ['ctd'], analysisTargets: cloud })).toEqual([])
  })
})

describe('choosing detection models', () => {
  it('keeps Full and Small apart: choosing one clears the other', () => {
    expect(toggleDetectorModel(['ctd', 'rtSmall'], 'rtFull', true)).toEqual(['ctd', 'rtFull'])
    expect(toggleDetectorModel(['ctd', 'rtFull', 'samTs'], 'rtSmall', true)).toEqual(['ctd', 'rtSmall', 'samTs'])
  })

  it('combines CTD and the lettering mask with either profile', () => {
    expect(toggleDetectorModel(['rtFull'], 'ctd', true)).toEqual(['ctd', 'rtFull'])
    expect(toggleDetectorModel(['ctd', 'rtFull'], 'samTs', true)).toEqual(['ctd', 'rtFull', 'samTs'])
    expect(toggleDetectorModel(['ctd', 'rtSmall', 'samTs'], 'ctd', false)).toEqual(['rtSmall', 'samTs'])
  })

  it('refuses to clear the last model, and ignores an id it does not know', () => {
    expect(toggleDetectorModel(['samTs'], 'samTs', false)).toEqual(['samTs'])
    expect(toggleDetectorModel(['ctd'], 'coo', true)).toEqual(['ctd'])
  })

  it('stores the selection in wire order, whatever order it was ticked in', () => {
    expect(toggleDetectorModel(['samTs'], 'ctd', true)).toEqual(['ctd', 'samTs'])
  })
})

describe('an older session’s detection models', () => {
  it('keeps Small and the rest when a record holds both profiles, which no run accepts', () => {
    expect(normalizeDetectorModels(['ctd', 'rtSmall', 'rtFull', 'samTs'])).toEqual(['ctd', 'rtSmall', 'samTs'])
    expect(normalizeDetectorModels(['rtFull', 'rtSmall'])).toEqual(['rtSmall'])
  })

  it('drops an id it does not know rather than voiding the whole choice', () => {
    expect(normalizeDetectorModels(['coo', 'samTs'])).toEqual(['samTs'])
    expect(normalizeDetectorModels(['ctd', 'rtFull', 'sfx'])).toEqual(['ctd', 'rtFull'])
  })

  it('reads nothing usable, or no list at all, as the default', () => {
    expect(normalizeDetectorModels([])).toEqual([...DEFAULT_DETECTOR_MODELS])
    expect(normalizeDetectorModels(['coo'])).toEqual([...DEFAULT_DETECTOR_MODELS])
    expect(normalizeDetectorModels('ctd')).toEqual([...DEFAULT_DETECTOR_MODELS])
    expect(normalizeDetectorModels(undefined)).toEqual([...DEFAULT_DETECTOR_MODELS])
  })
})

describe('one place for cloud detection', () => {
  it('says which choices have a cloud version: Full and the lettering mask only', () => {
    expect(DETECTOR_CHOICES.filter(runsOnCloud)).toEqual(['rtFull', 'samTs'])
  })

  it('keeps both stages where they agree, and settles a split on this computer', () => {
    expect(unifyAnalysisTargets({ rtFull: 'cloud', samTs: 'cloud' })).toEqual({ rtFull: 'cloud', samTs: 'cloud' })
    expect(unifyAnalysisTargets({ rtFull: 'local', samTs: 'local' })).toEqual({ rtFull: 'local', samTs: 'local' })
    // The retired per-model Run on could split them. A migration never sends
    // a model the user kept here, whichever side was the cloud one.
    expect(unifyAnalysisTargets({ rtFull: 'local', samTs: 'cloud' })).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(unifyAnalysisTargets({ rtFull: 'cloud', samTs: 'local' })).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(unifyAnalysisTargets({ samTs: 'cloud' })).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(unifyAnalysisTargets(undefined)).toEqual({ rtFull: 'local', samTs: 'local' })
  })

  it('places the cloud run’s models: Full and SAM-TS-L there, CTD here, Small nowhere', () => {
    const cloud = { rtFull: 'cloud', samTs: 'cloud' }
    for (const detectorModels of [['ctd', 'rtFull', 'samTs'], ['rtSmall', 'samTs'], ['ctd', 'rtSmall']]) {
      expect(detectionPlacement({ detectorModels, analysisTargets: cloud }))
        .toEqual({ target: 'cloud', cloud: ['rtFull', 'samTs'], local: ['ctd'] })
    }
    // On this computer every selected model runs here, in screen order.
    expect(detectionPlacement({ detectorModels: ['ctd', 'rtSmall', 'samTs'], analysisTargets: { rtFull: 'local', samTs: 'local' } }))
      .toEqual({ target: 'local', cloud: [], local: ['ctd', 'rtSmall', 'samTs'] })
  })
})
