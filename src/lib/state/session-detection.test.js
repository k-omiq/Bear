/**
 * The stored detection choices and the OCR rescue switch: a retired
 * `ctd-rtdetr-ocr` row reads back as the plain detector with the rescue on,
 * and the text policy keeps its name and values for the code that reads it.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'

// Capture what reaches disk, so the persisted record's shape can be read.
const written = vi.hoisted(() => ({ records: [] }))
vi.mock('./persist.js', async (importOriginal) => {
  const actual = await importOriginal()
  return {
    ...actual,
    writeRecord: (key, value) => {
      written.records.push({ key, value })
      return true
    },
  }
})
import {
  TEXT_POLICIES,
  sanitizeSession,
  session,
  adoptBackendSettings,
  backendSettingsPatch,
  setAnalysisTarget,
  setDetection,
  setModelAccelerator,
  setDetectionMode,
  setOcrRescue,
} from './session.svelte.js'

afterEach(() => {
  for (const language of ['ja', 'zh', 'ko']) setDetection(language, 'ctd-rtdetr')
  setDetectionMode('normal')
  setOcrRescue(false)
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
    expect(sanitizeSession({ textPolicy: 'all_text' }).textPolicy).toBe('all_text')
    expect(sanitizeSession({ textPolicy: 'other' }).textPolicy).toBe('legacy_gate')
  })
})

describe('the detection mode', () => {
  it('reads a record written before modes as the mode its combination reached for', () => {
    expect(sanitizeSession({ detectorModels: ['ctd', 'rtFull'] }).detectionMode).toBe('advanced')
    expect(sanitizeSession({ detectorModels: ['ctd', 'rtSmall'] }).detectionMode).toBe('normal')
    expect(sanitizeSession({ textPolicy: 'all_text' }).detectionMode).toBe('advanced')
    expect(sanitizeSession({ detectorModels: 'rtFull', textPolicy: 7 }).detectionMode).toBe('normal')
    expect(sanitizeSession({}).detectionMode).toBe('normal')
  })

  it('reads detection on the cloud GPU as Advanced, whatever else the record says', () => {
    const old = sanitizeSession({ detectorModels: ['ctd', 'rtSmall'], textPolicy: 'legacy_gate', analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } })
    expect(old).toMatchObject({ detectionMode: 'advanced', textPolicy: 'all_text', analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } })
    expect(sanitizeSession({ detectionMode: 'normal', analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } }).detectionMode).toBe('advanced')
  })

  it('lets a stored mode win over whatever combination the record also holds', () => {
    const normal = sanitizeSession({ detectionMode: 'normal', detectorModels: ['ctd', 'rtFull', 'samTs'], textPolicy: 'all_text' })
    expect(normal).toMatchObject({ detectionMode: 'normal', detectorModels: ['ctd', 'rtSmall'], textPolicy: 'legacy_gate' })
    const advanced = sanitizeSession({ detectionMode: 'advanced', detectorModels: ['ctd'] })
    expect(advanced).toMatchObject({ detectionMode: 'advanced', detectorModels: ['ctd', 'rtFull', 'samTs'], textPolicy: 'all_text' })
    expect(sanitizeSession({ detectionMode: 'turbo', detectorModels: ['ctd', 'rtFull'] }).detectionMode).toBe('advanced')
  })

  it('derives the models and the policy from the mode, whatever combination a record held', () => {
    expect(sanitizeSession({ detectorModels: ['ctd', 'rtSmall', 'samTs'] }).detectorModels).toEqual(['ctd', 'rtFull', 'samTs'])
    expect(sanitizeSession({ detectorModels: ['ctd'] }).detectorModels).toEqual(['ctd', 'rtSmall'])
  })

  it('sets the models, the policy and the reader together, and only on a change', () => {
    setDetectionMode('advanced')
    expect(session.detectionMode).toBe('advanced')
    expect(session.detectorModels).toEqual(['ctd', 'rtFull', 'samTs'])
    expect(session.textPolicy).toBe('all_text')
    expect(session.ocrRescue).toBe(true)
    // The reader stays the user's: choosing the same mode again leaves it alone.
    setOcrRescue(false)
    setDetectionMode('advanced')
    expect(session.ocrRescue).toBe(false)
    setDetectionMode('normal')
    expect(session).toMatchObject({ detectionMode: 'normal', detectorModels: ['ctd', 'rtSmall'], textPolicy: 'legacy_gate', ocrRescue: false })
    setDetectionMode('turbo')
    expect(session.detectionMode).toBe('normal')
  })

  it('stores the mode, and never the models or the policy it derives', () => {
    written.records.length = 0
    setDetectionMode('advanced')
    const record = written.records.at(-1)?.value
    expect(record.detectionMode).toBe('advanced')
    expect(record).not.toHaveProperty('detectorModels')
    expect(record).not.toHaveProperty('textPolicy')
    expect(sanitizeSession(JSON.parse(JSON.stringify(record))).detectorModels).toEqual(['ctd', 'rtFull', 'samTs'])
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

  it('changes one stage at a time, refuses unknown stages and places, and reaches the settings patch', () => {
    setAnalysisTarget('samTs', 'cloud')
    setAnalysisTarget('ctd', 'cloud')
    setAnalysisTarget('rtFull', 'elsewhere')
    expect(session.analysisTargets).toEqual({ rtFull: 'local', samTs: 'cloud' })
    expect(backendSettingsPatch().analysisTargets).toEqual({ rtFull: 'local', samTs: 'cloud' })
  })
})
