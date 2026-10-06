/**
 * The cloud setup, driven the way a person drives it: the key, Continue, the
 * review, one approval, Start, the live checklist, and each way it can end.
 *
 * Two stand-ins for the native side. The mock backend (`api/mock.js`, every
 * delay zero) runs the whole pipeline, IC-2 progress included, for the paths
 * that go end to end. A scripted helper stands in where a test has to hold the
 * setup at one step: its answers are promises the test settles, and its
 * progress events are sent by hand.
 */
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { tick } from 'svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createMockBackend, PINNED_CLOUD_MODEL_ID, PROVISION_APPLY_STEPS } from '../api/mock.js'
import { t } from '../i18n/index.js'
import { clearNotices } from '../state/app.svelte.js'
import CloudProvisioner, { isValidInstallationId, isValidPlanHash } from './CloudProvisioner.svelte'
import { forgetUnfinished, setup } from './provisioning.svelte.js'

const ZERO = { method: 0, openChapter: 0, export: 0, cloud: 0, provision: 0, region: 0, pageTail: 0, noticeStagger: 0 }
const HASH = 'ab'.repeat(32)
const ID = 'mc-test01'
const MODAL = { token_id: 'ak-test-id', token_secret: 'as-test-secret' }
const MODAL_KEY = 'settings.inference.provider.modal'
const BEAM_KEY = 'settings.inference.provider.beam'

/**
 * Local storage for the test. The node running the suite has none of its own,
 * and the unfinished-setup marker lives there.
 */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() {
      return values.size
    },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
})

afterEach(() => {
  cleanup()
  setup.run = null
  forgetUnfinished()
  clearNotices()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/* ---------- stand-ins ---------- */

function deferred() {
  /** @type {(value: any) => void} */
  let resolve = () => {}
  const promise = new Promise((done) => {
    resolve = done
  })
  return { promise, resolve }
}

/** @param {any} data */
const ok = (data) => ({ success: true, data, error: null })
/** @param {string} code */
const refused = (code) => ({ success: false, data: null, error: { code, message: 'refused' } })

/**
 * A plan in the shape the helper answers with.
 *
 * @param {{provider?: 'modal'|'beam', gpu?: string, hash?: string}} [options]
 */
function planData({ provider = 'modal', gpu, hash = HASH } = {}) {
  const gpus = provider === 'modal' ? ['L4', 'A10', 'L40S'] : ['RTX4090', 'A10G', 'RTX5090']
  return {
    plan_hash: hash,
    installation_id: ID,
    provider,
    resource_allocation: {
      gpu: gpu ?? gpus[0],
      gpu_options: gpus,
      idle_seconds: 120,
      model_weights_bytes: 5_475_930_180,
    },
    resources_to_create: [
      { type: 'volume', name: `mc-weights-${ID}` },
      { type: 'app', name: `mc-${ID}` },
      { type: 'proxy_token', name: `mc-token-${ID}` },
    ],
  }
}

const ENDPOINT = `https://ws--mc-${ID}-gateway.modal.run/mc/v1`

/** What the native side answers once it has saved and selected the endpoint (IC-1). */
function installed() {
  return ok({
    profile: { provider: 'modal', profile_id: ID, name: `Modal (${ID})`, endpoint_url: ENDPOINT },
    health: { ok: true, status: 'reachable', latency_ms: 42 },
    selected: true,
  })
}

/**
 * A helper that answers each op from `answers`: a value, or a function of the
 * spec, which may return a promise the test holds.
 *
 * @param {Record<string, any>} answers
 */
function scripted(answers) {
  return vi.fn(async (/** @type {{op: string}} */ spec) => {
    const answer = answers[spec.op]
    if (answer === undefined) throw new Error(`no answer for ${spec.op}`)
    return typeof answer === 'function' ? answer(spec) : answer
  })
}

/** The mock backend with the progress channel and Stop in the test's hands. */
function handBackend() {
  const base = createMockBackend({ timing: ZERO })
  /** @type {((event: unknown) => void)|null} */
  let listener = null
  const backend = Object.assign(Object.create(base), {
    onProvisionProgress: vi.fn((/** @type {(event: unknown) => void} */ handler) => {
      listener = handler
      return () => {
        if (listener === handler) listener = null
      }
    }),
    cancelCloudProvisioner: vi.fn(async () => ({ cancelled: true })),
  })
  return {
    backend,
    /** @param {Record<string, unknown>} event */
    async emit(event) {
      listener?.({ op: 'apply', provider: 'modal', ...event })
      await tick()
    },
  }
}

/* ---------- driving it ---------- */

/** @param {string} name */
const button = (name) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name }))

/** @param {string} key */
const heading = (key) => screen.findByRole('heading', { name: t(key) })

/**
 * @param {string} labelKey
 * @param {string} value
 */
async function typeKey(labelKey, value) {
  await fireEvent.input(screen.getByLabelText(t(labelKey)), { target: { value } })
}

async function typeModalKeys() {
  await typeKey('settings.cloud.setup.connect.modalTokenId', MODAL.token_id)
  await typeKey('settings.cloud.setup.connect.modalTokenSecret', MODAL.token_secret)
}

/**
 * What WebKit does when the focused control turns disabled: focus falls to the
 * body. jsdom keeps it on the disabled control, and will not blur one either,
 * so focus is moved away through a stand-in that is then removed.
 */
function dropFocus() {
  const stand = document.createElement('input')
  document.body.append(stand)
  stand.focus()
  stand.remove()
}

async function connectModal() {
  await typeModalKeys()
  await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
  await heading('settings.cloud.setup.heading.review')
}

/** @param {string} [providerKey] */
function approval(providerKey = MODAL_KEY) {
  return /** @type {HTMLInputElement} */ (
    screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.approve', { providerKey }) })
  )
}

async function approveAndStart() {
  await fireEvent.click(approval())
  await fireEvent.click(button(t('settings.cloud.setup.review.start')))
}

/**
 * One line of the live checklist, by its step. The checklist is named by the
 * heading above it, which says how the run is going.
 *
 * @param {string} step
 * @param {string} [headingKey]
 */
function stepRow(step, headingKey = 'settings.cloud.setup.heading.running') {
  const list = screen.getByRole('list', { name: t(headingKey) })
  return /** @type {HTMLElement} */ (within(list).getByText(t(`settings.cloud.setup.step.${step}`)).closest('li'))
}

/** Every value in local storage, where a secret must never be. */
function stored() {
  const values = []
  for (let index = 0; index < localStorage.length; index += 1) {
    const key = localStorage.key(index)
    if (key !== null) values.push(`${key}=${localStorage.getItem(key)}`)
  }
  return values.join('\n')
}

/* ---------- tests ---------- */

describe('CloudProvisioner helper validations', () => {
  it('validates installation IDs correctly', () => {
    expect(isValidInstallationId('mc-inst-prod-1')).toBe(true)
    expect(isValidInstallationId('worker_01')).toBe(true)
    expect(isValidInstallationId('a')).toBe(true)
    expect(isValidInstallationId('A'.repeat(64))).toBe(true)

    expect(isValidInstallationId('')).toBe(false)
    expect(isValidInstallationId(' mc-inst ')).toBe(false)
    expect(isValidInstallationId('has.dot')).toBe(false)
    expect(isValidInstallationId('has/slash')).toBe(false)
    expect(isValidInstallationId('has space')).toBe(false)
    expect(isValidInstallationId('A'.repeat(65))).toBe(false)
  })

  it('validates 64-character hex plan hashes correctly', () => {
    expect(isValidPlanHash('a'.repeat(64))).toBe(true)
    expect(isValidPlanHash('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855')).toBe(true)

    expect(isValidPlanHash('')).toBe(false)
    expect(isValidPlanHash('a'.repeat(63))).toBe(false)
    expect(isValidPlanHash('a'.repeat(65))).toBe(false)
    expect(isValidPlanHash('g'.repeat(64))).toBe(false)
    expect(isValidPlanHash('has non-hex!'.padEnd(64, '0'))).toBe(false)
  })
})

describe('setting up end to end on the mock backend', () => {
  it('identifies a missing bundled helper without blaming the network', async () => {
    render(CloudProvisioner, {
      props: {
        inline: true,
        installationId: ID,
        backend: createMockBackend({ timing: ZERO }),
        runCloudProvisioner: scripted({ inspect: refused('ERR_HELPER_MISSING') }),
      },
    })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    expect(await screen.findByText(t('settings.cloud.setup.error.helperMissing'))).toBeTruthy()
    expect(screen.queryByText(t('settings.cloud.setup.error.unavailable'))).toBeNull()
  })

  it('imports Modal’s copyable command and uses its token for inspect and plan', async () => {
    const runner = scripted({
      inspect: (spec) => {
        expect(spec.params.credentials).toEqual(MODAL)
        return ok({ eligible: true, workspace_name: 'k-omiq' })
      },
      plan: (spec) => {
        expect(spec.params.credentials).toEqual(MODAL)
        return ok(planData())
      },
    })
    render(CloudProvisioner, {
      props: { inline: true, installationId: ID, backend: createMockBackend({ timing: ZERO }), runCloudProvisioner: runner },
    })

    await typeKey(
      'settings.cloud.setup.connect.modalCommand',
      'modal token set --token-id ak-test-id --token-secret as-test-secret --profile=k-omiq',
    )
    expect(screen.getByText(t('settings.cloud.setup.connect.modalCommandImported', { profile: 'k-omiq' }))).toBeTruthy()
    expect(/** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.cloud.setup.connect.modalCommand'))).value).toBe('')
    expect(/** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.cloud.setup.connect.modalTokenId'))).value).toBe(MODAL.token_id)
    expect(/** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.cloud.setup.connect.modalTokenSecret'))).value).toBe(MODAL.token_secret)
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.review')
    expect(screen.getByText('k-omiq')).toBeTruthy()
    expect(runner).toHaveBeenCalledTimes(2)
    expect(stored()).not.toContain(MODAL.token_secret)
  })

  it('reviews the plan, runs the checklist, and hands over the saved endpoint', async () => {
    const backend = createMockBackend({ timing: ZERO })
    /** @type {any[]} */
    const progress = []
    backend.onProvisionProgress((/** @type {any} */ event) => progress.push(event))
    const onconfigured = vi.fn()
    const onbusychange = vi.fn()
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, onconfigured, onbusychange } })

    expect(screen.getByRole('heading', { name: t('settings.cloud.setup.heading.connect') })).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.connect.keyNoteModal'))).toBeTruthy()
    expect(button(t('settings.cloud.setup.connect.continue')).disabled).toBe(true)
    await connectModal()

    // The GPU and idle choices are the plan's own, not a list of ours.
    const gpu = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.gpu')))
    expect(Array.from(gpu.options, (option) => option.value)).toEqual(['L4', 'A10', 'L40S'])
    expect(gpu.value).toBe('L4')
    expect(screen.getByText(PINNED_CLOUD_MODEL_ID)).toBeTruthy()
    const idle = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.idle')))
    expect(idle.value).toBe('120')
    for (const name of [`mc-weights-${ID}`, `mc-jobs-${ID}`, `mc-${ID}`, `mc-token-${ID}`]) {
      expect(screen.getByText(name)).toBeTruthy()
    }
    expect(screen.getByText(t('settings.cloud.setup.review.weights', { size: '5.5' }))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.review.costGpu', { providerKey: MODAL_KEY }))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.review.tokenModal'))).toBeTruthy()
    expect(button(t('settings.cloud.setup.review.start')).disabled).toBe(true)

    await approveAndStart()
    // Even with zero mock delays, apply yields across every progress step and
    // endpoint save. A saturated full suite can take longer than the query's
    // default one second while the checklist is legitimately still running.
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.done') }, { timeout: 10_000 })

    // Every IC-2 step reached the checklist, in the helper's order.
    expect(progress.filter((event) => event.state === 'done').map((event) => event.step)).toEqual(PROVISION_APPLY_STEPS)
    expect(setup.run?.steps.map((step) => [step.id, step.state])).toEqual(
      PROVISION_APPLY_STEPS.map((step) => [step, 'done']),
    )

    const name = `Modal (${ID})`
    expect(screen.getByText(t('settings.cloud.setup.done.saved', { name }))).toBeTruthy()
    expect(screen.getByText(t('settings.inference.health.reachable'))).toBeTruthy()
    expect(screen.getByText(t('settings.inference.health.latency', { latency: 42 }))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.done.tryIt'))).toBeTruthy()
    expect(onconfigured).toHaveBeenCalledTimes(1)
    expect(onconfigured).toHaveBeenCalledWith({
      provider: 'modal',
      profileId: ID,
      endpointUrl: `https://mock-workspace--mc-${ID}-gateway.modal.run/mc/v1`,
      name,
      healthy: true,
    })
    await waitFor(() => expect(onbusychange.mock.calls.map(([busy]) => busy)).toEqual([true, false, true, false]))

    // The native side saved and selected it, with its token in the keychain.
    const config = await backend.readInferenceConfig()
    expect(config.selectedTarget).toEqual({ type: 'modal', profile_id: ID })
    const token = await backend.getCloudSecretSummary({ provider: 'modal', profileId: ID, role: 'runtime' })
    expect(token.present).toBe(true)

    // The setup key went in the calls that needed it and nowhere else.
    expect(setup.unfinished).toBeNull()
    for (const where of [JSON.stringify(setup), stored(), document.body.innerHTML]) {
      expect(where).not.toContain(MODAL.token_secret)
      expect(where).not.toContain(MODAL.token_id)
    }
  }, 15_000)

  it('replans with the selected 9B checkpoint and its required GPU', async () => {
    const model4b = 'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic'
    const model9b = 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32'
    const choices = [
      { model_id: model4b, label: 'FLUX.2 Klein 4B (4-bit)', required_gpu: null },
      { model_id: model9b, label: 'FLUX.2 Klein 9B (4-bit)', required_gpu: 'L40S' },
    ]
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: (spec) => {
        const data = planData({ gpu: spec.params.options?.gpu ?? 'L4' })
        data.resource_allocation.model_id = spec.params.options?.model_id ?? model4b
        data.resource_allocation.model_options = choices
        data.resource_allocation.model_license = data.resource_allocation.model_id === model9b
          ? 'FLUX non-commercial' : 'Apache-2.0'
        return ok(data)
      },
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    const model = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.model')))
    expect(model.value).toBe(model4b)
    await fireEvent.change(model, { target: { value: model9b } })
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith({ op: 'plan', provider: 'modal',
      params: { credentials: MODAL, installation_id: ID, options: { model_id: model9b, gpu: 'L40S' } } }))
    expect(screen.getByText(t('settings.cloud.setup.review.modelLicense', { license: 'FLUX non-commercial' }))).toBeTruthy()
  })

  it('offers the plan’s routing regions and replans with the one chosen', async () => {
    const regions = ['us-east', 'us-west', 'eu-west', 'ap-south']
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: (spec) => {
        const data = planData()
        Object.assign(data.resource_allocation, {
          routing_region: spec.params.options?.routing_region ?? 'us-east', routing_region_options: regions })
        return ok(data)
      },
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    const region = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.region')))
    expect(Array.from(region.options, (option) => option.value)).toEqual(regions)
    expect(region.value).toBe('us-east')
    expect(screen.getByText(t('settings.cloud.setup.review.regionNote'))).toBeTruthy()
    await fireEvent.change(region, { target: { value: 'ap-south' } })
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith({ op: 'plan', provider: 'modal',
      params: { credentials: MODAL, installation_id: ID, options: { routing_region: 'ap-south' } } }))
    await waitFor(() => expect(/** @type {HTMLSelectElement} */ (
      screen.getByLabelText(t('settings.cloud.setup.review.region'))).value).toBe('ap-south'))
  })

  it('offers no routing region when the plan lists none', async () => {
    const runner = scripted({ inspect: ok({ eligible: true }), plan: ok(planData()) })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    expect(screen.queryByLabelText(t('settings.cloud.setup.review.region'))).toBeNull()
    expect(screen.queryByText(t('settings.cloud.setup.review.regionNote'))).toBeNull()
  })

  it('gives the GPU back when the model that required one is unselected', async () => {
    const model4b = 'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic'
    const model9b = 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32'
    const choices = [
      { model_id: model4b, label: 'FLUX.2 Klein 4B (4-bit)', required_gpu: null },
      { model_id: model9b, label: 'FLUX.2 Klein 9B (4-bit)', required_gpu: 'L40S' },
    ]
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: (spec) => {
        const data = planData({ gpu: spec.params.options?.gpu ?? 'L4' })
        data.resource_allocation.model_id = spec.params.options?.model_id ?? model4b
        data.resource_allocation.model_options = choices
        return ok(data)
      },
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    const model = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.model')))
    const gpu = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.gpu')))
    const planned = (/** @type {object} */ options) => expect(runner).toHaveBeenLastCalledWith({ op: 'plan', provider: 'modal',
      params: { credentials: MODAL, installation_id: ID, options } })

    // No GPU picked: back to the provider's default, not the 9B's L40S.
    await fireEvent.change(model, { target: { value: model9b } })
    await waitFor(() => expect(gpu.value).toBe('L40S'))
    await fireEvent.change(model, { target: { value: model4b } })
    await waitFor(() => planned({ model_id: model4b }))
    await waitFor(() => expect(gpu.value).toBe('L4'))

    // A GPU picked: the person's own pick comes back.
    await fireEvent.change(gpu, { target: { value: 'A10' } })
    await waitFor(() => expect(gpu.value).toBe('A10'))
    await fireEvent.change(model, { target: { value: model9b } })
    await waitFor(() => planned({ model_id: model9b, gpu: 'L40S' }))
    await waitFor(() => expect(gpu.value).toBe('L40S'))
    await fireEvent.change(model, { target: { value: model4b } })
    await waitFor(() => planned({ model_id: model4b, gpu: 'A10' }))
    await waitFor(() => expect(gpu.value).toBe('A10'))
  })

  it('a replan that fails puts the choice back to what the plan still says', async () => {
    let plans = 0
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: () => (plans += 1) === 1 ? ok(planData()) : refused('ERR_VALIDATION_ERROR'),
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    const gpu = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.gpu')))
    await fireEvent.change(gpu, { target: { value: 'A10' } })
    await waitFor(() => expect(gpu.disabled).toBe(false))
    expect(gpu.value).toBe('L4')
  })

  it('asks which cloud analysis graphs to install and replans each selection', async () => {
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: (spec) => {
        const data = planData()
        data.resource_allocation.analysis_models = spec.params.options?.analysis_models ?? []
        data.resource_allocation.analysis_options = [
          { capability: 'text_regions_rt@1', label: 'Ogkalu comic text & bubble detector (Full)' },
          { capability: 'text_mask_sam_ts@1', label: 'SAM-TS-L lettering mask' },
        ]
        return ok(data)
      },
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    expect(screen.getByRole('group', { name: t('settings.cloud.setup.review.analysisModels') })).toBeTruthy()
    await fireEvent.click(screen.getByRole('checkbox', { name: 'SAM-TS-L lettering mask' }))
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith({ op: 'plan', provider: 'modal',
      params: { credentials: MODAL, installation_id: ID,
        options: { analysis_models: ['text_mask_sam_ts@1'] } } }))
    expect(screen.getByRole('checkbox', { name: 'SAM-TS-L lettering mask' }).checked).toBe(true)
    expect(screen.getByText(t('settings.cloud.setup.review.analysisNote'))).toBeTruthy()
  })

  it('offers Page denoise on Modal, off by default, and replans when it is turned on', async () => {
    const backend = createMockBackend({ timing: ZERO })
    /** @type {Promise<unknown>[]} */
    const answers = []
    const runner = vi.fn((/** @type {any} */ spec) => {
      const answer = backend.runCloudProvisioner(spec)
      answers.push(answer)
      return answer
    })
    const settled = async () => {
      await Promise.all(answers)
      await tick()
      await waitFor(() => expect(group.disabled).toBe(false))
    }
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, runCloudProvisioner: runner } })
    await connectModal()
    const group = /** @type {HTMLFieldSetElement} */ (screen.getByRole('group', { name: t('settings.cloud.setup.review.denoise') }))
    const toggle = /** @type {HTMLInputElement} */ (screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.denoiseInstall') }))
    expect(toggle.checked).toBe(false)
    expect(screen.getByText(t('settings.cloud.setup.review.denoiseNote', { size: '0.5' }))).toBeTruthy()

    await fireEvent.click(toggle)
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith(expect.objectContaining({
      op: 'plan', provider: 'modal', params: expect.objectContaining({ options: { denoise: true } }) })))
    await settled()
    expect(toggle.checked).toBe(true)

    // Another choice keeps it on.
    const gpu = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.gpu')))
    await fireEvent.change(gpu, { target: { value: 'A10' } })
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith(expect.objectContaining({
      op: 'plan', params: expect.objectContaining({ options: { denoise: true, gpu: 'A10' } }) })))
    await settled()

    await fireEvent.click(toggle)
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith(expect.objectContaining({
      op: 'plan', params: expect.objectContaining({ options: { denoise: false, gpu: 'A10' } }) })))
    await settled()
    expect(toggle.checked).toBe(false)
  })

  it('shows the plan’s own denoise choice again when a replan is refused', async () => {
    let plans = 0
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: () => {
        if ((plans += 1) > 1) return refused('ERR_VALIDATION_ERROR')
        const data = planData()
        Object.assign(data.resource_allocation, { denoise: false, denoise_supported: true, denoise_models_bytes: 506_170_387 })
        return ok(data)
      },
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID,
      runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    const group = /** @type {HTMLFieldSetElement} */ (screen.getByRole('group', { name: t('settings.cloud.setup.review.denoise') }))
    const toggle = /** @type {HTMLInputElement} */ (screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.denoiseInstall') }))
    await fireEvent.click(toggle)
    await waitFor(() => expect(runner).toHaveBeenCalledTimes(3))
    await waitFor(() => expect(group.disabled).toBe(false))
    expect(toggle.checked).toBe(false)
  })

  it('shows Beam as paused, and it cannot be chosen', async () => {
    const backend = createMockBackend({ timing: ZERO })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, initialProvider: 'beam' } })

    const beam = /** @type {HTMLInputElement} */ (screen.getAllByRole('radio').find((radio) => /** @type {HTMLInputElement} */ (radio).value === 'beam'))
    expect(beam.disabled).toBe(true)
    expect(beam.checked).toBe(false)
    expect(screen.getByText(t('settings.cloud.setup.connect.pausedNote'))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.connect.pausedTag'))).toBeTruthy()
    await fireEvent.click(beam)
    // Still Modal: its key fields stay, Beam's never appear.
    expect(screen.getByLabelText(t('settings.cloud.setup.connect.modalTokenId'))).toBeTruthy()
    expect(screen.queryByLabelText(t('settings.cloud.setup.connect.beamToken'))).toBeNull()
  })

  it('offers where to find the key', async () => {
    render(CloudProvisioner, { props: { inline: true, backend: createMockBackend({ timing: ZERO }) } })
    const help = button(t('settings.cloud.setup.connect.help'))
    expect(help.getAttribute('aria-expanded')).toBe('false')
    await fireEvent.click(help)
    expect(help.getAttribute('aria-expanded')).toBe('true')
    expect(screen.getByText(t('settings.cloud.setup.connect.helpModal'))).toBeTruthy()
    expect(screen.getByText('https://modal.com/settings/tokens')).toBeTruthy()
  })
})

describe('the live checklist', () => {
  it('shows each step with its time, and Stop asks the helper to stop', async () => {
    const applied = deferred()
    const runner = scripted({ inspect: ok({ eligible: true }), plan: ok(planData()), apply: () => applied.promise })
    const { backend, emit } = handBackend()
    const onbusychange = vi.fn()
    render(CloudProvisioner, {
      props: { inline: true, installationId: ID, runCloudProvisioner: runner, backend, onbusychange },
    })
    await connectModal()
    await approveAndStart()

    const running = await heading('settings.cloud.setup.heading.running')
    expect(document.activeElement).toBe(running)
    expect(runner).toHaveBeenLastCalledWith({
      op: 'apply',
      provider: 'modal',
      params: {
        credentials: MODAL,
        installation_id: ID,
        approved_plan_hash: HASH,
        options: {},
        remember_setup_credential: true,
      },
    })

    await emit({ step: 'validate', state: 'start' })
    await emit({ step: 'validate', state: 'done' })
    await emit({ step: 'volume', state: 'skip' })
    await emit({ step: 'weights', state: 'start', pct: 0 })
    await emit({ step: 'weights', state: 'start', pct: 50 })
    // Another operation's event is not this checklist's.
    await emit({ op: 'cleanup_apply', step: 'image', state: 'start' })

    const validate = stepRow('validate')
    expect(validate.classList.contains('done')).toBe(true)
    expect(within(validate).getByText(t('settings.cloud.setup.state.done'))).toBeTruthy()
    expect(validate.querySelector('.step-time')?.textContent?.trim()).toMatch(/^\d+:\d\d$/)
    expect(stepRow('volume').classList.contains('skip')).toBe(true)
    const weights = stepRow('weights')
    expect(weights.classList.contains('running')).toBe(true)
    expect(weights.querySelector('.step-time')?.textContent?.trim()).toBe('50%')
    expect(screen.queryByText(t('settings.cloud.setup.step.image'))).toBeNull()

    await fireEvent.click(button(t('settings.cloud.setup.running.stop')))
    expect(backend.cancelCloudProvisioner).toHaveBeenCalledTimes(1)
    await waitFor(() => expect(button(t('settings.cloud.setup.running.stopping')).disabled).toBe(true))

    applied.resolve(refused('ERR_CANCELLED'))
    await heading('settings.cloud.setup.heading.stopped')
    const alert = screen.getByRole('alert')
    expect(alert.textContent).toContain(t('settings.cloud.setup.error.cancelled'))
    expect(alert.textContent).toContain(t('settings.cloud.setup.failed.code', { code: 'ERR_CANCELLED' }))
    // The step it stopped in reads as stopped, not as still running.
    expect(stepRow('weights', 'settings.cloud.setup.heading.stopped').classList.contains('fail')).toBe(true)
    expect(button(t('settings.cloud.setup.failed.resume')).disabled).toBe(false)
    expect(button(t('settings.cloud.setup.failed.cleanup'))).toBeTruthy()
    expect(onbusychange.mock.calls.map(([busy]) => busy)).toEqual([true, false, true, false])
  })

  it('cannot be dismissed while it runs, and Escape closes it once it has ended', async () => {
    const applied = deferred()
    const runner = scripted({ inspect: ok({ eligible: true }), plan: ok(planData()), apply: () => applied.promise })
    const { backend } = handBackend()
    const onclose = vi.fn()
    render(CloudProvisioner, { props: { installationId: ID, runCloudProvisioner: runner, backend, onclose } })
    expect(screen.getByRole('dialog')).toBeTruthy()

    await connectModal()
    await approveAndStart()
    await heading('settings.cloud.setup.heading.running')
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(onclose).not.toHaveBeenCalled()
    expect(screen.queryByRole('button', { name: t('shell.action.close') })).toBeNull()

    applied.resolve(refused('ERR_EXECUTION_FAILED'))
    await heading('settings.cloud.setup.heading.failed')
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(onclose).toHaveBeenCalledTimes(1)
  })
})

describe('when setup does not finish', () => {
  it('says why, keeps what finished, and Resume finishes it with the same key', async () => {
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: ok(planData()),
      apply: refused('ERR_SECRET_STORE'),
      resume: installed(),
    })
    const { backend } = handBackend()
    const onconfigured = vi.fn()
    render(CloudProvisioner, { props: { inline: true, installationId: ID, runCloudProvisioner: runner, backend, onconfigured } })
    await connectModal()
    await approveAndStart()

    await heading('settings.cloud.setup.heading.failed')
    const alert = screen.getByRole('alert')
    expect(alert.textContent).toContain(t('settings.cloud.setup.error.secretStore'))
    expect(alert.textContent).toContain(t('settings.cloud.setup.failed.code', { code: 'ERR_SECRET_STORE' }))
    expect(screen.getByText(t('settings.cloud.setup.failed.kept'))).toBeTruthy()
    expect(setup.unfinished).toEqual({ provider: 'modal', installationId: ID, options: {} })
    expect(onconfigured).not.toHaveBeenCalled()

    await fireEvent.click(button(t('settings.cloud.setup.failed.resume')))
    await heading('settings.cloud.setup.heading.done')
    expect(runner).toHaveBeenLastCalledWith({
      op: 'resume',
      provider: 'modal',
      params: { credentials: MODAL, installation_id: ID, options: {}, remember_setup_credential: true },
    })
    expect(screen.getByText(t('settings.cloud.setup.done.saved', { name: `Modal (${ID})` }))).toBeTruthy()
    expect(onconfigured).toHaveBeenCalledTimes(1)
    expect(onconfigured).toHaveBeenCalledWith({
      provider: 'modal',
      profileId: ID,
      endpointUrl: ENDPOINT,
      name: `Modal (${ID})`,
      healthy: true,
    })
    expect(setup.unfinished).toBeNull()
  })

  it('offers a setup the last run left unfinished, with the choices it was planned with', async () => {
    setup.unfinished = { provider: 'modal', installationId: 'mc-half01', options: { gpu: 'A10' } }
    const runner = scripted({ resume: ok({ resumed: false }) })
    const onconfigured = vi.fn()
    render(CloudProvisioner, {
      props: { inline: true, runCloudProvisioner: runner, backend: handBackend().backend, onconfigured },
    })

    await heading('settings.cloud.setup.heading.resume')
    expect(
      screen.getByText(t('settings.cloud.setup.resume.lead', { providerKey: MODAL_KEY, id: 'mc-half01' })),
    ).toBeTruthy()
    const resume = button(t('settings.cloud.setup.failed.resume'))
    expect(resume.disabled).toBe(true)
    const secret = screen.getByLabelText(t('settings.cloud.setup.connect.modalTokenSecret'))
    await typeModalKeys()
    // The fields stay while they are typed in, so a typo can still be fixed.
    expect(screen.getByLabelText(t('settings.cloud.setup.connect.modalTokenSecret'))).toBe(secret)
    expect(resume.disabled).toBe(false)
    await fireEvent.click(resume)

    // A resume of a setup that had already finished changes nothing.
    await heading('settings.cloud.setup.heading.finished')
    expect(runner).toHaveBeenLastCalledWith({
      op: 'resume',
      provider: 'modal',
      params: { credentials: MODAL, installation_id: 'mc-half01', options: { gpu: 'A10' }, remember_setup_credential: true },
    })
    expect(screen.getByText(t('settings.cloud.setup.done.nothing'))).toBeTruthy()
    expect(onconfigured).not.toHaveBeenCalled()
  })

  it('never calls an endpoint ready when its first check failed, and still hands it over as saved', async () => {
    const failing = installed()
    failing.data.health = { ok: false, status: 'unreachable', latency_ms: null }
    const runner = scripted({ inspect: ok({ eligible: true }), plan: ok(planData()), apply: failing })
    const onconfigured = vi.fn()
    const onbusychange = vi.fn()
    render(CloudProvisioner, {
      props: { inline: true, installationId: ID, runCloudProvisioner: runner, backend: handBackend().backend, onconfigured, onbusychange },
    })
    await connectModal()
    await approveAndStart()

    await heading('settings.cloud.setup.heading.finished')
    const name = `Modal (${ID})`
    expect(screen.getByText(t('settings.inference.health.unreachable'))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.done.unchecked', { name }))).toBeTruthy()
    expect(screen.queryByRole('heading', { name: t('settings.cloud.setup.heading.done') })).toBeNull()
    expect(screen.queryByText(t('settings.cloud.setup.done.saved', { name }))).toBeNull()
    expect(screen.queryByText(t('settings.cloud.setup.done.tryIt'))).toBeNull()
    // The host hears of it, so it does not offer a second setup, and is told
    // it is not ready, so it does not turn the cloud on.
    expect(onconfigured).toHaveBeenCalledTimes(1)
    expect(onconfigured).toHaveBeenCalledWith({ provider: 'modal', profileId: ID, endpointUrl: ENDPOINT, name, healthy: false })
    expect(onbusychange).toHaveBeenLastCalledWith(false)
  })

  it('reads an answer that carries no check as a check that could not be made', async () => {
    const unchecked = installed()
    delete unchecked.data.health
    const runner = scripted({ inspect: ok({ eligible: true }), plan: ok(planData()), apply: unchecked })
    const onconfigured = vi.fn()
    render(CloudProvisioner, {
      props: { inline: true, installationId: ID, runCloudProvisioner: runner, backend: handBackend().backend, onconfigured },
    })
    await connectModal()
    await approveAndStart()

    await heading('settings.cloud.setup.heading.finished')
    expect(screen.getByText(t('settings.inference.health.unknown'))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.done.unchecked', { name: `Modal (${ID})` }))).toBeTruthy()
    expect(onconfigured).toHaveBeenCalledTimes(1)
    expect(onconfigured.mock.calls[0][0]).toMatchObject({ profileId: ID, healthy: false })
  })
})

describe('a Modal token saved for updates', () => {
  /** @param {boolean} present */
  function savedBackend(present) {
    const { backend } = handBackend()
    backend.getCloudSecretSummary = vi.fn(async (/** @type {any} */ spec) => ({ ...spec, present, backend: 'keyring' }))
    return backend
  }

  it('updates with the saved token and asks for none', async () => {
    const runner = scripted({ resume: installed() })
    const backend = savedBackend(true)
    render(CloudProvisioner, {
      props: { inline: true, runCloudProvisioner: runner, backend, existing: { provider: 'modal', installationId: ID, action: 'resume' } },
    })

    await screen.findByText(t('settings.cloud.setup.connect.savedKey'))
    expect(backend.getCloudSecretSummary).toHaveBeenCalledWith({ provider: 'modal', profileId: ID, role: 'setup' })
    expect(screen.queryByLabelText(t('settings.cloud.setup.connect.modalTokenSecret'))).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.failed.resume')))
    await heading('settings.cloud.setup.heading.done')
    expect(runner).toHaveBeenLastCalledWith({
      op: 'resume',
      provider: 'modal',
      params: { saved_setup_credential: ID, installation_id: ID, options: {}, remember_setup_credential: true },
    })
  })

  it('asks for a token again when the saved one is refused', async () => {
    const runner = scripted({ resume: refused('ERR_ACTIONABLE_MISSING_PERMISSION') })
    render(CloudProvisioner, {
      props: { inline: true, runCloudProvisioner: runner, backend: savedBackend(true), existing: { provider: 'modal', installationId: ID, action: 'resume' } },
    })

    await screen.findByText(t('settings.cloud.setup.connect.savedKey'))
    await fireEvent.click(button(t('settings.cloud.setup.failed.resume')))
    await heading('settings.cloud.setup.heading.failed')
    await screen.findByLabelText(t('settings.cloud.setup.connect.modalTokenSecret'))
    expect(button(t('settings.cloud.setup.failed.resume')).disabled).toBe(true)
  })

  it('sends a pasted token with the choice to forget it', async () => {
    const runner = scripted({ resume: installed() })
    render(CloudProvisioner, {
      props: { inline: true, runCloudProvisioner: runner, backend: savedBackend(false), existing: { provider: 'modal', installationId: ID, action: 'resume' } },
    })

    await typeModalKeys()
    await fireEvent.click(screen.getByLabelText(t('settings.cloud.setup.connect.rememberKey')))
    await fireEvent.click(button(t('settings.cloud.setup.failed.resume')))
    await heading('settings.cloud.setup.heading.done')
    expect(runner).toHaveBeenLastCalledWith({
      op: 'resume',
      provider: 'modal',
      params: { credentials: MODAL, installation_id: ID, options: {}, remember_setup_credential: false },
    })
  })
})

describe('the Review step', () => {
  it('a different GPU is a new plan: approval is asked again and focus stays on the select', async () => {
    const second = deferred()
    let plans = 0
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: () => {
        plans += 1
        return plans === 1 ? ok(planData()) : second.promise
      },
    })
    render(CloudProvisioner, {
      props: { inline: true, installationId: ID, runCloudProvisioner: runner, backend: handBackend().backend },
    })
    await connectModal()
    await fireEvent.click(approval())
    expect(approval().checked).toBe(true)

    const gpu = /** @type {HTMLSelectElement} */ (screen.getByLabelText(t('settings.cloud.setup.review.gpu')))
    gpu.focus()
    await fireEvent.change(gpu, { target: { value: 'A10' } })
    expect(runner).toHaveBeenLastCalledWith({
      op: 'plan',
      provider: 'modal',
      params: { credentials: MODAL, installation_id: ID, options: { gpu: 'A10' } },
    })
    expect(gpu.disabled).toBe(true)
    expect(approval().disabled).toBe(true)
    dropFocus()
    expect(document.activeElement).toBe(document.body)

    const replanned = 'cd'.repeat(32)
    second.resolve(ok(planData({ gpu: 'A10', hash: replanned })))
    await waitFor(() => expect(gpu.disabled).toBe(false))
    await waitFor(() => expect(document.activeElement).toBe(gpu))
    expect(gpu.value).toBe('A10')
    expect(approval().checked).toBe(false)
    expect(button(t('settings.cloud.setup.review.start')).disabled).toBe(true)

    // The plan's hash, which apply repeats back, is behind a disclosure.
    expect(screen.queryByText(replanned)).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.review.details')))
    expect(screen.getByText(replanned)).toBeTruthy()
  })

  it('a refused key is said on the Connect step, with focus back on Continue', async () => {
    const inspected = deferred()
    const runner = scripted({ inspect: () => inspected.promise })
    render(CloudProvisioner, { props: { inline: true, runCloudProvisioner: runner, backend: handBackend().backend } })
    await typeModalKeys()

    const go = button(t('settings.cloud.setup.connect.continue'))
    go.focus()
    expect(document.activeElement).toBe(go)
    await fireEvent.click(go)
    expect(go.textContent?.trim()).toBe(t('settings.cloud.setup.connect.checking'))
    expect(go.disabled).toBe(true)
    dropFocus()

    inspected.resolve(refused('ERR_ACTIONABLE_MISSING_PERMISSION'))
    const alert = await screen.findByRole('alert')
    expect(alert.textContent).toContain(t('settings.cloud.setup.error.permission'))
    expect(alert.textContent).toContain(t('settings.cloud.setup.failed.code', { code: 'ERR_ACTIONABLE_MISSING_PERMISSION' }))
    await waitFor(() => expect(document.activeElement).toBe(go))
    expect(screen.getByRole('heading', { name: t('settings.cloud.setup.heading.connect') })).toBeTruthy()
  })

  it('Cancel wipes the keys before it hands back', async () => {
    const onclose = vi.fn()
    render(CloudProvisioner, { props: { inline: true, backend: handBackend().backend, onclose } })
    await typeModalKeys()
    await fireEvent.click(button(t('shell.action.cancel')))
    expect(onclose).toHaveBeenCalledTimes(1)
    for (const key of ['settings.cloud.setup.connect.modalTokenId', 'settings.cloud.setup.connect.modalTokenSecret']) {
      expect(/** @type {HTMLInputElement} */ (screen.getByLabelText(t(key))).value).toBe('')
    }
  })
})

describe('deleting what a setup created', () => {
  it('lists it, asks for the key again, and removes the endpoint once it is gone', async () => {
    const backend = createMockBackend({ timing: ZERO })
    const credentials = { token_id: 'ak-old', token_secret: 'as-old' }
    const planned = await backend.runCloudProvisioner({
      op: 'plan',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-old001' },
    })
    await backend.runCloudProvisioner({
      op: 'apply',
      provider: 'modal',
      params: { credentials, installation_id: 'mc-old001', approved_plan_hash: planned.data.plan_hash },
    })
    const oncleaned = vi.fn()
    const onbusychange = vi.fn()
    render(CloudProvisioner, {
      props: {
        inline: true,
        existing: { provider: 'modal', installationId: 'mc-old001', action: 'cleanup' },
        backend,
        oncleaned,
        onbusychange,
      },
    })

    await heading('settings.cloud.setup.heading.cleanup')
    await screen.findByText(t('settings.cloud.setup.cleanup.lead', { providerKey: MODAL_KEY }))
    for (const name of ['mc-weights-mc-old001', 'mc-jobs-mc-old001', 'mc-mc-old001', 'mc-token-mc-old001']) {
      expect(screen.getByText(name)).toBeTruthy()
    }
    const remove = button(t('settings.cloud.setup.cleanup.delete'))
    expect(remove.disabled).toBe(true)
    await typeKey('settings.cloud.setup.connect.modalTokenId', 'ak-old')
    await typeKey('settings.cloud.setup.connect.modalTokenSecret', 'as-old')
    expect(remove.disabled).toBe(true)
    await fireEvent.click(screen.getByRole('checkbox', { name: t('settings.cloud.setup.cleanup.approve') }))
    await fireEvent.click(remove)

    await heading('settings.cloud.setup.heading.cleaned')
    expect(screen.getByText(t('settings.cloud.setup.cleanup.done', { providerKey: MODAL_KEY }))).toBeTruthy()
    await waitFor(() => expect(oncleaned).toHaveBeenCalledTimes(1))
    const config = await backend.readInferenceConfig()
    expect(config.modalProfiles['mc-old001']).toBeUndefined()
    expect(config.selectedTarget).toEqual({ type: 'local' })
    await waitFor(() => expect(onbusychange.mock.calls.at(-1)?.[0]).toBe(false))
  })

  it('forgets a setup that left nothing behind, without asking for a key', async () => {
    const oncleaned = vi.fn()
    render(CloudProvisioner, {
      props: {
        inline: true,
        existing: { provider: 'beam', installationId: 'mc-gone01', action: 'cleanup' },
        backend: createMockBackend({ timing: ZERO }),
        oncleaned,
      },
    })
    await screen.findByText(t('settings.cloud.setup.cleanup.none', { providerKey: BEAM_KEY }))
    expect(screen.queryByLabelText(t('settings.cloud.setup.connect.beamToken'))).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.cleanup.forget')))
    await heading('settings.cloud.setup.heading.cleaned')
    expect(screen.getByText(t('settings.cloud.setup.cleanup.doneEmpty', { providerKey: BEAM_KEY }))).toBeTruthy()
    expect(oncleaned).toHaveBeenCalledTimes(1)
  })
})

describe('a Modal token setup could not record', () => {
  it('names it, lists the way out in order, offers Clean up and not Resume, and shows none of the helper’s own words', async () => {
    const orphaned = {
      success: false,
      data: null,
      error: {
        code: 'ERR_ORPHANED_TOKEN',
        message: 'helper message with as-leaked-secret',
        actionable_guidance: 'helper guidance with as-leaked-secret',
        remedy_steps: ['helper step one with as-leaked-secret', 'helper step two'],
      },
    }
    const runner = scripted({
      inspect: ok({ eligible: true }),
      plan: ok(planData()),
      apply: orphaned,
      cleanup_plan: ok({ plan_hash: HASH, resources_to_delete: [{ type: 'app', name: `mc-${ID}` }], foreign_resources_ignored: [] }),
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, runCloudProvisioner: runner, backend: handBackend().backend } })
    await connectModal()
    await approveAndStart()

    await heading('settings.cloud.setup.heading.failed')
    const alert = screen.getByRole('alert')
    expect(alert.textContent).toContain(t('settings.cloud.setup.error.orphanedToken'))
    expect(alert.textContent).not.toContain(t('settings.cloud.setup.error.generic'))
    expect(alert.textContent).toContain(t('settings.cloud.setup.failed.code', { code: 'ERR_ORPHANED_TOKEN' }))

    // The three steps, in the catalogue's words and in order, under a heading
    // that names them.
    const steps = screen.getByRole('list', { name: t('settings.cloud.setup.orphaned.heading') })
    expect(within(steps).getAllByRole('listitem').map((item) => item.textContent)).toEqual([
      t('settings.cloud.setup.orphaned.dashboard'),
      t('settings.cloud.setup.orphaned.cleanup'),
      t('settings.cloud.setup.orphaned.again'),
    ])
    expect(document.body.textContent).not.toContain('as-leaked-secret')
    expect(document.body.textContent).not.toContain('helper step two')

    // Resume stays blocked; nothing here says it would pick up.
    expect(screen.queryByRole('button', { name: t('settings.cloud.setup.failed.resume') })).toBeNull()
    expect(screen.queryByText(t('settings.cloud.setup.failed.kept'))).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.failed.cleanup')))
    await heading('settings.cloud.setup.heading.cleanup')
    expect(runner).toHaveBeenLastCalledWith({ op: 'cleanup_plan', provider: 'modal', params: { installation_id: ID } })
  })
})

describe('a setup that already exists', () => {
  const FOUND = 'mc-k3v9qa'

  it('offers what the account holds first, and reusing it plans with its id and downloads nothing', async () => {
    const backend = createMockBackend({ timing: ZERO, cloudExisting: '1' })
    /** @type {any[]} */
    const progress = []
    backend.onProvisionProgress((/** @type {any} */ event) => progress.push(event))
    const runner = vi.fn((/** @type {any} */ spec) => backend.runCloudProvisioner(spec))
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, runCloudProvisioner: runner } })

    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    // Nothing is planned until a choice is made.
    expect(runner.mock.calls.map(([spec]) => spec.op)).toEqual(['inspect'])
    const choice = /** @type {HTMLInputElement} */ (screen.getByRole('radio', { name: new RegExp(FOUND) }))
    expect(choice.checked).toBe(true)
    expect(screen.getByText(t('settings.cloud.setup.found.models', { models: PINNED_CLOUD_MODEL_ID.split('/')[1] }))).toBeTruthy()
    // A new setup stays on offer, second, and says what it costs.
    expect(button(t('settings.cloud.setup.found.new'))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.found.newNote', { size: '5.5' }))).toBeTruthy()

    await fireEvent.click(button(t('settings.cloud.setup.found.use')))
    await heading('settings.cloud.setup.heading.review')
    const planned = runner.mock.calls.find(([spec]) => spec.op === 'plan')?.[0]
    expect(planned.params.installation_id).toBe(FOUND)
    expect(planned.params.options).toEqual({ gpu: 'L4', idle_seconds: 120, model_id: PINNED_CLOUD_MODEL_ID, analysis_models: [], denoise: false })
    expect(screen.getByText(t('settings.cloud.setup.review.leadReuse', { providerKey: MODAL_KEY, id: FOUND }))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.review.weightsReused'))).toBeTruthy()
    expect(screen.queryByText(t('settings.cloud.setup.review.weights', { size: '5.5' }))).toBeNull()
    expect(screen.getByText(t('settings.cloud.setup.review.tokenReuse'))).toBeTruthy()

    await fireEvent.click(screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.approveReuse', { providerKey: MODAL_KEY }) }))
    await fireEvent.click(button(t('settings.cloud.setup.review.startReuse')))
    await screen.findByRole('heading', { name: t('settings.cloud.setup.heading.done') }, { timeout: 10_000 })
    const applied = runner.mock.calls.find(([spec]) => spec.op === 'apply')?.[0]
    expect(applied.params.installation_id).toBe(FOUND)
    // The weights step finished without a download to report.
    expect(progress.filter((event) => event.step === 'weights').map((event) => event.pct)).toEqual([0, 100])
    const config = await backend.readInferenceConfig()
    expect(config.selectedTarget).toEqual({ type: 'modal', profile_id: FOUND })
  }, 15_000)

  it('turning Page denoise on for one that recorded it off says its models download', async () => {
    const backend = createMockBackend({ timing: ZERO, cloudExisting: '1' })
    const runner = vi.fn((/** @type {any} */ spec) => backend.runCloudProvisioner(spec))
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, runCloudProvisioner: runner } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    await fireEvent.click(button(t('settings.cloud.setup.found.use')))
    await heading('settings.cloud.setup.heading.review')
    const toggle = /** @type {HTMLInputElement} */ (screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.denoiseInstall') }))
    expect(toggle.checked).toBe(false)
    expect(screen.getByText(t('settings.cloud.setup.review.weightsReused'))).toBeTruthy()

    await fireEvent.click(toggle)
    await waitFor(() => expect(runner).toHaveBeenLastCalledWith(expect.objectContaining({ op: 'plan',
      params: expect.objectContaining({ installation_id: FOUND,
        options: { gpu: 'L4', idle_seconds: 120, model_id: PINNED_CLOUD_MODEL_ID, analysis_models: [], denoise: true } }) })))
    await screen.findByText(t('settings.cloud.setup.review.denoiseBeside'))
    expect(screen.queryByText(t('settings.cloud.setup.review.weightsReused'))).toBeNull()
  })

  it('a changed model says it downloads beside the one there, and Back returns to the choice', async () => {
    const backend = createMockBackend({ timing: ZERO, cloudExisting: '2' })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')

    // The older one never finished its download and recorded no choices.
    await fireEvent.click(screen.getByRole('radio', { name: /mc-p0old7/ }))
    expect(screen.getByText(t('settings.cloud.setup.found.noModels'))).toBeTruthy()
    await fireEvent.click(button(t('settings.cloud.setup.found.useUnready')))
    await heading('settings.cloud.setup.heading.review')
    expect(screen.getByText(t('settings.cloud.setup.review.optionsUnknown'))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.review.weightsBeside', { size: '5.5' }))).toBeTruthy()

    await fireEvent.click(button(t('settings.cloud.setup.review.back')))
    await heading('settings.cloud.setup.heading.found')
  })

  it('a new setup from the choice keeps its own new id and the usual words', async () => {
    const backend = createMockBackend({ timing: ZERO, cloudExisting: '1' })
    const runner = vi.fn((/** @type {any} */ spec) => backend.runCloudProvisioner(spec))
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, runCloudProvisioner: runner } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    await fireEvent.click(button(t('settings.cloud.setup.found.new')))
    await heading('settings.cloud.setup.heading.review')
    expect(runner.mock.calls.find(([spec]) => spec.op === 'plan')?.[0].params.installation_id).toBe(ID)
    expect(screen.getByText(t('settings.cloud.setup.review.lead', { providerKey: MODAL_KEY }))).toBeTruthy()
    expect(screen.getByText(t('settings.cloud.setup.review.weights', { size: '5.5' }))).toBeTruthy()
  })

  it('one this computer can resume is offered as an update, which resumes it', async () => {
    const runner = scripted({
      inspect: ok({
        workspace_name: 'acme',
        existing_installations: [{
          installation_id: 'mc-mine01', app_name: 'mc-mine01', created_at: null, deployed_at: null,
          weights_checked: true, models_ready: [PINNED_CLOUD_MODEL_ID], analysis_ready: [], options: null,
          on_this_computer: true,
        }],
        existing_installations_complete: true,
      }),
      resume: installed(),
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, runCloudProvisioner: runner } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    expect(screen.getByText(t('settings.cloud.setup.found.here'))).toBeTruthy()
    await fireEvent.click(button(t('settings.cloud.setup.found.update')))
    await heading('settings.cloud.setup.heading.update')
    // The keys from Connect are still held: no second ask.
    expect(screen.queryByLabelText(t('settings.cloud.setup.connect.modalTokenId'))).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.guard.update')))
    await waitFor(() => expect(runner.mock.calls.map(([spec]) => spec.op)).toEqual(['inspect', 'resume']))
    expect(runner.mock.calls[1][0].params.installation_id).toBe('mc-mine01')
  })

  it('a refused update names the account, not a plan, and leaves the setup finished', async () => {
    const runner = scripted({
      inspect: ok({
        workspace_name: 'acme',
        existing_installations: [{
          installation_id: 'mc-mine01', app_name: 'mc-mine01', created_at: null, deployed_at: null,
          weights_checked: true, models_ready: [PINNED_CLOUD_MODEL_ID], analysis_ready: [], options: null,
          on_this_computer: true,
        }],
        existing_installations_complete: true,
      }),
      resume: refused('ERR_UNAPPROVED_PLAN'),
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, runCloudProvisioner: runner } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    await fireEvent.click(button(t('settings.cloud.setup.found.update')))
    await heading('settings.cloud.setup.heading.update')
    await fireEvent.click(button(t('settings.cloud.setup.guard.update')))

    await heading('settings.cloud.setup.heading.failed')
    const alert = screen.getByRole('alert')
    expect(alert.textContent).toContain(t('settings.cloud.setup.error.wrongAccount'))
    expect(alert.textContent).not.toContain(t('settings.cloud.setup.error.planChanged'))
    // Nothing ran, so Settings does not offer it as a setup that did not finish.
    expect(setup.unfinished).toBeNull()
    expect(button(t('settings.cloud.setup.failed.resume'))).toBeTruthy()
  })

  it('an update can change the options: it plans them, asks approval, and resumes with that plan', async () => {
    const recorded = { gpu: 'L4', idle_seconds: 120, model_id: PINNED_CLOUD_MODEL_ID, analysis_models: [], denoise: false }
    const runner = scripted({
      inspect: ok({
        workspace_name: 'acme',
        existing_installations: [{
          installation_id: 'mc-mine01', app_name: 'mc-mine01', created_at: null, deployed_at: null,
          weights_checked: true, models_ready: [PINNED_CLOUD_MODEL_ID], analysis_ready: [], options: recorded,
          on_this_computer: true,
        }],
        existing_installations_complete: true,
      }),
      plan: (/** @type {any} */ spec) => {
        const plan = planData({ hash: spec.params.options.denoise ? 'cd'.repeat(32) : HASH })
        plan.installation_id = spec.params.installation_id
        Object.assign(plan.resource_allocation, { denoise: spec.params.options.denoise === true, denoise_supported: true })
        return ok(plan)
      },
      resume: installed(),
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, runCloudProvisioner: runner } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    await fireEvent.click(button(t('settings.cloud.setup.found.update')))
    await heading('settings.cloud.setup.heading.update')
    expect(screen.getByText(t('settings.cloud.setup.update.optionsNote'))).toBeTruthy()

    await fireEvent.click(button(t('settings.cloud.setup.update.options')))
    await heading('settings.cloud.setup.heading.review')
    const planned = runner.mock.calls.filter(([spec]) => spec.op === 'plan').map(([spec]) => spec.params)
    expect(planned).toEqual([expect.objectContaining({ installation_id: 'mc-mine01', options: recorded })])

    await fireEvent.click(screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.denoiseInstall') }))
    await waitFor(() => expect(runner.mock.calls.filter(([spec]) => spec.op === 'plan')).toHaveLength(2))
    await fireEvent.click(screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.approveReuse', { providerKey: MODAL_KEY }) }))
    await fireEvent.click(button(t('settings.cloud.setup.guard.update')))
    await waitFor(() => expect(runner.mock.calls.at(-1)?.[0].op).toBe('resume'))
    const resumed = runner.mock.calls.at(-1)?.[0]
    expect(resumed.params).toMatchObject({
      installation_id: 'mc-mine01',
      approved_plan_hash: 'cd'.repeat(32),
      options: { ...recorded, denoise: true },
    })
    expect(runner.mock.calls.some(([spec]) => spec.op === 'apply')).toBe(false)
  })

  it('with page denoise asked for, a new setup plans it from the start', async () => {
    const backend = createMockBackend({ timing: ZERO })
    const runner = vi.fn((/** @type {any} */ spec) => backend.runCloudProvisioner(spec))
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, runCloudProvisioner: runner, wantDenoise: true } })
    await connectModal()
    const planned = runner.mock.calls.filter(([spec]) => spec.op === 'plan').map(([spec]) => spec.params.options)
    expect(planned).toEqual([{ denoise: true }])
    await waitFor(() => expect(/** @type {HTMLInputElement} */ (screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.denoiseInstall') })).checked).toBe(true))
  })

  it('with page denoise asked for, Update goes through a plan that adds it', async () => {
    const recorded = { gpu: 'L4', idle_seconds: 120, model_id: PINNED_CLOUD_MODEL_ID, analysis_models: [], denoise: false }
    const runner = scripted({
      inspect: ok({
        workspace_name: 'acme',
        existing_installations: [{
          installation_id: 'mc-mine01', app_name: 'mc-mine01', created_at: null, deployed_at: null,
          weights_checked: true, models_ready: [PINNED_CLOUD_MODEL_ID], analysis_ready: [], options: recorded,
          on_this_computer: true,
        }],
        existing_installations_complete: true,
      }),
      plan: (/** @type {any} */ spec) => {
        const plan = planData({ hash: spec.params.options.denoise ? 'cd'.repeat(32) : HASH })
        plan.installation_id = spec.params.installation_id
        Object.assign(plan.resource_allocation, { denoise: spec.params.options.denoise === true, denoise_supported: true })
        return ok(plan)
      },
      resume: installed(),
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, runCloudProvisioner: runner, wantDenoise: true } })
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.found')
    await fireEvent.click(button(t('settings.cloud.setup.found.update')))
    await heading('settings.cloud.setup.heading.update')
    expect(screen.getByText(t('settings.cloud.setup.update.denoiseNote'))).toBeTruthy()
    // No Update that skips the plan: Continue is the only way on.
    expect(screen.queryByRole('button', { name: t('settings.cloud.setup.guard.update') })).toBeNull()

    await fireEvent.click(button(t('settings.cloud.setup.connect.continue')))
    await heading('settings.cloud.setup.heading.review')
    const planned = runner.mock.calls.filter(([spec]) => spec.op === 'plan').map(([spec]) => spec.params)
    expect(planned).toEqual([expect.objectContaining({ installation_id: 'mc-mine01', options: { ...recorded, denoise: true } })])
    await fireEvent.click(screen.getByRole('checkbox', { name: t('settings.cloud.setup.review.approveReuse', { providerKey: MODAL_KEY }) }))
    await fireEvent.click(button(t('settings.cloud.setup.guard.update')))
    await waitFor(() => expect(runner.mock.calls.at(-1)?.[0].op).toBe('resume'))
    expect(runner.mock.calls.at(-1)?.[0].params).toMatchObject({ approved_plan_hash: 'cd'.repeat(32), options: { ...recorded, denoise: true } })
  })

  it('a new setup on a computer that has one points to it first, with Update as the way on', async () => {
    const backend = createMockBackend({ timing: ZERO })
    await backend.writeInferenceConfig({
      config: {
        ...(await backend.readInferenceConfig()),
        modalProfiles: {
          'mc-here01': { id: 'mc-here01', name: 'Modal (mc-here01)', endpointUrl: 'https://ws--mc-here01-gateway.modal.run/mc/v1' },
          ep_manual: { id: 'ep_manual', name: 'By hand', endpointUrl: 'https://example.modal.run/mc/v1' },
        },
      },
    })
    const runner = vi.fn(async () => installed())
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend, runCloudProvisioner: runner } })

    await heading('settings.cloud.setup.heading.existing')
    expect(screen.getByText(t('settings.cloud.setup.guard.lead', { count: 1 }))).toBeTruthy()
    expect(screen.getByText('Modal (mc-here01)')).toBeTruthy()
    expect(screen.queryByText('By hand')).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.guard.update')))
    await heading('settings.cloud.setup.heading.update')
    expect(screen.getByText(t('settings.cloud.setup.resume.update', { providerKey: MODAL_KEY, id: 'mc-here01' }))).toBeTruthy()
    await typeModalKeys()
    await fireEvent.click(button(t('settings.cloud.setup.guard.update')))
    await waitFor(() => expect(runner).toHaveBeenCalledTimes(1))
    expect(runner.mock.calls[0][0]).toMatchObject({ op: 'resume', params: { installation_id: 'mc-here01' } })
  })

  it('the guard lets a second setup through only on an explicit choice', async () => {
    const backend = createMockBackend({ timing: ZERO })
    await backend.writeInferenceConfig({
      config: {
        ...(await backend.readInferenceConfig()),
        modalProfiles: { 'mc-here01': { id: 'mc-here01', name: 'Modal (mc-here01)', endpointUrl: 'https://ws--mc-here01-gateway.modal.run/mc/v1' } },
      },
    })
    render(CloudProvisioner, { props: { inline: true, installationId: ID, backend } })
    await heading('settings.cloud.setup.heading.existing')
    expect(screen.queryByLabelText(t('settings.cloud.setup.connect.modalTokenId'))).toBeNull()
    await fireEvent.click(button(t('settings.cloud.setup.guard.another')))
    await heading('settings.cloud.setup.heading.connect')
    await connectModal()
  })
})
