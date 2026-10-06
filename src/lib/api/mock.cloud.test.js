/**
 * The browser mock's cloud half, held to the native side's contracts: the
 * provisioner's progress events and its IC-1 answer (IC-1, IC-2), a cloud
 * render's phase events, its attempt id and its cancel (IC-3), and the
 * start-up recovery shape (IC-4). The interface is built against these, so a
 * mock that drifted from them would make every browser check a lie.
 */

import { createHash } from 'node:crypto'
import { describe, expect, it, vi } from 'vitest'

import { cloudAttemptId, sha256Hex } from './attempt.js'
import { readCloudReadiness } from './backend.js'
import { createMockBackend, MOCK_PROVISION_GPUS, PROVISION_APPLY_STEPS } from './mock.js'
import { rerunNeedsCloud } from './tools.js'
import { DENOISE_PRESETS } from '../model/denoise.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const MODAL_KEYS = { token_id: 'ak-fake', token_secret: 'as-fake' }

function makeMock() {
  return createMockBackend({ timing: { method: 0, cloud: 0, provision: 0, region: 0, pageTail: 0 } })
}

/** A plan for `provider`, then its approval, as the Review step does it. */
async function planned(mock, provider, installationId, options = {}) {
  const credentials = provider === 'modal' ? MODAL_KEYS : { beam_token: 'beam-fake' }
  const plan = await mock.runCloudProvisioner({
    op: 'plan',
    provider,
    params: { credentials, installation_id: installationId, options },
  })
  expect(plan.success).toBe(true)
  return { credentials, plan: plan.data }
}

async function firstRegion(mock) {
  const pages = await mock.loadPages({ chapterId: CHAPTER, indices: [0, 1, 2, 3, 4, 5] })
  const page = pages.find((candidate) => candidate.regions.length > 0)
  return { page, region: page.regions[0] }
}

/** A mock with cloud allowed and a provisioned Modal endpoint selected. */
async function readyMock() {
  const mock = makeMock()
  await mock.writeSettings({ cloudEngines: 'allowed' })
  const { credentials, plan } = await planned(mock, 'modal', 'mc-ready1')
  const applied = await mock.runCloudProvisioner({
    op: 'apply',
    provider: 'modal',
    params: { credentials, installation_id: 'mc-ready1', approved_plan_hash: plan.plan_hash },
  })
  expect(applied.success).toBe(true)
  return mock
}

/** Consent for one action on one region, as `requestCloudConsent` asks for it. */
async function grantFor(mock, regionId, intent) {
  const config = await mock.readInferenceConfig()
  const target = config.selectedTarget
  const proposal = await mock.prepareCloudConsent({ target, intent, regionId, chapterId: CHAPTER, pageIndex: 0 })
  const grant = await mock.confirmCloudConsent({ proposalId: proposal.proposalId, intent, rightsAttested: true,
    retentionAcknowledged: true })
  return {
    proposal,
    params: { grantNonce: grant.nonce, executionTarget: target, recipe: proposal.recipe, intent },
  }
}

describe('standing cloud consent', () => {
  it('is recorded by a project\'s first grant and covers that project only', async () => {
    const mock = await readyMock()
    const { region } = await firstRegion(mock)
    const intent = { action: 'applyTool', tool: 'contentAwareFill' }
    const target = (await mock.readInferenceConfig()).selectedTarget
    const propose = (chapterId) => mock.prepareCloudConsent({ target, intent, regionId: '', chapterId, pageIndex: 0 })

    expect((await propose(CHAPTER)).standing).toBe(false)
    const { proposal } = await grantFor(mock, region.id, intent)
    expect(proposal.standing).toBe(false)
    expect((await propose(CHAPTER)).standing).toBe(true)
    expect((await propose('tsuki-to-hane-ch106')).standing).toBe(true)
    expect((await propose('wandering-moon-ch12')).standing).toBe(false)
  })

  it('needs both statements for a first consent, and none once the project stands', async () => {
    const mock = await readyMock()
    const intent = { action: 'applyTool', tool: 'contentAwareFill' }
    const target = (await mock.readInferenceConfig()).selectedTarget
    const propose = () => mock.prepareCloudConsent({ target, intent, regionId: '', chapterId: CHAPTER, pageIndex: 0 })
    const confirm = async (statements) => mock.confirmCloudConsent({
      proposalId: (await propose()).proposalId, intent, ...statements })

    await expect(confirm({})).rejects.toThrow('rights_attestation_required')
    await expect(confirm({ rightsAttested: true })).rejects.toThrow('retention_acknowledgement_required')
    expect((await propose()).standing).toBe(false)
    await confirm({ rightsAttested: true, retentionAcknowledged: true })
    expect((await propose()).standing).toBe(true)
    expect((await confirm({})).nonce).toMatch(/^grant-/)
  })

  it('keeps no Modal setup token after setup', async () => {
    const mock = await readyMock()
    const summary = await mock.getCloudSecretSummary({ provider: 'modal', profileId: 'mc-ready1', role: 'setup' })
    expect(summary.present).toBe(false)
  })
})

describe('attempt ids', () => {
  it('are att- and 24 hex digits of the SHA-256 of the grant nonce, as the native side derives them', () => {
    for (const nonce of ['grant-0123456789abcdef', '', 'nonce with spaces and ünïcode']) {
      const digest = createHash('sha256').update(nonce, 'utf8').digest('hex')
      expect(sha256Hex(nonce)).toBe(digest)
      expect(cloudAttemptId(nonce)).toBe(`att-${digest.slice(0, 24)}`)
    }
  })
})

describe('the mock provisioner', () => {
  it('plans with the GPUs each provider really offers', async () => {
    const mock = makeMock()
    const modal = (await planned(mock, 'modal', 'mc-gpu001')).plan
    expect(modal.resource_allocation.gpu).toBe('L4')
    expect(modal.resource_allocation.gpu_options).toEqual([...MOCK_PROVISION_GPUS.modal.options])
    expect(modal.plan_hash).toMatch(/^[0-9a-f]{64}$/)

    const beam = (await planned(mock, 'beam', 'mc-gpu002')).plan
    expect(beam.resource_allocation.gpu).toBe('RTX4090')
    expect(beam.resource_allocation.gpu_options).toEqual(['RTX4090', 'A10G', 'RTX5090'])
    expect(beam.resource_allocation.gpu_options).not.toContain('L4')
    expect(beam.runtime_credential_kind).toBe('beam_bearer')

    const refused = await mock.runCloudProvisioner({
      op: 'plan',
      provider: 'beam',
      params: { credentials: { beam_token: 'b' }, installation_id: 'mc-gpu003', options: { gpu: 'L4' } },
    })
    expect(refused.success).toBe(false)
    expect(refused.error.code).toBe('ERR_VALIDATION_ERROR')

    const other = (await planned(mock, 'modal', 'mc-gpu001', { gpu: 'A10', idle_seconds: 300 })).plan
    expect(other.resource_allocation).toMatchObject({ gpu: 'A10', idle_seconds: 300 })
    expect(other.plan_hash).not.toBe(modal.plan_hash)
  })

  it('plans Page denoise as the helper does: off by default, Modal only, a boolean', async () => {
    const mock = makeMock()
    const off = (await planned(mock, 'modal', 'mc-den001')).plan
    expect(off.resource_allocation).toMatchObject({ denoise: false, denoise_supported: true })
    expect(off.resource_allocation.denoise_models_bytes).toBeGreaterThan(500_000_000)
    const beam = (await planned(mock, 'beam', 'mc-den002')).plan
    expect(beam.resource_allocation).toMatchObject({ denoise: false, denoise_supported: false })

    const refuse = async (provider, denoise) => mock.runCloudProvisioner({
      op: 'plan',
      provider,
      params: { credentials: provider === 'modal' ? MODAL_KEYS : { beam_token: 'b' }, installation_id: 'mc-den003',
        options: { denoise } },
    })
    const onBeam = await refuse('beam', true)
    expect(onBeam.success).toBe(false)
    expect(onBeam.error).toMatchObject({ code: 'ERR_VALIDATION_ERROR', message: 'Page denoise is only available on Modal installations' })
    expect((await refuse('modal', 'yes')).error.code).toBe('ERR_VALIDATION_ERROR')

    const { credentials, plan } = await planned(mock, 'modal', 'mc-den001', { denoise: true })
    expect(plan.resource_allocation.denoise).toBe(true)
    expect(plan.plan_hash).not.toBe(off.plan_hash)
    const applied = await mock.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-den001', approved_plan_hash: plan.plan_hash },
    })
    expect(applied.success).toBe(true)
    expect(applied.data.denoise).toBe(true)
  })

  it('lists an installation another computer made with its recorded denoise choice', async () => {
    const mock = createMockBackend({ timing: { method: 0, cloud: 0, provision: 0, region: 0, pageTail: 0 }, cloudExisting: '1' })
    const inspected = await mock.runCloudProvisioner({ op: 'inspect', provider: 'modal',
      params: { credentials: MODAL_KEYS, installation_id: 'mc-den004' } })
    expect(inspected.data.existing_installations[0].options.denoise).toBe(false)
  })

  it('refuses a cloud denoise on a setup made with Page denoise off, and prepares one made with it on', async () => {
    const recipe = DENOISE_PRESETS.find((entry) => entry.targets.includes('cloud')).recipe
    for (const denoise of [false, true]) {
      const mock = makeMock()
      await mock.writeSettings({ cloudEngines: 'allowed' })
      const { credentials, plan } = await planned(mock, 'modal', 'mc-den005', { denoise })
      await mock.runCloudProvisioner({ op: 'apply', provider: 'modal',
        params: { credentials, installation_id: 'mc-den005', approved_plan_hash: plan.plan_hash } })
      const prepared = mock.prepareCloudDenoise({ chapterId: CHAPTER, recipe })
      if (denoise) await expect(prepared).resolves.toMatchObject({ pages: expect.any(Number) })
      else await expect(prepared).rejects.toThrow('capability_unavailable: gateway denoise is not configured')
    }
  })

  it('applies with IC-2 progress and answers IC-1 without the runtime credential', async () => {
    const mock = makeMock()
    const progress = []
    const unlisten = await mock.onProvisionProgress((event) => progress.push(event))
    const { credentials, plan } = await planned(mock, 'modal', 'mc-ab12cd')

    const unapproved = await mock.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-ab12cd', approved_plan_hash: '0'.repeat(64) },
    })
    expect(unapproved.error.code).toBe('ERR_UNAPPROVED_PLAN')
    expect(progress).toEqual([])

    const result = await mock.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-ab12cd', approved_plan_hash: plan.plan_hash },
    })
    unlisten()

    expect(result.success).toBe(true)
    expect(result.data).not.toHaveProperty('runtime_credential')
    expect(JSON.stringify(result)).not.toContain('as-fake')
    expect(result.data.profile).toEqual({
      provider: 'modal',
      profile_id: 'mc-ab12cd',
      name: 'Modal (mc-ab12cd)',
      endpoint_url: result.data.endpoint_url,
    })
    expect(result.data.endpoint_url).toMatch(/^https:\/\/.+\/mc\/v1$/)
    expect(result.data.health).toMatchObject({ ok: true, status: 'reachable' })
    expect(result.data.selected).toBe(true)
    expect(result.data.model.recipe_id).toBe('mc-flux2-klein-inpaint-v1')

    // Every step starts and finishes in order; weights report a percentage.
    const finished = progress.filter((event) => event.state === 'done').map((event) => event.step)
    expect(finished).toEqual([...PROVISION_APPLY_STEPS])
    for (const event of progress) {
      expect(Object.keys(event).sort()).toEqual(['op', 'pct', 'provider', 'state', 'step'])
      expect(event).toMatchObject({ op: 'apply', provider: 'modal' })
    }
    expect(progress.some((event) => event.step === 'weights' && event.pct === 50)).toBe(true)

    // Saved, selected, and holding a runtime secret: ready once allowed.
    const config = await mock.readInferenceConfig()
    expect(config.selectedTarget).toEqual({ type: 'modal', profile_id: 'mc-ab12cd' })
    await mock.writeSettings({ cloudEngines: 'allowed' })
    expect((await readCloudReadiness(mock)).ready).toBe(true)
  })

  it('fails at a step, then resumes from it without repeating what finished', async () => {
    const mock = makeMock()
    const progress = []
    await mock.onProvisionProgress((event) => progress.push(event))
    const { credentials, plan } = await planned(mock, 'beam', 'mc-resume')

    const failed = await mock.runCloudProvisioner({
      op: 'apply',
      provider: 'beam',
      params: {
        credentials,
        installation_id: 'mc-resume',
        approved_plan_hash: plan.plan_hash,
        simulate_fail_step: 'deploy',
      },
    })
    expect(failed.success).toBe(false)
    expect(failed.error.code).toBe('ERR_EXECUTION_FAILED')
    expect(progress.at(-1)).toMatchObject({ step: 'deploy', state: 'fail' })

    progress.length = 0
    const resumed = await mock.runCloudProvisioner({
      op: 'resume',
      provider: 'beam',
      params: { credentials, installation_id: 'mc-resume' },
    })
    expect(resumed.success).toBe(true)
    expect(resumed.data.profile.name).toBe('Beam (mc-resume)')
    const skipped = progress.filter((event) => event.state === 'skip').map((event) => event.step)
    expect(skipped).toEqual(['validate', 'volume', 'state', 'secret', 'image'])
    expect(progress.find((event) => event.state === 'start')).toMatchObject({ op: 'resume', step: 'deploy' })
  })

  it('stops when asked, and a later resume finishes the job', async () => {
    const mock = makeMock()
    const { credentials, plan } = await planned(mock, 'modal', 'mc-stop01')
    let stopped = false
    await mock.onProvisionProgress((event) => {
      if (!stopped && event.step === 'image' && event.state === 'start') {
        stopped = true
        mock.cancelCloudProvisioner()
      }
    })
    const result = await mock.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-stop01', approved_plan_hash: plan.plan_hash },
    })
    expect(result.success).toBe(false)
    expect(result.error.code).toBe('ERR_CANCELLED')
    expect(await mock.cancelCloudProvisioner()).toEqual({ cancelled: false })

    const resumed = await mock.runCloudProvisioner({
      op: 'resume',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-stop01' },
    })
    expect(resumed.success).toBe(true)
  })

  it('cleans up only what the installation created, after a second approval', async () => {
    const mock = await readyMock()
    const cleanupPlan = await mock.runCloudProvisioner({
      op: 'cleanup_plan',
      provider: 'modal',
      params: { installation_id: 'mc-ready1' },
    })
    expect(cleanupPlan.success).toBe(true)
    expect(cleanupPlan.data.resources_to_delete.map((resource) => resource.resource_type)).toEqual([
      'volume',
      'dict',
      'app',
      'proxy_token',
    ])

    const base = {
      credentials: MODAL_KEYS,
      installation_id: 'mc-ready1',
      approved_cleanup_plan_hash: cleanupPlan.data.plan_hash,
    }
    const unconfirmed = await mock.runCloudProvisioner({ op: 'cleanup_apply', provider: 'modal', params: base })
    expect(unconfirmed.error.code).toBe('ERR_VALIDATION_ERROR')

    const done = await mock.runCloudProvisioner({
      op: 'cleanup_apply',
      provider: 'modal',
      params: { ...base, confirm_delete_persistent_storage: true },
    })
    expect(done.success).toBe(true)
    expect(done.data.deleted).toHaveLength(4)
  })
})

describe('mock cloud renders', () => {
  it('walks the IC-3 phases under the derived attempt id and commits a FLUX result', async () => {
    const mock = await readyMock()
    const events = []
    await mock.onCloudAttempt((event) => events.push(event))
    const { page, region } = await firstRegion(mock)
    const intent = { action: 'applyTool', tool: 'contentAwareFill', params: { engine: 'cloud' } }
    const { proposal, params } = await grantFor(mock, region.id, intent)

    // The crop is around the region, not the page.
    expect(proposal.rect.w).toBeLessThan(page.width)
    expect(proposal.rect.x).toBeLessThanOrEqual(region.bbox.x)
    expect(proposal.recipe.model_revision).toBe('45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd')

    const result = await mock.applyTool({
      tool: 'contentAwareFill',
      params: { engine: 'cloud', ...params },
      chapterId: CHAPTER,
      pageIndex: page.index,
      regionId: region.id,
    })
    expect(result.status).toBe('applied')
    expect(result.mask.provenance.engine).toBe('flux')

    expect(events.map((event) => event.phase)).toEqual([
      'preparing',
      'submitting',
      'queued',
      'running',
      'downloading',
      'compositing',
      'committed',
    ])
    for (const event of events) {
      expect(event).toMatchObject({
        attemptId: cloudAttemptId(params.grantNonce),
        regionId: region.id,
        chapterId: CHAPTER,
        pageIndex: page.index,
      })
      expect(typeof event.elapsedMs).toBe('number')
    }
    expect(events.at(-1).errorCode).toBeNull()

    // A grant is spent once. The refusal answers the way `apply_tool` does,
    // with the code, and ends on the event channel too.
    await expect(
      mock.applyTool({ tool: 'contentAwareFill', params: { engine: 'cloud', ...params }, regionId: region.id }),
    ).resolves.toEqual({ status: 'failed', errorCode: 'consent_invalid' })
    expect(events.at(-1)).toMatchObject({ phase: 'failed', errorCode: 'consent_invalid' })
  })

  it('asks for consent when a cloud engine comes without a grant, and refuses while cloud is off', async () => {
    const mock = await readyMock()
    const { page, region } = await firstRegion(mock)
    const where = { chapterId: CHAPTER, pageIndex: page.index, regionId: region.id }
    await expect(
      mock.applyTool({ tool: 'contentAwareFill', params: { engine: 'cloud' }, ...where }),
    ).resolves.toEqual({ status: 'needs-confirmation' })

    await mock.writeSettings({ cloudEngines: 'blocked' })
    await expect(
      mock.applyTool({ tool: 'contentAwareFill', params: { engine: 'cloud' }, ...where }),
    ).resolves.toEqual({ status: 'blocked', errorCode: 'cloud_disabled' })
    const drawn = await mock.createRegion({
      chapterId: CHAPTER,
      pageIndex: page.index,
      bbox: { x: 0.1, y: 0.1, w: 0.05, h: 0.05 },
      tool: 'contentAwareFill',
      params: { engine: 'cloud' },
    })
    expect(drawn).toBeNull()
  })

  it('cancels promptly and ends with the cancelled phase', async () => {
    const mock = await readyMock()
    const { page, region } = await firstRegion(mock)
    const { params } = await grantFor(mock, region.id, { action: 'cleanAnyway' })
    const events = []
    /** @type {Promise<unknown>[]} */
    const answers = []
    await mock.onCloudAttempt((event) => {
      events.push(event)
      if (event.phase === 'queued') answers.push(mock.cancelCloudAttempt({ attemptId: event.attemptId }))
    })

    // A render that does not commit answers null; its code is on the event.
    await expect(
      mock.cleanAnyway({ regionId: region.id, engine: 'cloud', params }),
    ).resolves.toBeNull()
    expect(events.at(-1)).toMatchObject({ phase: 'cancelled', errorCode: 'cancelled' })
    expect(events.map((event) => event.phase)).not.toContain('running')
    // Asked for, not confirmed, as `commands.rs#cancel_cloud_attempt` answers
    // for a render in this process: the event is what says it stopped.
    await expect(answers[0]).resolves.toEqual({ handle: '', status: 'cancel_requested', acknowledged: false })
    expect(page).toBeTruthy()
  })

  it('commits a render whose cancel came while the result downloaded, and refuses one after that', async () => {
    const mock = await readyMock()
    const { page, region } = await firstRegion(mock)
    const intent = { action: 'applyTool', tool: 'contentAwareFill', params: { engine: 'cloud' } }
    const { params } = await grantFor(mock, region.id, intent)
    const events = []
    /** @type {Promise<unknown>[]} */
    const answers = []
    await mock.onCloudAttempt((event) => {
      events.push(event)
      if (event.phase === 'downloading') answers.push(mock.cancelCloudAttempt({ attemptId: event.attemptId }))
      if (event.phase === 'compositing') {
        answers.push(mock.cancelCloudAttempt({ attemptId: event.attemptId }).catch((error) => error.message))
      }
    })

    const result = await mock.applyTool({
      tool: 'contentAwareFill',
      params: { engine: 'cloud', ...params },
      chapterId: CHAPTER,
      pageIndex: page.index,
      regionId: region.id,
    })
    // Too late to stop: the answer is the same as for any live render, and
    // the last event is the commit, so the interface shows the result.
    await expect(answers[0]).resolves.toEqual({ handle: '', status: 'cancel_requested', acknowledged: false })
    expect(result.status).toBe('applied')
    expect(events.at(-1)).toMatchObject({ phase: 'committed', errorCode: null })
    expect(events.map((event) => event.phase)).not.toContain('cancelled')
    // Past cancelling once the result is in hand.
    await expect(answers[1]).resolves.toBe('cannot cancel attempt: job is already completed')
  })

  it('refuses a render the grant does not cover, and says why on the event channel', async () => {
    const mock = await readyMock()
    const { region } = await firstRegion(mock)
    const events = []
    await mock.onCloudAttempt((event) => events.push(event))
    const { params } = await grantFor(mock, region.id, { action: 'cleanAnyway' })
    await expect(
      mock.cleanAnyway({
        regionId: region.id,
        engine: 'cloud',
        params: { ...params, executionTarget: { type: 'beam', profile_id: 'other' } },
      }),
    ).resolves.toBeNull()
    expect(events.at(-1)).toMatchObject({ phase: 'failed', errorCode: 'target_changed' })

    await expect(
      mock.cleanAnyway({ regionId: region.id, engine: 'cloud', params: { grantNonce: 'grant-forged' } }),
    ).resolves.toBeNull()
    expect(events.at(-1)).toMatchObject({
      attemptId: cloudAttemptId('grant-forged'),
      phase: 'failed',
      errorCode: 'consent_invalid',
    })

    await mock.deleteCloudSecret({ provider: 'modal', profileId: 'mc-ready1', role: 'runtime' })
    const second = await grantFor(mock, region.id, { action: 'cleanAnyway' })
    await expect(
      mock.cleanAnyway({ regionId: region.id, engine: 'cloud', params: second.params }),
    ).resolves.toBeNull()
    expect(events.at(-1)).toMatchObject({ phase: 'failed', errorCode: 'credential_missing' })
  })

  it('reruns a mask on the cloud when the params carry a grant', async () => {
    const mock = await readyMock()
    const { page, region } = await firstRegion(mock)
    const local = await mock.applyTool({
      tool: 'contentAwareFill',
      params: { engine: 'lama' },
      chapterId: CHAPTER,
      pageIndex: page.index,
      regionId: region.id,
    })
    const intent = { action: 'rerunMask', mask_id: local.mask.id, kind: 'fill', engine: 'cloud' }
    const { params } = await grantFor(mock, region.id, intent)
    const rerun = await mock.rerunMask({ maskId: local.mask.id, kind: 'fill', engine: 'cloud', params })
    expect(rerun.mask.provenance.engine).toBe('flux')
  })
})

describe('re-running a patch the cloud rendered, with no grant', () => {
  it('refuses to run it again as what it was, and runs a local engine chosen for it', async () => {
    const mock = await readyMock()
    /** @type {any[]} */
    const events = []
    mock.subscribe((event) => events.push(event))
    const { page, region } = await firstRegion(mock)
    const intent = { action: 'applyTool', tool: 'contentAwareFill', params: { engine: 'cloud' } }
    const { params } = await grantFor(mock, region.id, intent)
    const rendered = await mock.applyTool({
      tool: 'contentAwareFill',
      params: { engine: 'cloud', ...params },
      chapterId: CHAPTER,
      pageIndex: page.index,
      regionId: region.id,
    })
    const maskId = rendered.mask.id
    expect(rendered.mask.provenance).toMatchObject({ engine: 'flux', cloud: { provider: 'modal' } })
    const refusals = () => events.filter((event) => event.key === 'notice.mask.rerunFailed')

    // Try again, a step that lands where it is, and the cloud named: each
    // would render in the cloud, so none runs here (`region.rs#rerun_needs_cloud`).
    for (const spec of [{ kind: 'retry' }, { kind: 'stronger' }, { kind: 'engine', engine: 'cloud' }]) {
      await expect(mock.rerunMask({ maskId, ...spec })).resolves.toBeNull()
    }
    await vi.waitFor(() => expect(refusals()).toHaveLength(3))
    expect(refusals().map((event) => event.params.reasonKey)).toEqual(Array(3).fill('decline.reason.rungUnavailable'))

    // With the cloud off, the same request is refused as blocked.
    await mock.writeSettings({ cloudEngines: 'blocked' })
    await expect(mock.rerunMask({ maskId, kind: 'retry' })).resolves.toBeNull()
    await vi.waitFor(() => expect(events.map((event) => event.key)).toContain('notice.cloud.blocked'))

    // A local engine stepped to runs on this machine, cloud off or not.
    const simpler = await mock.rerunMask({ maskId, kind: 'simpler' })
    expect(simpler?.mask.provenance).toMatchObject({ engine: 'lama', cloud: null })
  })

  it('runs a local engine named for a cloud patch on this machine', async () => {
    const mock = await readyMock()
    const { page, region } = await firstRegion(mock)
    const intent = { action: 'applyTool', tool: 'contentAwareFill', params: { engine: 'cloud' } }
    const { params } = await grantFor(mock, region.id, intent)
    const rendered = await mock.applyTool({
      tool: 'contentAwareFill',
      params: { engine: 'cloud', ...params },
      chapterId: CHAPTER,
      pageIndex: page.index,
      regionId: region.id,
    })
    // FLUX is the rung a cloud patch records, and naming it is naming the
    // local helper, not a second cloud render.
    expect(rerunNeedsCloud(rendered.mask, 'engine', 'flux')).toBe(false)
    expect(rerunNeedsCloud(rendered.mask, 'retry')).toBe(true)
    const named = await mock.rerunMask({ maskId: rendered.mask.id, kind: 'engine', engine: 'lama' })
    expect(named?.mask.provenance).toMatchObject({ engine: 'lama', cloud: null })
  })
})

describe('mock cloud recovery', () => {
  it('answers apply with the IC-4 shape and never resubmits an unknown attempt', async () => {
    const mock = await readyMock()
    const { region } = await firstRegion(mock)
    const { params } = await grantFor(mock, region.id, { action: 'cleanAnyway' })
    await mock.submitCloudAttempt({
      attemptId: 'att-left-over',
      grantNonce: params.grantNonce,
      simulateMode: 'ambiguous_acceptance',
    })

    const report = await mock.reconcileCloudRecovery({ apply: true })
    expect(Object.keys(report).sort()).toEqual(['attached', 'needsAttention', 'stillRunning'])
    expect(report.needsAttention).toEqual([
      { attemptId: 'att-left-over', chapterId: null, pageIndex: null, regionId: null, reason: 'ambiguous' },
    ])
    // Uncertainty stays actionable until explicitly acknowledged or abandoned.
    expect((await mock.reconcileCloudRecovery({ apply: true })).needsAttention).toEqual(report.needsAttention)
    await expect(mock.reconcileCloudRecovery({ action: 'abandon', attemptId: 'att-left-over' })).rejects.toThrow('confirmation')
    await mock.reconcileCloudRecovery({ action: 'abandon', attemptId: 'att-left-over', acceptDuplicateRisk: true })
    expect((await mock.getCloudAttemptStatus({ attemptId: 'att-left-over' })).status).toBe('abandoned')
    expect((await mock.reconcileCloudRecovery({ apply: true })).needsAttention).toEqual([])
    // Without `apply` the command keeps its old answer.
    expect((await mock.reconcileCloudRecovery({ attemptId: 'att-left-over' })).decision).toBeDefined()
  })
})
