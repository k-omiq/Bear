import { describe, expect, it } from 'vitest'
import { hasKey, t } from '../i18n/index.js'
import { declineReasonOf, hitTest, isRemoteAnalysis, outcomeCopy, outcomeOf, outcomeOfRecord, readinessKeyOf, runtimeLoadOf, toneOf } from './workflowoutcome.js'

/**
 * One row per refusal text the review can receive, spelled exactly as the
 * backend spells it: every `Err(...)` in `src-tauri/src/model_workflows.rs`
 * and `src-tauri/src/inference/analysis.rs`, the tile planner's refusals in
 * `cleaner-core/src/cloud_tiles.rs`, and the journal's failure codes. A
 * wording change on the native side fails its row here instead of quietly
 * becoming "Something went wrong".
 */
const LOCAL = [
  // Requests and readiness
  ['Analysis request id is required', 'analyze', { kind: 'invalidRequest' }],
  ['Analysis request id was already used', 'analyze', { kind: 'invalidRequest' }],
  ['Choose Regions, Mask, or Text-shaped review', 'analyze', { kind: 'invalidRequest' }],
  ['Choose the Full (tiled) or installed Small Ogkalu detector profile', 'analyze', { kind: 'invalidRequest' }],
  ['This analysis path requires matching-hardware qualification on this operating system', 'analyze', { kind: 'declined', reason: 'host' }],
  ['RT backend ort-webgpu is not qualified for this analysis path', 'analyze', { kind: 'declined', reason: 'host' }],
  ['SAM backend ort-coreml is unavailable for this analysis path', 'analyze', { kind: 'declined', reason: 'host' }],
  ['Unsupported SAM backend', 'analyze', { kind: 'declined', reason: 'host' }],
  ['Chapter model analysis currently requires a paginated chapter', 'analyze', { kind: 'sourceLimit', reason: 'longstrip' }],
  ['Chapter source path is not UTF-8', 'analyze', { kind: 'sourceUnreadable' }],
  ['Chapter source is missing', 'analyze', { kind: 'sourceUnreadable' }],
  ['Page is no longer in chapter', 'analyze', { kind: 'expired' }],
  // Models
  ['The SAM-TS-L lettering mask graphs are not installed', 'analyze', { kind: 'modelMissing' }],
  ['The Ogkalu comic text & bubble detector (Full) graph is not installed or failed SHA-256', 'analyze', { kind: 'modelMissing' }],
  ['The Ogkalu comic text & bubble detector (Small) graph is not installed or failed SHA-256', 'analyze', { kind: 'modelMissing' }],
  ['The SAM-TS-L lettering mask needs at least 10 GB of measured process memory room', 'analyze', { kind: 'memory' }],
  ['The Ogkalu comic text & bubble detector (Full) graph size or SHA-256 does not match the pinned manifest', 'models', { kind: 'wrongModel' }],
  ['The copied Ogkalu comic text & bubble detector (Full) graph failed verification', 'models', { kind: 'wrongModel' }],
  ['encoder.onnx has 12 bytes; expected 4096', 'models', { kind: 'wrongModel' }],
  ['encoder.onnx has 12 bytes; expected 4096', 'analyze', { kind: 'modelMissing' }],
  [`head.onnx SHA-256 mismatch: ${'f'.repeat(64)}`, 'models', { kind: 'wrongModel' }],
  [`head.onnx SHA-256 mismatch: ${'f'.repeat(64)}`, 'analyze', { kind: 'modelMissing' }],
  // Source page
  ['Source page exceeds the 20 MB review-preview limit', 'analyze', { kind: 'sourceLimit' }],
  ['This review preview is limited to 24 megapixels', 'analyze', { kind: 'sourceLimit' }],
  ['Source preview format is unsupported', 'analyze', { kind: 'sourceLimit' }],
  // Component writes
  [new Error('Component writes require the desktop runtime'), 'prepare', { kind: 'unavailable' }],
  ['Only a SAM component can grant write support', 'prepare', { kind: 'declined', reason: 'box' }],
  ['Invalid SAM component id', 'prepare', { kind: 'expired' }],
  ['Invalid SAM component or mask dimensions', 'prepare', { kind: 'expired' }],
  ['SAM component is absent from this analysis', 'prepare', { kind: 'expired' }],
  ['Component writing cannot fill indexed palette PNG sources', 'prepare', { kind: 'declined', reason: 'indexed' }],
  ['Component writing cannot fill sub-8-bit PNG sources', 'prepare', { kind: 'declined', reason: 'sub8Bit' }],
  ['Component writes currently require a PNG chapter source; JPEG parity is unresolved', 'prepare', { kind: 'declined', reason: 'jpeg' }],
  ['Component writing currently requires a paginated chapter', 'prepare', { kind: 'declined', reason: 'longstrip' }],
  ['This analysis is not qualified for component writing on this host and runtime', 'prepare', { kind: 'declined', reason: 'host' }],
  ['This prepared write is not qualified on this host and runtime', 'apply', { kind: 'declined', reason: 'host' }],
  ['Remote analysis is review-only and cannot prepare a component write', 'prepare', { kind: 'declined', reason: 'remote' }],
  ['Remote analysis is review-only and cannot load a component correction', 'load', { kind: 'declined', reason: 'remote' }],
  ['Outside-bubble component is held until explicitly permitted', 'prepare', { kind: 'held' }],
  ['Empty component has no write support', 'prepare', { kind: 'needsCorrection', reason: 'empty' }],
  ['Analyzed component overlaps an existing visible edit; analyze a fresh page or use the editor mask tools', 'prepare', { kind: 'needsCorrection', reason: 'overlap' }],
  ['Correction raster exceeds the 16 megapixel plan limit', 'prepare', { kind: 'needsCorrection', reason: 'tooLarge' }],
  ['Existing region does not use text-shaped geometry', 'prepare', { kind: 'existingGeometry' }],
  ['Analysis expired; analyze the page again', 'prepare', { kind: 'expired' }],
  ['Analysis changed; preview the component again', 'apply', { kind: 'expired' }],
  ['The selected chapter page does not match the analyzed source', 'prepare', { kind: 'expired' }],
  ['Saved correction does not match this analysis; analyze the page again', 'load', { kind: 'expired' }],
  ['Saved correction raster is outside the analyzed page bounds', 'load', { kind: 'expired' }],
  ['Saved SAM base mask changed; analyze the page again', 'prepare', { kind: 'expired' }],
  ['Mask corrections changed without a new correction revision', 'prepare', { kind: 'stale' }],
  ['Prepared write expired; preview the component again', 'apply', { kind: 'stale' }],
  ['Approval does not match the prepared support raster', 'apply', { kind: 'stale' }],
  ['Source changed since approval', 'apply', { kind: 'stale' }],
  ['Approved support raster changed', 'apply', { kind: 'stale' }],
  ['Region order changed since preview', 'apply', { kind: 'stale' }],
  ['Mask plan revision changed since preview', 'apply', { kind: 'stale' }],
  ['Visible page changed since preview; prepare the component again', 'apply', { kind: 'stale' }],
  ['No surrounding pixels available for bounded fill', 'apply', { kind: 'badReconstruction' }],
  ['Write support is outside the composited underlay', 'prepare', { kind: 'badReconstruction' }],
  ['Patch order is exhausted', 'apply', { kind: 'exhausted' }],
  ['Mask plan revision is exhausted', 'apply', { kind: 'exhausted' }],
  ['Saved component could not be found in chapter', 'apply', { kind: 'unconfirmed' }],
]

const CLOUD = [
  ['cloud_disabled', 'capabilities', { kind: 'cloudDisabled' }],
  ['analysis_profile_not_active', 'capabilities', { kind: 'cloudProfile', reason: 'inactive' }],
  ['analysis_profile_missing', 'capabilities', { kind: 'cloudProfile', reason: 'missing' }],
  ['credential missing: no runtime key stored for this profile', 'capabilities', { kind: 'cloudProfile', reason: 'credential' }],
  ['configuration error: endpoint URL must use https', 'capabilities', { kind: 'cloudProfile', reason: 'config' }],
  ['capability_unavailable: gateway advertisement is invalid', 'capabilities', { kind: 'capabilityMissing', reason: 'invalid' }],
  ['capability_unavailable: gateway analysis is not configured', 'capabilities', { kind: 'capabilityMissing', reason: 'notConfigured' }],
  ['capability_unavailable: gateway does not advertise this analysis model', 'propose', { kind: 'capabilityMissing', reason: 'absent' }],
  ['capability_unavailable: unsupported analysis capability', 'propose', { kind: 'capabilityMissing', reason: 'absent' }],
  ['capability_unavailable: gateway tile limits are smaller than this upload', 'propose', { kind: 'capabilityMissing', reason: 'limits' }],
  ['capability_unavailable: model identity changed', 'confirm', { kind: 'capabilityMissing', reason: 'modelChanged' }],
  ['Remote analysis currently requires a paginated chapter', 'propose', { kind: 'sourceLimit', reason: 'longstrip' }],
  ['Analysis page exceeds the 24 megapixel review limit', 'propose', { kind: 'sourceLimit' }],
  ['Source page exceeds the review-preview limit', 'propose', { kind: 'sourceLimit' }],
  ['Analysis source geometry changed or exceeds the review limit', 'propose', { kind: 'sourceLimit', reason: 'geometry' }],
  ['Analysis tile PNG exceeds transfer limit', 'propose', { kind: 'sourceLimit', reason: 'tile' }],
  ['analysis tile count exceeded', 'propose', { kind: 'sourceLimit', reason: 'upload' }],
  ['analysis upload extent exceeded', 'propose', { kind: 'sourceLimit', reason: 'upload' }],
  ['analysis page grid too large', 'propose', { kind: 'sourceLimit', reason: 'upload' }],
  ['analysis_proposal_limit', 'propose', { kind: 'proposalLimit' }],
  ['Random source unavailable', 'propose', { kind: 'system' }],
  ['rights_attestation_required', 'confirm', { kind: 'consentRequired', reason: 'rights' }],
  ['retention_acknowledgement_required', 'confirm', { kind: 'consentRequired', reason: 'retention' }],
  ['analysis_proposal_missing', 'confirm', { kind: 'proposalExpired' }],
  ['analysis_proposal_expired', 'confirm', { kind: 'proposalExpired' }],
  ['proposal_expired', 'confirm', { kind: 'proposalExpired' }],
  ['analysis_proposal_consumed', 'confirm', { kind: 'proposalConsumed' }],
  ['analysis_stale: source or underlay changed before dispatch', 'confirm', { kind: 'remoteStale' }],
  ['analysis_stale: source or underlay changed during batch', 'confirm', { kind: 'remoteStale' }],
  ['analysis_stale: source or underlay changed before evidence attachment', 'confirm', { kind: 'remoteStale' }],
  ['review_evidence_expired', 'confirm', { kind: 'expired' }],
  ['analysis page too large', 'confirm', { kind: 'remoteInvalid' }],
  ['analysis result lies outside page', 'confirm', { kind: 'remoteInvalid' }],
  ['analysis mask missing', 'confirm', { kind: 'remoteInvalid' }],
  ['invalid analysis box class', 'confirm', { kind: 'remoteInvalid' }],
]

describe('outcomeOf names every native refusal the review can meet', () => {
  it('names a cancellation without a technical detail, since the user asked for it', () => {
    expect(outcomeOf('analysis cancelled', 'analyze')).toEqual({ kind: 'cancelled' })
    expect(outcomeOf('analysis_cancelled', 'confirm')).toEqual({ kind: 'remoteCancelled' })
  })

  it.each([...LOCAL, ...CLOUD])('%s (%s)', (cause, phase, expected) => {
    const outcome = outcomeOf(cause, /** @type {any} */ (phase))
    expect(outcome).toMatchObject(expected)
    expect(outcome.detail).toBe(typeof cause === 'string' ? cause : cause.message)
    if (!('reason' in expected) && outcome.kind !== 'failed') expect(outcome.reason).toBeUndefined()
    // Every outcome is said in the catalogue's words, never the native text.
    const copy = outcomeCopy(outcome)
    expect(hasKey(copy.title)).toBe(true)
    expect(hasKey(copy.body)).toBe(true)
  })

  it('falls through to a model failure while analyzing and a phase failure otherwise', () => {
    expect(outcomeOf('ONNX session aborted', 'analyze')).toMatchObject({ kind: 'modelFailed' })
    expect(outcomeOf('disk full', 'apply')).toMatchObject({ kind: 'failed', reason: 'apply' })
    expect(outcomeOf(undefined, 'load')).toMatchObject({ kind: 'failed', reason: 'load', detail: '' })
    expect(outcomeOf('/models/sam/encoder.onnx: No such file or directory (os error 2)', 'models'))
      .toMatchObject({ kind: 'failed', reason: 'models' })
    for (const phase of ['capabilities', 'propose', 'confirm']) {
      const outcome = outcomeOf('transport error: connection reset', /** @type {any} */ (phase))
      expect(outcome).toMatchObject({ kind: 'failed', reason: phase, detail: 'transport error: connection reset' })
      expect(t(outcomeCopy(outcome).body)).toContain('Nothing')
    }
  })
})

describe('outcomeOfRecord reads how a cloud run ended from its journal record', () => {
  const record = (phase, extra = {}) => ({ proposal_id: 'p', total_tiles: 2, completed_tiles: 1, phase, ...extra })

  it('is null while a run is going or once its evidence is attached', () => {
    for (const phase of ['proposed', 'confirmed', 'submitted_tile', 'result_cached_tile', 'attached_evidence']) {
      expect(outcomeOfRecord(record({ phase, index: 0 }))).toBeNull()
    }
    expect(outcomeOfRecord(null)).toBeNull()
  })

  it('names a cancelled run', () => {
    expect(outcomeOfRecord(record({ phase: 'cancelled' }, { cancel_requested: true }))).toEqual({ kind: 'remoteCancelled' })
  })

  it('says a tile may have run when its answer never came, and whether the user had asked to cancel', () => {
    const unknown = outcomeOfRecord(record({ phase: 'unknown_remote_state', index: 1 }))
    expect(unknown).toEqual({ kind: 'remoteUnknown' })
    expect(t(outcomeCopy(unknown).body)).toBe('The last tile may have run. It will not be sent again automatically. Check the provider before starting a new analysis.')

    const requested = outcomeOfRecord(record({ phase: 'unknown', index: 1 }, { cancel_requested: true }))
    expect(requested).toEqual({ kind: 'remoteUnknown', reason: 'cancelRequested' })
    expect(t(outcomeCopy(requested).title)).toBe('Last tile state unknown')
    expect(t(outcomeCopy(requested).body)).toBe('Cancel was requested while a tile was out. That tile may have run. It will not be sent again automatically. Check the provider before starting a new analysis.')
    expect(outcomeOfRecord(record({ phase: 'unknown', index: 1 }, { cancel_requested: false }))).toEqual({ kind: 'remoteUnknown' })
  })

  it('reads a failure code like the thrown text', () => {
    expect(outcomeOfRecord(record({ phase: 'failed', code: 'analysis_stale' }))).toMatchObject({ kind: 'remoteStale' })
    expect(outcomeOfRecord(record({ phase: 'failed', code: 'proposal_expired' }))).toMatchObject({ kind: 'proposalExpired' })
    expect(outcomeOfRecord(record({ phase: 'failed', code: 'review_evidence_expired' }))).toMatchObject({ kind: 'expired' })
    expect(outcomeOfRecord(record({ phase: 'failed', code: 'analysis_failed' }))).toMatchObject({ kind: 'failed', reason: 'confirm' })
  })
})

describe('outcomeCopy', () => {
  it('says each kind with its own title and the body its reason picks', () => {
    expect(outcomeCopy({ kind: 'declined', reason: 'remote' })).toEqual({ title: 'workflow.outcome.declined', body: 'cloud.analysis.reviewOnly', params: undefined })
    expect(outcomeCopy({ kind: 'declined', reason: 'unheard-of' }).body).toBe('workflow.declined.host')
    expect(outcomeCopy({ kind: 'capabilityMissing' }).body).toBe('cloud.analysis.capabilityMissing')
    expect(outcomeCopy({ kind: 'sourceLimit' }).body).toBe('workflow.explain.sourceLimit')
    expect(outcomeCopy({ kind: 'sourceLimit', reason: 'longstrip' }).body).toBe('workflow.explain.longstrip')
    expect(outcomeCopy({ kind: 'failed', reason: 'confirm' }).body).toBe('cloud.analysis.failure.confirm')
    expect(outcomeCopy({ kind: 'notReady', reason: 'workflow.ready.sam' })).toEqual({ title: 'workflow.outcome.notReady', body: 'workflow.ready.sam', params: undefined })
    expect(outcomeCopy({ kind: 'applied', params: { page: 3 } })).toEqual({ title: 'workflow.outcome.applied', body: 'workflow.explain.applied', params: { page: 3 } })
    expect(outcomeCopy({ kind: /** @type {any} */ ('unheard-of') })).toEqual({ title: 'workflow.outcome.failed', body: 'workflow.explain.modelFailed', params: undefined })
  })

  it('never shows a cancel the user asked for as a warning', () => {
    expect(toneOf('remoteCancelled')).toBe('info')
    expect(toneOf('cancelled')).toBe('info')
    expect(toneOf('capabilityMissing')).toBe('info')
    expect(toneOf('applied')).toBe('ok')
    expect(toneOf('remoteUnknown')).toBe('warn')
    expect(toneOf('remoteStale')).toBe('warn')
  })
})

describe('runtimeLoadOf', () => {
  it('reads the ONNX Runtime row of a diagnostics answer', () => {
    expect(runtimeLoadOf({ components: [{ name: 'onnxruntime', available: true }] })).toEqual({ state: 'loaded' })
    expect(runtimeLoadOf({ components: [{ name: 'onnxruntime', available: false, reasonKey: 'diagnostics.runtime.refused' }] }))
      .toEqual({ state: 'failed', reasonKey: 'diagnostics.runtime.refused' })
    expect(runtimeLoadOf({ components: [{ name: 'onnxruntime', available: false, reasonKey: 'diagnostics.runtime.somethingNew' }] }))
      .toEqual({ state: 'failed', reasonKey: 'diagnostics.runtime.unloadable' })
    expect(runtimeLoadOf({ components: [{ name: 'other', available: true }] })).toEqual({ state: 'unchecked' })
    expect(runtimeLoadOf(null)).toEqual({ state: 'unchecked' })
  })
})

describe('isRemoteAnalysis', () => {
  it('knows cloud evidence by its source or its backend', () => {
    expect(isRemoteAnalysis({ remoteSource: 'remote:modal:text_mask_sam_ts@1' })).toBe(true)
    expect(isRemoteAnalysis({ samBackend: 'remote' })).toBe(true)
    expect(isRemoteAnalysis({ rtBackend: 'remote' })).toBe(true)
    expect(isRemoteAnalysis({ samBackend: 'ort-webgpu', rtBackend: 'ort-cpu' })).toBe(false)
    expect(isRemoteAnalysis(null)).toBe(false)
  })
})

describe('declineReasonOf', () => {
  const base = { samWriteEligible: false, samBackend: 'ort-webgpu', sourceDataUrl: 'data:image/png;base64,AA', samCpuFallbackNodes: [0, 0] }
  const qualified = { samWriteQualified: true }

  it('is null for an eligible result', () => {
    expect(declineReasonOf({ ...base, samWriteEligible: true }, qualified)).toBeNull()
  })

  it('declines cloud evidence first, whatever else it claims', () => {
    expect(declineReasonOf({ ...base, samWriteEligible: true, samBackend: 'remote' }, qualified)).toBe('remote')
    expect(declineReasonOf({ ...base, samBackend: null, rtBackend: 'remote', remoteSource: 'remote:beam:text_regions_rt@1' }, qualified)).toBe('remote')
  })

  it('names the first condition a user can act on', () => {
    expect(declineReasonOf({ ...base, samBackend: null }, qualified)).toBe('regions')
    expect(declineReasonOf({ ...base, samBackend: 'ort-cpu' }, qualified)).toBe('cpu')
    expect(declineReasonOf({ ...base, sourceDataUrl: 'data:image/jpeg;base64,AA' }, qualified)).toBe('jpeg')
    expect(declineReasonOf({ ...base, sourceDataUrl: 'data:image/webp;base64,AA' }, qualified)).toBe('format')
    expect(declineReasonOf(base, { samWriteQualified: false })).toBe('host')
    expect(declineReasonOf({ ...base, samCpuFallbackNodes: [0, 3] }, qualified)).toBe('provider')
    expect(declineReasonOf(base, qualified)).toBe('host')
  })
})

describe('readinessKeyOf', () => {
  const caps = {
    runtimeInstalled: true, fullRtInstalled: true, rtInstalled: false, samInstalled: true, samMemoryReady: true,
    rtBackends: [{ id: 'ort-cpu', selectable: true }], samBackends: [{ id: 'ort-cpu', selectable: true }],
  }
  const choice = { rtProfile: 'full-halves', rtBackend: 'ort-cpu', samBackend: 'ort-cpu', verified: true, load: 'loaded' }
  const both = { needs: ['rt', 'sam'] }

  it('is null when the workflow can run', () => {
    expect(readinessKeyOf(caps, both, choice)).toBeNull()
    expect(readinessKeyOf(caps, both, { ...choice, rtBackend: 'auto', samBackend: 'auto' })).toBeNull()
  })

  it('names the missing piece', () => {
    expect(readinessKeyOf({ ...caps, runtimeInstalled: false }, both, choice)).toBe('workflow.ready.runtime')
    expect(readinessKeyOf(caps, both, { ...choice, rtProfile: 'small-whole' })).toBe('workflow.ready.rt')
    expect(readinessKeyOf({ ...caps, samInstalled: false }, both, choice)).toBe('workflow.ready.sam')
    expect(readinessKeyOf(caps, both, { ...choice, verified: false })).toBe('workflow.ready.samUnverified')
    expect(readinessKeyOf({ ...caps, samMemoryReady: false }, both, choice)).toBe('workflow.ready.memory')
    expect(readinessKeyOf(caps, both, { ...choice, samBackend: 'ort-webgpu' })).toBe('workflow.ready.backend')
  })

  it('reads the runtime as Settings does: installed is not enough, it must load', () => {
    expect(readinessKeyOf(caps, both, { ...choice, load: 'failed' })).toBe('workflow.ready.runtimeUnloadable')
    expect(readinessKeyOf(caps, both, { ...choice, load: 'checking' })).toBe('workflow.ready.runtimeChecking')
    expect(readinessKeyOf(caps, both, { ...choice, load: 'unchecked' })).toBe('workflow.ready.runtimeUnchecked')
    const { load: _load, ...unasked } = choice
    expect(readinessKeyOf(caps, both, unasked)).toBe('workflow.ready.runtimeChecking')
    // A runtime that is not there is missing, whatever an old answer said.
    expect(readinessKeyOf({ ...caps, runtimeInstalled: false }, both, { ...choice, load: 'loaded' })).toBe('workflow.ready.runtime')
    expect(t('workflow.ready.runtimeUnloadable', { reasonKey: 'diagnostics.runtime.missingDependency' }))
      .toBe('ONNX Runtime is installed but does not load. The Microsoft Visual C++ 2015-2022 Redistributable (x64) is not installed, so the ONNX Runtime cannot load. Install it from Microsoft and start Manga Cleaner again.')
  })

  it('checks only what the workflow needs', () => {
    expect(readinessKeyOf({ ...caps, samInstalled: false }, { needs: ['rt'] }, choice)).toBeNull()
    expect(readinessKeyOf({ ...caps, fullRtInstalled: false }, { needs: ['sam'] }, choice)).toBeNull()
  })
})

describe('hitTest', () => {
  const evidence = {
    components: [
      { id: 'sam-00001', bounds: { x: 10, y: 10, w: 40, h: 40 } },
      { id: 'sam-00002', bounds: { x: 20, y: 20, w: 2, h: 2 } },
    ],
    regions: [{ id: 'rt-0000', bounds: { x: 0, y: 0, w: 100, h: 100 } }],
  }

  it('prefers the smallest component that holds the point', () => {
    expect(hitTest({ x: 20, y: 21 }, evidence)).toBe('sam-00002')
    expect(hitTest({ x: 30, y: 30 }, evidence)).toBe('sam-00001')
  })

  it('widens tiny boxes by the display tolerance and falls back to detector boxes', () => {
    expect(hitTest({ x: 8, y: 30 }, evidence)).toBe('rt-0000')
    expect(hitTest({ x: 8, y: 30 }, evidence, 3)).toBe('sam-00001')
    expect(hitTest({ x: 200, y: 200 }, evidence)).toBeNull()
  })
})
