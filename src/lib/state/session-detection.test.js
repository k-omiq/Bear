/**
 * The stored detection choices and the OCR rescue switch: a retired
 * `ctd-rtdetr-ocr` row reads back as the plain detector with the rescue on,
 * and the text policy keeps its name and values for the code that reads it.
 */

import { afterEach, describe, expect, it } from 'vitest'
import {
  TEXT_POLICIES,
  sanitizeSession,
  session,
  adoptBackendSettings,
  backendSettingsPatch,
  setAnalysisTarget,
  setDetection,
  setModelAccelerator,
  setOcrRescue,
  setTextPolicy,
} from './session.svelte.js'

afterEach(() => {
  for (const language of ['ja', 'zh', 'ko']) setDetection(language, 'ctd-rtdetr')
  setOcrRescue(false)
  setTextPolicy('legacy_gate')
  for (const id of ['ctd', 'rtSmall', 'rtFull', 'samTs', 'inpainter']) setModelAccelerator(id, 'inherit')
  setAnalysisTarget('rtFull', 'local')
  setAnalysisTarget('samTs', 'local')
})

describe('a stored detection map', () => {
  it('migrates a retired OCR row to the plain detector and turns the rescue on', () => {
    const stored = sanitizeSession({ detection: { ja: 'ctd-rtdetr-ocr', zh: null, ko: 'ctd-rtdetr' }, ocrRescue: false })
    expect(stored.detection).toEqual({ ja: 'ctd-rtdetr', zh: null, ko: 'ctd-rtdetr' })
    expect(stored.ocrRescue).toBe(true)
  })

  it('keeps an explicit rescue choice when no retired row is stored', () => {
    expect(sanitizeSession({ detection: { ja: 'ctd-rtdetr' }, ocrRescue: true }).ocrRescue).toBe(true)
    expect(sanitizeSession({ detection: { ja: 'ctd-rtdetr' } }).ocrRescue).toBe(false)
  })

  it('drops a retired row where it never applied, and anything unknown', () => {
    const stored = sanitizeSession({ detection: { ko: 'ctd-rtdetr-ocr', zh: 'nonsense' } })
    expect(stored.detection).toEqual({ ja: 'ctd-rtdetr', zh: 'ctd-rtdetr', ko: 'ctd-rtdetr' })
    expect(stored.ocrRescue).toBe(false)
  })
})

describe('setDetection', () => {
  it('stores a detector or a skip', () => {
    setDetection('ko', null)
    expect(session.detection.ko).toBeNull()
    setDetection('ko', 'ctd-rtdetr')
    expect(session.detection.ko).toBe('ctd-rtdetr')
  })

  it('accepts a retired OCR row as the plain detector plus the rescue', () => {
    setDetection('ja', 'ctd-rtdetr-ocr')
    expect(session.detection.ja).toBe('ctd-rtdetr')
    expect(session.ocrRescue).toBe(true)
  })

  it('ignores an id the language cannot hold', () => {
    setDetection('zh', 'ctd-rtdetr-ocr')
    expect(session.detection.zh).toBe('ctd-rtdetr')
    expect(session.ocrRescue).toBe(false)
  })
})

describe('the text policy', () => {
  it('keeps its field name and two values stable', () => {
    expect(TEXT_POLICIES).toEqual(['legacy_gate', 'all_text'])
    setTextPolicy('all_text')
    expect(session.textPolicy).toBe('all_text')
    expect(sanitizeSession({ textPolicy: 'all_text' }).textPolicy).toBe('all_text')
    expect(sanitizeSession({ textPolicy: 'other' }).textPolicy).toBe('legacy_gate')
  })
})

describe('local model backend overrides', () => {
  it('persists a model choice through the backend patch and clears it to inherit', () => {
    setModelAccelerator('samTs', 'cpu')
    expect(session.modelAccelerators).toEqual({ samTs: 'cpu' })
    expect(backendSettingsPatch().modelAccelerators).toEqual({ samTs: 'cpu' })
    setModelAccelerator('samTs', 'inherit')
    expect(backendSettingsPatch().modelAccelerators).toEqual({})
  })

  it('drops unknown model keys and malformed provider ids from persisted state', () => {
    expect(sanitizeSession({ modelAccelerators: { samTs: 'cpu', invented: 'cuda', ctd: '$(bad)' } }).modelAccelerators)
      .toEqual({ samTs: 'cpu' })
  })
})

describe('where the cloud-capable detection stages run', () => {
  it('starts on this computer and drops anything it does not know', () => {
    expect(sanitizeSession({}).analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(sanitizeSession({ analysisTargets: { samTs: 'gpu', rtFull: 'elsewhere', coo: 'cloud' } }).analysisTargets)
      .toEqual({ rtFull: 'local', samTs: 'local' })
    expect(sanitizeSession({ analysisTargets: 'cloud' }).analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
  })

  it('reads a record from the per-model Run on back as one place: this computer', () => {
    // SAM on the cloud and Full here: nothing the user kept here is sent.
    expect(sanitizeSession({ detectorModels: ['ctd', 'rtFull', 'samTs'], analysisTargets: { rtFull: 'local', samTs: 'cloud' } }).analysisTargets)
      .toEqual({ rtFull: 'local', samTs: 'local' })
    expect(sanitizeSession({ detectorModels: ['ctd', 'rtSmall'], analysisTargets: { rtFull: 'cloud', samTs: 'local' } }).analysisTargets)
      .toEqual({ rtFull: 'local', samTs: 'local' })
    // Both on the cloud is one place already, and is kept.
    expect(sanitizeSession({ analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } }).analysisTargets)
      .toEqual({ rtFull: 'cloud', samTs: 'cloud' })
  })

  it('settles a split the backend stored, and hands the settled value back to be written', () => {
    // `App.svelte` writes `backendSettingsPatch()` after the boot
    // reconciliation, so the native file stops holding the split after one start.
    adoptBackendSettings({ analysisTargets: { rtFull: 'local', samTs: 'cloud' } })
    expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
    expect(backendSettingsPatch().analysisTargets).toEqual({ rtFull: 'local', samTs: 'local' })
  })

  it('reads a record holding both detector profiles as Small with the rest of its choices', () => {
    expect(sanitizeSession({ detectorModels: ['ctd', 'rtSmall', 'rtFull', 'samTs'] }).detectorModels).toEqual(['ctd', 'rtSmall', 'samTs'])
    expect(sanitizeSession({ detectorModels: ['coo', 'samTs'] }).detectorModels).toEqual(['samTs'])
  })

  it('changes one stage at a time, refuses unknown stages and places, and reaches the settings patch', () => {
    setAnalysisTarget('samTs', 'cloud')
    setAnalysisTarget('ctd', 'cloud')
    setAnalysisTarget('rtFull', 'elsewhere')
    expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'cloud' })
    expect(backendSettingsPatch().analysisTargets).toEqual({ rtFull: 'local', samTs: 'cloud' })
  })
})
