import { describe, expect, it, vi } from 'vitest'

import { createMockBackend } from './mock.js'
import {
  createTauriBackend,
  isCloudExecutionRegistered,
  TAURI_REGISTERED_CLOUD_COMMANDS,
  TAURI_PENDING_CLOUD_COMMANDS,
} from './tauri.js'

describe('P3c1 cloud inference and secrets frontend API mapping', () => {
  describe('Tauri Adapter command mapping & zero-fallback', () => {
    function createFallbackTracker() {
      const calls = []
      const fallback = {
        calls,
        readInferenceConfig: vi.fn((...args) => {
          calls.push({ method: 'readInferenceConfig', args })
          return Promise.resolve({ fallback: true })
        }),
        writeInferenceConfig: vi.fn((...args) => {
          calls.push({ method: 'writeInferenceConfig', args })
          return Promise.resolve({ fallback: true })
        }),
        storeCloudSecret: vi.fn((...args) => {
          calls.push({ method: 'storeCloudSecret', args })
          return Promise.resolve({ fallback: true })
        }),
        deleteCloudSecret: vi.fn((...args) => {
          calls.push({ method: 'deleteCloudSecret', args })
          return Promise.resolve({ fallback: true })
        }),
        getCloudSecretSummary: vi.fn((...args) => {
          calls.push({ method: 'getCloudSecretSummary', args })
          return Promise.resolve({ fallback: true })
        }),
        readSettings: vi.fn(() => Promise.resolve({})),
        subscribe: vi.fn(() => () => {}),
      }
      return fallback
    }

    it('spies invoke for read_inference_config with exact argument mapping', async () => {
      const publicConfig = {
        schemaVersion: 1,
        selectedTarget: { type: 'local' },
        beamProfiles: {},
        modalProfiles: {},
      }
      const invoke = vi.fn().mockResolvedValue(publicConfig)
      const fallback = createFallbackTracker()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.readInferenceConfig()
      expect(invoke).toHaveBeenCalledTimes(1)
      expect(invoke).toHaveBeenCalledWith('read_inference_config')
      expect(result).toEqual(publicConfig)
      expect(fallback.calls).toHaveLength(0)
    })

    it('spies invoke for write_inference_config with { config } argument mapping', async () => {
      const config = {
        schemaVersion: 1,
        selectedTarget: { type: 'modal', profile_id: 'modal-worker-1' },
        beamProfiles: {},
        modalProfiles: {
          'modal-worker-1': {
            id: 'modal-worker-1',
            name: 'Modal GPU',
            endpointUrl: 'https://cleaner.modal.run/v1',
            canonicalOrigin: 'https://cleaner.modal.run',
            canonicalOriginFingerprint: '9f8e7d6c',
            createdAtMs: 12345,
            updatedAtMs: 67890,
          },
        },
      }
      const invoke = vi.fn().mockResolvedValue(config)
      const fallback = createFallbackTracker()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.writeInferenceConfig({ config })
      expect(invoke).toHaveBeenCalledTimes(1)
      expect(invoke).toHaveBeenCalledWith('write_inference_config', { config })
      expect(result).toEqual(config)
      expect(JSON.stringify(result)).not.toContain('secret')
      expect(fallback.calls).toHaveLength(0)
    })

    it('spies invoke for store_cloud_secret with exact argument mapping', async () => {
      const summary = {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        present: true,
        backend: 'keyring',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = createFallbackTracker()
      const backend = createTauriBackend({ fallback, invoke })

      const spec = {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        secret: 'raw-bearer-token-sec-12345',
        sessionOnly: true,
      }
      const result = await backend.storeCloudSecret(spec)

      expect(invoke).toHaveBeenCalledTimes(1)
      expect(invoke).toHaveBeenCalledWith('store_cloud_secret', {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        secret: 'raw-bearer-token-sec-12345',
        sessionOnly: true,
      })
      expect(result).toEqual(summary)
      expect(fallback.calls).toHaveLength(0)
    })

    it('spies invoke for delete_cloud_secret with exact argument mapping', async () => {
      const summary = {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
        present: false,
        backend: 'keyring',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = createFallbackTracker()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.deleteCloudSecret({
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
      })

      expect(invoke).toHaveBeenCalledTimes(1)
      expect(invoke).toHaveBeenCalledWith('delete_cloud_secret', {
        provider: 'beam',
        profileId: 'beam-prod',
        role: 'runtime',
      })
      expect(result).toEqual(summary)
      expect(fallback.calls).toHaveLength(0)
    })

    it('spies invoke for get_cloud_secret_summary with exact argument mapping', async () => {
      const summary = {
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'setup',
        present: true,
        backend: 'session',
      }
      const invoke = vi.fn().mockResolvedValue(summary)
      const fallback = createFallbackTracker()
      const backend = createTauriBackend({ fallback, invoke })

      const result = await backend.getCloudSecretSummary({
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'setup',
      })

      expect(invoke).toHaveBeenCalledTimes(1)
      expect(invoke).toHaveBeenCalledWith('get_cloud_secret_summary', {
        provider: 'modal',
        profileId: 'modal-dev',
        role: 'setup',
      })
      expect(result).toEqual(summary)
      expect(fallback.calls).toHaveLength(0)
    })

    it('never falls back to mock when an invoke rejects (fails closed)', async () => {
      const invoke = vi.fn().mockRejectedValue(new Error('OS credential store access denied'))
      const fallback = createFallbackTracker()
      const backend = createTauriBackend({ fallback, invoke })

      await expect(backend.readInferenceConfig()).rejects.toThrow('OS credential store access denied')
      await expect(
        backend.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'local' },
            beamProfiles: {},
            modalProfiles: {},
          },
        }),
      ).rejects.toThrow('OS credential store access denied')
      await expect(
        backend.storeCloudSecret({
          provider: 'beam',
          profileId: 'beam-1',
          role: 'runtime',
          secret: 's1',
        }),
      ).rejects.toThrow('OS credential store access denied')
      await expect(
        backend.deleteCloudSecret({
          provider: 'beam',
          profileId: 'beam-1',
          role: 'runtime',
        }),
      ).rejects.toThrow('OS credential store access denied')
      await expect(
        backend.getCloudSecretSummary({
          provider: 'beam',
          profileId: 'beam-1',
          role: 'runtime',
        }),
      ).rejects.toThrow('OS credential store access denied')

      expect(fallback.calls).toHaveLength(0)
    })
  })

  describe('Browser Mock backend behavior', () => {
    it('initializes default in-memory config with local execution target and empty profile maps', async () => {
      const mock = createMockBackend({ timing: { method: 0 } })
      const config = await mock.readInferenceConfig()

      expect(config).toEqual({
        schemaVersion: 1,
        selectedTarget: { type: 'local' },
        beamProfiles: {},
        modalProfiles: {},
      })
    })

    it('stores isolated clone of public config in-memory without secrets', async () => {
      const mock = createMockBackend({ timing: { method: 0 } })
      const customConfig = {
        schemaVersion: 1,
        selectedTarget: { type: 'beam', profile_id: 'beam-staging' },
        beamProfiles: {
          'beam-staging': {
            id: 'beam-staging',
            name: 'Beam Staging',
            endpointUrl: 'https://staging.beam.cloud/ep',
            canonicalOrigin: 'https://staging.beam.cloud',
            canonicalOriginFingerprint: '1122334455',
            createdAtMs: 500,
            updatedAtMs: 600,
          },
        },
        modalProfiles: {},
      }

      const written = await mock.writeInferenceConfig({ config: customConfig })
      expect(written).toEqual(customConfig)
      expect(written).not.toBe(customConfig) // must be an isolated clone

      // Modifying caller input does not alter stored mock state
      customConfig.beamProfiles['beam-staging'].name = 'Mutated After Write'
      const readBack = await mock.readInferenceConfig()
      expect(readBack.beamProfiles['beam-staging'].name).toBe('Beam Staging')
      expect(JSON.stringify(readBack)).not.toContain('secret')
    })

    it('rejects adversarial extra keys and raw secrets injected into writeInferenceConfig', async () => {
      const mock = createMockBackend({ timing: { method: 0 } })

      // Injected top-level secret
      await expect(
        mock.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'local' },
            beamProfiles: {},
            modalProfiles: {},
            secret: 'top-secret-token',
          },
        }),
      ).rejects.toThrow(/unrecognized inference config field 'secret'/)

      // Injected profile-level secret
      await expect(
        mock.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'beam', profile_id: 'beam-1' },
            beamProfiles: {
              'beam-1': {
                id: 'beam-1',
                name: 'Beam Worker',
                endpointUrl: 'https://api.beam.cloud/v1',
                canonicalOrigin: 'https://api.beam.cloud',
                canonicalOriginFingerprint: 'fp',
                createdAtMs: 10,
                updatedAtMs: 10,
                apiKey: 'leaked-api-key',
              },
            },
            modalProfiles: {},
          },
        }),
      ).rejects.toThrow(/unrecognized field 'apiKey' in beam profile 'beam-1'/)

      // Injected target-level secret
      await expect(
        mock.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'beam', profile_id: 'beam-1', secret: 'leaked-target-key' },
            beamProfiles: {},
            modalProfiles: {},
          },
        }),
      ).rejects.toThrow(/unrecognized field 'secret' in beam execution target/)

      const clean = await mock.readInferenceConfig()
      expect(JSON.stringify(clean)).not.toContain('top-secret-token')
      expect(JSON.stringify(clean)).not.toContain('leaked')
    })

    it('keeps a session-only secret store that answers presence and never the value', async () => {
      const mock = createMockBackend({ timing: { method: 0 } })
      const key = { provider: 'beam', profileId: 'beam-1', role: 'runtime' }

      expect(await mock.getCloudSecretSummary(key)).toEqual({ ...key, present: false, backend: 'session' })
      const stored = await mock.storeCloudSecret({ ...key, secret: 'token-abc' })
      expect(stored).toEqual({ ...key, present: true, backend: 'session' })
      expect(JSON.stringify(stored)).not.toContain('token-abc')
      expect(await mock.getCloudSecretSummary(key)).toMatchObject({ present: true })
      expect(await mock.deleteCloudSecret(key)).toMatchObject({ present: false })

      await expect(mock.storeCloudSecret({ ...key, secret: '   ' })).rejects.toThrow(/must not be empty/)
      await expect(mock.getCloudSecretSummary({ ...key, role: 'owner' })).rejects.toThrow(/unknown secret role/)
      await expect(mock.getCloudSecretSummary({ ...key, provider: 'google' })).rejects.toThrow(/unknown cloud provider/)
    })

    describe('P3/P4 deterministic mock cloud authorization and attempt lifecycle', () => {
      async function setupMockWithProfiles() {
        const mock = createMockBackend({ timing: { method: 0 } })
        await mock.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'modal', profile_id: 'modal-prod' },
            beamProfiles: {
              'beam-prod': {
                id: 'beam-prod',
                name: 'Beam Production',
                endpointUrl: 'https://api.beam.cloud/v1',
                canonicalOrigin: 'https://api.beam.cloud',
                canonicalOriginFingerprint: 'beam-fp-1',
                createdAtMs: 1000,
                updatedAtMs: 1000,
              },
            },
            modalProfiles: {
              'modal-prod': {
                id: 'modal-prod',
                name: 'Modal Production',
                endpointUrl: 'https://modal.run/v1',
                canonicalOrigin: 'https://modal.run',
                canonicalOriginFingerprint: 'modal-fp-1',
                createdAtMs: 2000,
                updatedAtMs: 2000,
              },
              'modal-offline': {
                id: 'modal-offline',
                name: 'Modal Offline',
                endpointUrl: 'https://modal-offline.run/v1',
                canonicalOrigin: 'https://modal-offline.run',
                canonicalOriginFingerprint: 'modal-fp-2',
                createdAtMs: 2000,
                updatedAtMs: 2000,
              },
            },
          },
        })
        return mock
      }

      async function createTestGrant(mock, profileId = 'modal-prod') {
        const proposal = await mock.prepareCloudConsent({
          target: { type: 'modal', profile_id: profileId },
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
        })
        return await mock.confirmCloudConsent({
          proposalId: proposal.proposalId,
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
          rightsAttested: true,
          retentionAcknowledged: true,
        })
      }

      it('checks connection reachability without triggering GPU work or model download', async () => {
        const mock = await setupMockWithProfiles()

        // No runtime token yet: the check does not reach out at all.
        const tokenless = await mock.checkCloudConnection({ provider: 'modal', profileId: 'modal-prod' })
        expect(tokenless).toEqual({ ok: false, status: 'credential_missing', provider: 'modal', profileId: 'modal-prod' })

        for (const profileId of ['modal-prod', 'modal-offline']) {
          await mock.storeCloudSecret({ provider: 'modal', profileId, role: 'runtime', secret: 'runtime-secret', tokenId: 'wk-1' })
        }
        const reachable = await mock.checkCloudConnection({ provider: 'modal', profileId: 'modal-prod' })
        expect(reachable).toEqual({
          ok: true,
          status: 'reachable',
          provider: 'modal',
          profileId: 'modal-prod',
          latencyMs: expect.any(Number),
        })

        // Unreachable endpoint reports failure gracefully without throwing
        const offline = await mock.checkCloudConnection({ provider: 'modal', profileId: 'modal-offline' })
        expect(offline.ok).toBe(false)
        expect(offline.status).toBe('unreachable')

        // Non-existent profile fails closed
        await expect(
          mock.checkCloudConnection({ provider: 'modal', profileId: 'non-existent' }),
        ).rejects.toThrow(/does not exist/)
      })

      it('queries model info and returns wire limits', async () => {
        const mock = await setupMockWithProfiles()
        const info = await mock.getCloudModelInfo({ provider: 'beam', profileId: 'beam-prod' })

        expect(info.supportedProtocolVersion).toBe('1.0.0')
        expect(info.pinnedModelId).toBe('Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic')
        expect(info.pinnedRecipeId).toBe('mc-flux2-klein-inpaint-v1')
        expect(info.preprocessingVersion).toBe('2.0.0')
        expect(info.nativeMaskConditioning).toBe(false)
        expect(info.limits.maxDimensions).toEqual([2048, 2048])
        expect(info.limits.maxMegapixels).toBe(4.19)
        expect(info.limits.defaultWorkerDeadlineSec).toBe(120)
      })

      it('exercises blocked authorization when cloudEngines is not allowed', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'blocked' })

        await expect(
          mock.prepareCloudConsent({
            target: { type: 'modal', profile_id: 'modal-prod' },
            intent: { action: 'applyTool', tool: 'contentAwareFill' },
          }),
        ).rejects.toThrow(/Backend authorization blocked/)
      })

      it('prepares proposal and validates epoch match on confirm', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const proposal = await mock.prepareCloudConsent({
          target: { type: 'modal', profile_id: 'modal-prod' },
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
          regionRevision: 3,
        })

        expect(proposal.proposalId).toMatch(/^prop-/)
        expect(proposal.provider).toBe('modal')
        expect(proposal.profileId).toBe('modal-prod')
        expect(proposal.estimatedCostUsd).toBeNull() // Unknown cost stays null
        expect(proposal.cropSha256).toHaveLength(64)
        expect(proposal.hintSha256).toHaveLength(64)
        expect(proposal.sourceHash).toHaveLength(64)
        expect(proposal.maskHash).toHaveLength(64)

        // Confirming with matching intent mints a scoped grant
        const grant = await mock.confirmCloudConsent({
          proposalId: proposal.proposalId,
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
          rightsAttested: true,
          retentionAcknowledged: true,
        })

        expect(grant.nonce).toMatch(/^grant-/)
        expect(grant.scope.cropSha256).toBe(proposal.cropSha256)
        expect(grant.scope.revision).toBe(3)
        expect(grant.allowedAttempts).toBe(1)

        // Single-use consumption: confirming again fails
        await expect(
          mock.confirmCloudConsent({
            proposalId: proposal.proposalId,
            intent: { action: 'applyTool', tool: 'contentAwareFill' },
          }),
        ).rejects.toThrow(/already consumed/)
      })

      it('fails proposal confirmation when profile is mutated (epoch mismatch)', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const proposal = await mock.prepareCloudConsent({
          target: { type: 'modal', profile_id: 'modal-prod' },
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
        })

        // Profile mutation triggers epoch advancement and proposal invalidation
        const config = await mock.readInferenceConfig()
        config.modalProfiles['modal-prod'].name = 'Updated Name'
        await mock.writeInferenceConfig({ config })

        await expect(
          mock.confirmCloudConsent({
            proposalId: proposal.proposalId,
            intent: { action: 'applyTool', tool: 'contentAwareFill' },
          }),
        ).rejects.toThrow(/not found|epoch mismatch/)
      })

      it('fails proposal confirmation when intent mismatches', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const proposal = await mock.prepareCloudConsent({
          target: { type: 'modal', profile_id: 'modal-prod' },
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
        })

        await expect(
          mock.confirmCloudConsent({
            proposalId: proposal.proposalId,
            intent: { action: 'createRegion', tool: 'brush' },
          }),
        ).rejects.toThrow(/Intent mismatch/)
      })

      it('exercises ambiguous acceptance: marks unknown and strictly prevents auto-retry', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const grant = await createTestGrant(mock)
        const attemptId = 'attempt-ambiguous-1'
        const submission = await mock.submitCloudAttempt({
          attemptId,
          grantNonce: grant.nonce,
          simulateMode: 'ambiguous_acceptance',
          snapshot: { regionRevision: 5, sourceImageHash: 'img-sha256-abc' },
        })

        expect(submission.status).toBe('unknown')
        expect(submission.handle).toBeNull()
        expect(submission.autoRetryable).toBe(false)

        // Querying status reflects unknown
        const status = await mock.getCloudAttemptStatus({ attemptId })
        expect(status.status).toBe('unknown')
        expect(status.reportedCostUsd).toBeNull()

        // Reconcile recovery evaluates as ambiguous_unknown and refuses auto-retry
        const recovery = await mock.reconcileCloudRecovery({ attemptId })
        expect(recovery.decision).toBe('ambiguous_unknown')
        expect(recovery.autoRetryable).toBe(false)
      })

      it('exercises known-handle recovery: resumes polling without fresh submission', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const grant = await createTestGrant(mock)
        const attemptId = 'attempt-known-handle-1'
        const submission = await mock.submitCloudAttempt({
          attemptId,
          grantNonce: grant.nonce,
          snapshot: { regionRevision: 2, sourceImageHash: 'img-sha256-xyz' },
        })

        expect(submission.status).toBe('accepted')
        expect(submission.handle).toBe('handle-mock-attempt-known-handle-1')

        // Recovery discovers the accepted attempt and reuses known handle
        const recovery = await mock.reconcileCloudRecovery({ attemptId })
        expect(recovery.decision).toBe('resume_polling')
        expect(recovery.handle).toBe(submission.handle)
      })

      it('exercises nonterminal cancellation: acknowledgement does not force terminal cancelled', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const grant = await createTestGrant(mock)
        const attemptId = 'attempt-cancel-1'
        const submission = await mock.submitCloudAttempt({
          attemptId,
          grantNonce: grant.nonce,
          snapshot: { regionRevision: 1, sourceImageHash: 'img-sha' },
        })

        const cancelAck = await mock.cancelCloudAttempt({ attemptId, handle: submission.handle })
        expect(cancelAck.status).toBe('cancel_requested')
        expect(cancelAck.acknowledged).toBe(true)

        // Status inquiry continues to reflect nonterminal cancel_requested
        const status = await mock.getCloudAttemptStatus({ attemptId })
        expect(status.status).toBe('cancel_requested')
        expect(status.acknowledged).toBe(true)

        // Recovery resumes cancel polling
        const recovery = await mock.reconcileCloudRecovery({ attemptId })
        expect(recovery.decision).toBe('resume_cancel_polling')
        expect(recovery.handle).toBe(submission.handle)
      })

      it('exercises stale attachment: retains cached result while rejecting attachment on snapshot drift', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const grant = await createTestGrant(mock)
        const attemptId = 'attempt-stale-1'
        const submission = await mock.submitCloudAttempt({
          attemptId,
          grantNonce: grant.nonce,
          snapshot: { regionRevision: 1, sourceImageHash: 'source-hash-orig' },
        })

        // Poll until completed
        await mock.getCloudAttemptStatus({ attemptId })
        await mock.getCloudAttemptStatus({ attemptId })

        // Snapshot matched -> ready for attach
        const readyRecovery = await mock.reconcileCloudRecovery({
          attemptId,
          regionRevision: 1,
          sourceImageHash: 'source-hash-orig',
        })
        expect(readyRecovery.decision).toBe('result_cached_ready')
        expect(readyRecovery.handle).toBe(submission.handle)

        // Snapshot drifted (region revision modified or image hash drifted) -> stale attachment rejected
        const staleRecovery = await mock.reconcileCloudRecovery({
          attemptId,
          regionRevision: 2, // modified locally
          sourceImageHash: 'source-hash-orig',
        })
        expect(staleRecovery.decision).toBe('stale_attachment')
        expect(staleRecovery.resultDigest).toBeTruthy() // result retained for review!
      })

      it('exercises retry-safe, idempotent result retrieval without triggering fresh GPU work', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const grant = await createTestGrant(mock)
        const attemptId = 'attempt-retry-safe-1'
        const submission = await mock.submitCloudAttempt({
          attemptId,
          grantNonce: grant.nonce,
          snapshot: { regionRevision: 1, sourceImageHash: 'img-hash' },
        })

        // Complete the job
        await mock.getCloudAttemptStatus({ attemptId })
        await mock.getCloudAttemptStatus({ attemptId })

        // Retrieve result once
        const res1 = await mock.getCloudAttemptResult({ attemptId, handle: submission.handle })
        expect(res1.attemptId).toBe(attemptId)
        expect(res1.cached).toBe(true)
        expect(res1.reportedCostUsd).toBeNull()

        // Retrieve result second time (retry-safe, idempotent)
        const res2 = await mock.getCloudAttemptResult({ attemptId, handle: submission.handle })
        expect(res2).toEqual(res1)
      })

      it('uses the immutable pinned revision in getCloudModelInfo and default consent recipe (never mutable main)', async () => {
        const mock = await setupMockWithProfiles()
        const info = await mock.getCloudModelInfo({ provider: 'beam', profileId: 'beam-prod' })
        expect(info.pinnedModelRevision).toBe('45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd')
        expect(info.pinnedModelRevision).not.toBe('main')

        await mock.writeSettings({ cloudEngines: 'allowed' })
        const proposal = await mock.prepareCloudConsent({
          target: { type: 'modal', profile_id: 'modal-prod' },
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
        })
        expect(proposal.recipe.model_revision).toBe('45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd')
        expect(proposal.recipe.model_revision).not.toBe('main')

        const grant = await mock.confirmCloudConsent({
          proposalId: proposal.proposalId,
          intent: { action: 'applyTool', tool: 'contentAwareFill' },
          rightsAttested: true,
          retentionAcknowledged: true,
        })
        expect(grant.scope.recipe.model_revision).toBe('45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd')
      })

      it('fails closed when grantNonce is missing, empty, or unknown', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        // Missing grantNonce
        await expect(
          mock.submitCloudAttempt({ attemptId: 'att-missing-grant' }),
        ).rejects.toThrow('Authorization blocked: invalid or missing grant')

        // Null grantNonce
        await expect(
          mock.submitCloudAttempt({ attemptId: 'att-null-grant', grantNonce: null }),
        ).rejects.toThrow('Authorization blocked: invalid or missing grant')

        // Empty string grantNonce
        await expect(
          mock.submitCloudAttempt({ attemptId: 'att-empty-grant', grantNonce: '' }),
        ).rejects.toThrow('Authorization blocked: invalid or missing grant')

        // Whitespace string grantNonce
        await expect(
          mock.submitCloudAttempt({ attemptId: 'att-ws-grant', grantNonce: '   ' }),
        ).rejects.toThrow('Authorization blocked: invalid or missing grant')

        // Unknown / non-existent grantNonce
        await expect(
          mock.submitCloudAttempt({ attemptId: 'att-unknown-grant', grantNonce: 'grant-unknown-12345' }),
        ).rejects.toThrow('Authorization blocked: invalid or missing grant')
      })

      it('atomically enforces allowedAttempts and increments usedAttempts so replay cannot submit', async () => {
        const mock = await setupMockWithProfiles()
        await mock.writeSettings({ cloudEngines: 'allowed' })

        const grant = await createTestGrant(mock)
        expect(grant.allowedAttempts).toBe(1)
        expect(grant.usedAttempts).toBe(0)

        // First attempt with valid grantNonce succeeds
        const first = await mock.submitCloudAttempt({
          attemptId: 'att-first-submission',
          grantNonce: grant.nonce,
          snapshot: { regionRevision: 1, sourceImageHash: 'src-1' },
        })
        expect(first.status).toBe('accepted')

        // Second attempt replaying the same grantNonce is rejected
        await expect(
          mock.submitCloudAttempt({
            attemptId: 'att-replayed-submission',
            grantNonce: grant.nonce,
            snapshot: { regionRevision: 1, sourceImageHash: 'src-1' },
          }),
        ).rejects.toThrow('Authorization blocked: grant attempt limit reached (replay detected)')

        // Replay is also rejected following ambiguous acceptance
        const ambiguousGrant = await createTestGrant(mock)
        const ambSub = await mock.submitCloudAttempt({
          attemptId: 'att-amb-first',
          grantNonce: ambiguousGrant.nonce,
          simulateMode: 'ambiguous_acceptance',
          snapshot: { regionRevision: 1, sourceImageHash: 'src-1' },
        })
        expect(ambSub.status).toBe('unknown')

        await expect(
          mock.submitCloudAttempt({
            attemptId: 'att-amb-replay',
            grantNonce: ambiguousGrant.nonce,
            snapshot: { regionRevision: 1, sourceImageHash: 'src-1' },
          }),
        ).rejects.toThrow('Authorization blocked: grant attempt limit reached (replay detected)')
      })
    })

    describe('P3/P4 Tauri adapter mappings and fail-closed behavior for all lifecycle commands', () => {
      it('maps all 9 lifecycle commands to exact Tauri IPC names with zero mock fallback', async () => {
        const invoke = vi.fn().mockImplementation(async (cmd, args) => {
          return { command: cmd, args, ok: true }
        })
        const fallback = {
          checkCloudConnection: vi.fn(),
          getCloudModelInfo: vi.fn(),
          prepareCloudConsent: vi.fn(),
          confirmCloudConsent: vi.fn(),
          submitCloudAttempt: vi.fn(),
          getCloudAttemptStatus: vi.fn(),
          getCloudAttemptResult: vi.fn(),
          cancelCloudAttempt: vi.fn(),
          reconcileCloudRecovery: vi.fn(),
          readSettings: vi.fn(() => Promise.resolve({})),
          subscribe: vi.fn(() => () => {}),
        }
        const backend = createTauriBackend({ fallback, invoke })

        await backend.checkCloudConnection({ provider: 'modal', profileId: 'm1' })
        expect(invoke).toHaveBeenLastCalledWith('check_cloud_connection', { provider: 'modal', profileId: 'm1' })

        await backend.getCloudModelInfo({ provider: 'beam', profileId: 'b1' })
        expect(invoke).toHaveBeenLastCalledWith('get_cloud_model_info', { provider: 'beam', profileId: 'b1' })

        await backend.prepareCloudConsent({ target: { type: 'modal', profile_id: 'm1' }, intent: { action: 'applyTool' } })
        expect(invoke).toHaveBeenLastCalledWith('prepare_cloud_consent', { target: { type: 'modal', profile_id: 'm1' }, intent: { action: 'applyTool' } })

        await backend.confirmCloudConsent({ proposalId: 'p1', intent: { action: 'applyTool' } })
        expect(invoke).toHaveBeenLastCalledWith('confirm_cloud_consent', { proposalId: 'p1', intent: { action: 'applyTool' } })

        await backend.submitCloudAttempt({ attemptId: 'att1' })
        expect(invoke).toHaveBeenLastCalledWith('submit_cloud_attempt', { attemptId: 'att1' })

        await backend.getCloudAttemptStatus({ attemptId: 'att1', handle: 'h1' })
        expect(invoke).toHaveBeenLastCalledWith('get_cloud_attempt_status', { attemptId: 'att1', handle: 'h1' })

        await backend.getCloudAttemptResult({ attemptId: 'att1', handle: 'h1' })
        expect(invoke).toHaveBeenLastCalledWith('get_cloud_attempt_result', { attemptId: 'att1', handle: 'h1' })

        await backend.cancelCloudAttempt({ attemptId: 'att1', handle: 'h1' })
        expect(invoke).toHaveBeenLastCalledWith('cancel_cloud_attempt', { attemptId: 'att1', handle: 'h1' })

        await backend.reconcileCloudRecovery({ attemptId: 'att1' })
        expect(invoke).toHaveBeenLastCalledWith('reconcile_cloud_recovery', { attemptId: 'att1' })

        // Proves zero fallback calls: all 9 commands went straight to invoke
        expect(fallback.checkCloudConnection).not.toHaveBeenCalled()
        expect(fallback.getCloudModelInfo).not.toHaveBeenCalled()
        expect(fallback.prepareCloudConsent).not.toHaveBeenCalled()
        expect(fallback.confirmCloudConsent).not.toHaveBeenCalled()
        expect(fallback.submitCloudAttempt).not.toHaveBeenCalled()
        expect(fallback.getCloudAttemptStatus).not.toHaveBeenCalled()
        expect(fallback.getCloudAttemptResult).not.toHaveBeenCalled()
        expect(fallback.cancelCloudAttempt).not.toHaveBeenCalled()
        expect(fallback.reconcileCloudRecovery).not.toHaveBeenCalled()
      })

      it('propagates invoke rejection directly without falling back to mock (never fake success)', async () => {
        const invoke = vi.fn().mockRejectedValue(new Error('Tauri command not registered'))
        const fallback = {
          checkCloudConnection: vi.fn().mockResolvedValue({ fake: true }),
          getCloudModelInfo: vi.fn().mockResolvedValue({ fake: true }),
          prepareCloudConsent: vi.fn().mockResolvedValue({ fake: true }),
          confirmCloudConsent: vi.fn().mockResolvedValue({ fake: true }),
          submitCloudAttempt: vi.fn().mockResolvedValue({ fake: true }),
          getCloudAttemptStatus: vi.fn().mockResolvedValue({ fake: true }),
          getCloudAttemptResult: vi.fn().mockResolvedValue({ fake: true }),
          cancelCloudAttempt: vi.fn().mockResolvedValue({ fake: true }),
          reconcileCloudRecovery: vi.fn().mockResolvedValue({ fake: true }),
          readSettings: vi.fn(() => Promise.resolve({})),
          subscribe: vi.fn(() => () => {}),
        }
        const backend = createTauriBackend({ fallback, invoke })

        await expect(backend.checkCloudConnection({ provider: 'modal', profileId: 'm1' })).rejects.toThrow('Tauri command not registered')
        await expect(backend.getCloudModelInfo({ provider: 'beam', profileId: 'b1' })).rejects.toThrow('Tauri command not registered')
        await expect(backend.prepareCloudConsent({ target: { type: 'modal', profile_id: 'm1' }, intent: { action: 'applyTool' } })).rejects.toThrow('Tauri command not registered')
        await expect(backend.confirmCloudConsent({ proposalId: 'p1', intent: { action: 'applyTool' } })).rejects.toThrow('Tauri command not registered')
        await expect(backend.submitCloudAttempt({ attemptId: 'att1' })).rejects.toThrow('Tauri command not registered')
        await expect(backend.getCloudAttemptStatus({ attemptId: 'att1' })).rejects.toThrow('Tauri command not registered')
        await expect(backend.getCloudAttemptResult({ attemptId: 'att1' })).rejects.toThrow('Tauri command not registered')
        await expect(backend.cancelCloudAttempt({ attemptId: 'att1' })).rejects.toThrow('Tauri command not registered')
        await expect(backend.reconcileCloudRecovery({})).rejects.toThrow('Tauri command not registered')

        expect(fallback.checkCloudConnection).not.toHaveBeenCalled()
        expect(fallback.prepareCloudConsent).not.toHaveBeenCalled()
        expect(fallback.submitCloudAttempt).not.toHaveBeenCalled()
      })

      it('truthfully reports that remote execution is registered now that lifecycle commands are implemented', () => {
        expect(isCloudExecutionRegistered()).toBe(true)
        expect(TAURI_REGISTERED_CLOUD_COMMANDS).toContain('read_inference_config')
        expect(TAURI_REGISTERED_CLOUD_COMMANDS).toContain('write_inference_config')
        expect(TAURI_REGISTERED_CLOUD_COMMANDS).toContain('store_cloud_secret')
        expect(TAURI_REGISTERED_CLOUD_COMMANDS).toContain('prepare_cloud_consent')
        expect(TAURI_REGISTERED_CLOUD_COMMANDS).toContain('confirm_cloud_consent')
        expect(TAURI_REGISTERED_CLOUD_COMMANDS).toContain('submit_cloud_attempt')
        expect(TAURI_PENDING_CLOUD_COMMANDS).toHaveLength(0)
      })
    })
  })
})
