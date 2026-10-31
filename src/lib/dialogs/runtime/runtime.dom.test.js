/**
 * The Runtime screen's two halves, against a fake backend.
 *
 * This computer: a machine with CUDA 12 and 13 side by side, judged for the
 * build being chosen, says which toolkit and cuDNN that build would use, greys
 * the others, and offers the build the machine is ready for.
 *
 * Measurement: the fastest backend is marked, a backend that could not run
 * says why, a stale result says it is no longer followed, a model run on
 * something other than its winner says where and why, and Measure is offered
 * only once a runtime and a model are here.
 */
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../../api/backend.js'
import { t } from '../../i18n/index.js'
import BuildChoice from './BuildChoice.svelte'
import MachineCheck from './MachineCheck.svelte'
import Measurement from './Measurement.svelte'

/** CUDA 13.0 and 12.6 installed, cuDNN only for 13. */
const REPORT = {
  wanted: 12,
  toolkits: [
    { major: 13, minor: 0, dir: 'C:\\CUDA\\v13.0\\bin', missing: [] },
    { major: 12, minor: 6, dir: 'C:\\CUDA\\v12.6\\bin', missing: [] },
  ],
  cudnn: [{ cudaMajor: 13, dir: 'C:\\CUDNN\\v9.8\\bin\\13.0', missing: [] }],
  gpus: [{ name: 'NVIDIA GeForce RTX 4070', driver: '581.42' }],
  verdictKey: 'accel.cudaCheck.noCudnn',
  verdictHave: null,
  verdictNeed: null,
  recommended: 'cuda13',
}

const FLAVOURS = [{ id: 'directml' }, { id: 'cuda12' }, { id: 'cuda13' }]

/**
 * The same machine judged for whichever build is asked about, as the backend does.
 *
 * @param {{flavour: string}} ask
 */
function reportFor({ flavour }) {
  const wanted = flavour === 'cuda13' ? 13 : flavour === 'cuda12' ? 12 : null
  return { ...REPORT, wanted, verdictKey: wanted === 13 ? 'accel.cudaCheck.ready' : wanted === 12 ? 'accel.cudaCheck.noCudnn' : null }
}

/** @param {Record<string, unknown>} methods */
function fake(methods) {
  const handlers = new Set()
  const backend = {
    subscribe: vi.fn((handler) => {
      handlers.add(handler)
      return () => handlers.delete(handler)
    }),
    /** @param {unknown} event */
    emit(event) {
      for (const handler of [...handlers]) handler(event)
    },
    ...methods,
  }
  setBackend(/** @type {any} */ (backend))
  return backend
}

afterEach(() => {
  cleanup()
  setBackend(null)
})

describe('This computer', () => {
  // On the page, not in a tooltip: a keyboard or touch never sees one, and these
  // are the words a bug report needs.
  it('says the runtime\'s own words beside a path that failed its test run', async () => {
    fake({ runtimeReport: vi.fn(async () => REPORT) })
    const accelerators = {
      preference: 'auto',
      models: [],
      providers: [{
        id: 'cuda', labelKey: 'accel.cuda', available: false, reasonKey: 'accel.declined.selfTest',
        detail: 'Could not locate cudnn_engines_tensor_ir64_9.dll', used: true, measured: false, active: false, selected: false,
      }],
    }
    const rendered = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'windows-x64', accelerators })
    await waitFor(() => expect(rendered.getByText(t('accel.declined.selfTest'))).toBeTruthy())
    expect(rendered.queryByText('Could not locate cudnn_engines_tensor_ir64_9.dll')).not.toBeNull()
  })

  it('asks for a restart only when the install found is not the one loaded', async () => {
    fake({ runtimeReport: vi.fn(async () => ({ ...REPORT, restartNeeded: true })) })
    const rendered = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'windows-x64', accelerators: null })
    await waitFor(() => expect(rendered.getByText(t('settings.runtime.machine.restartNeeded'))).toBeTruthy())
    cleanup()
    fake({ runtimeReport: vi.fn(async () => ({ ...REPORT, restartNeeded: false })) })
    const again = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'windows-x64', accelerators: null })
    await waitFor(() => expect(again.getByText(t('settings.runtime.machine.toolkits'))).toBeTruthy())
    expect(again.queryByText(t('settings.runtime.machine.restartNeeded'))).toBeNull()
  })

  it('judges the build being chosen against the toolkit and cuDNN of its own CUDA major', async () => {
    const backend = fake({ runtimeReport: vi.fn(async () => REPORT) })
    const onchooseflavour = vi.fn()
    const rendered = render(MachineCheck, {
      flavour: 'cuda12',
      flavours: FLAVOURS,
      platform: 'windows-x64',
      accelerators: null,
      pathsPending: true,
      onchooseflavour,
    })
    await waitFor(() => expect(rendered.getByText(t('accel.cudaCheck.noCudnn', { major: 12 }))).toBeTruthy())
    expect(backend.runtimeReport).toHaveBeenCalledWith({ flavour: 'cuda12', rescan: false })
    expect(rendered.getByText(t('settings.runtime.machine.gpuLine', { name: 'NVIDIA GeForce RTX 4070', driver: '581.42' }))).toBeTruthy()
    // CUDA 13 is installed but not this build's, so it reads as set aside.
    expect(rendered.getByText('CUDA 13.0', { selector: '.tag' }).closest('.line')?.classList.contains('off')).toBe(true)
    expect(rendered.getByText('CUDA 12.6', { selector: '.tag' }).closest('.line')?.classList.contains('off')).toBe(false)
    expect(rendered.getByText(t('settings.runtime.machine.pathsLater'))).toBeTruthy()

    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.machine.useBuild', { build: 'CUDA 13' }) }))
    expect(onchooseflavour).toHaveBeenCalledWith('cuda13')
  })

  it('names the CUDA release it found and the one the build needs when the toolkit is too old', async () => {
    fake({
      runtimeReport: vi.fn(async () => ({
        ...REPORT,
        verdictKey: 'accel.cudaCheck.toolkitTooOld',
        verdictHave: '12.6',
        verdictNeed: '12.8',
      })),
    })
    const rendered = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'windows-x64', accelerators: null })
    const text = t('accel.cudaCheck.toolkitTooOld', { major: 12, have: '12.6', need: '12.8' })
    expect(text).toContain('12.6')
    expect(text).toContain('12.8')
    await waitFor(() => expect(rendered.getByText(text)).toBeTruthy())
  })

  it('reads the disk again on Check again', async () => {
    const backend = fake({ runtimeReport: vi.fn(async () => REPORT) })
    const onrecheck = vi.fn()
    const rendered = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'windows-x64', accelerators: null, onrecheck })
    await waitFor(() => expect(backend.runtimeReport).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.machine.recheck') })).toBeTruthy())
    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.machine.recheck') }))
    expect(backend.runtimeReport).toHaveBeenLastCalledWith({ flavour: 'cuda12', rescan: true })
    expect(onrecheck).toHaveBeenCalledTimes(1)
  })

  it('never draws the last build\'s verdict under the new build\'s name', async () => {
    /** @type {(value: unknown) => void} */
    let answer13 = () => {}
    const backend = fake({
      runtimeReport: vi.fn((ask) => (ask.flavour === 'cuda13' ? new Promise((resolve) => (answer13 = resolve)) : Promise.resolve(reportFor(ask)))),
    })
    const rendered = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'windows-x64', accelerators: null })
    await waitFor(() => expect(rendered.getByText(t('accel.cudaCheck.noCudnn', { major: 12 }))).toBeTruthy())

    await rendered.rerender({ flavour: 'cuda13', flavours: FLAVOURS, platform: 'windows-x64', accelerators: null })
    await waitFor(() => expect(backend.runtimeReport).toHaveBeenLastCalledWith({ flavour: 'cuda13', rescan: false }))
    // CUDA 12's answer is still the last one here, and it is not CUDA 13's to show.
    expect(rendered.queryByText(t('accel.cudaCheck.noCudnn', { major: 13 }))).toBeNull()
    expect(rendered.queryByText(t('accel.cudaCheck.noCudnn', { major: 12 }))).toBeNull()

    answer13(reportFor({ flavour: 'cuda13' }))
    await waitFor(() => expect(rendered.getByText(t('accel.cudaCheck.ready'))).toBeTruthy())
  })

  it('says when the disk could not be read', async () => {
    fake({ runtimeReport: vi.fn(async () => Promise.reject(new Error('io'))) })
    const rendered = render(MachineCheck, { flavour: 'cuda12', flavours: FLAVOURS, platform: 'linux-x64', accelerators: null })
    await waitFor(() => expect(rendered.getByText(t('settings.runtime.machine.unreadable'))).toBeTruthy())
  })

  it('asks nothing about NVIDIA on a Mac', async () => {
    const backend = fake({ runtimeReport: vi.fn(async () => REPORT) })
    const rendered = render(MachineCheck, { flavour: 'stock', platform: 'macos-arm64', accelerators: null })
    expect(rendered.queryByText(t('settings.runtime.machine.gpu'))).toBeNull()
    expect(backend.runtimeReport).not.toHaveBeenCalled()
  })

  it('shows Check again working on a Mac too, where only the paths are read', async () => {
    const backend = fake({ runtimeReport: vi.fn(async () => REPORT) })
    /** @type {() => void} */
    let finish = () => {}
    const onrecheck = vi.fn(() => new Promise((resolve) => (finish = () => resolve(undefined))))
    const rendered = render(MachineCheck, { flavour: 'stock', platform: 'macos-arm64', accelerators: null, onrecheck })
    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.machine.recheck') }))
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.machine.checking') })).toBeTruthy())
    // A second press while it reads asks nothing twice.
    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.machine.checking') }))
    expect(onrecheck).toHaveBeenCalledTimes(1)
    finish()
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.machine.recheck') })).toBeTruthy())
    expect(backend.runtimeReport).not.toHaveBeenCalled()
  })
})

describe('Build', () => {
  const BUILDS = [
    { id: 'directml', bytes: 1, isDefault: true, userInstalled: [] },
    { id: 'cuda12', bytes: 1, isDefault: false, userInstalled: [] },
  ]
  /** @param {ReturnType<typeof render>} rendered @param {string} name */
  const radio = (rendered, name) => /** @type {HTMLInputElement} */ (rendered.getByRole('radio', { name: new RegExp(name) }))

  it('draws the saved build, never a click the choice did not keep', async () => {
    const onchoose = vi.fn()
    const rendered = render(BuildChoice, { flavours: BUILDS, value: 'directml', onchoose })
    await fireEvent.click(radio(rendered, 'CUDA 12'))
    expect(onchoose).toHaveBeenCalledWith('cuda12')
    // Refused or failed: `value` did not move, and neither does the mark.
    expect(radio(rendered, 'DirectML').checked).toBe(true)
    expect(radio(rendered, 'CUDA 12').checked).toBe(false)

    // Saved: the mark follows `value`.
    await rendered.rerender({ flavours: BUILDS, value: 'cuda12', onchoose })
    expect(radio(rendered, 'CUDA 12').checked).toBe(true)
    expect(radio(rendered, 'DirectML').checked).toBe(false)
  })
})

/** @param {Partial<import('../../api/backend.js').MeasurementView>} [patch] */
function saved(patch = {}) {
  return {
    saved: {
      version: 1,
      measuredAt: Date.UTC(2026, 9, 25, 9),
      setup: 'x',
      models: [
        {
          id: 'inpainter',
          winner: 'cuda',
          results: [
            { backend: 'cpu', loadMs: 300, firstMs: 900, medianMs: 800, runs: 3, error: null },
            { backend: 'cuda', loadMs: 1200, firstMs: 400, medianMs: 120, runs: 10, error: null },
            { backend: 'directml', loadMs: null, firstMs: null, medianMs: null, runs: 0, error: 'accel.declined.memory' },
          ],
        },
      ],
    },
    current: true,
    running: false,
    ...patch,
  }
}

const PLACED = {
  preference: 'auto',
  providers: [],
  models: [{ id: 'inpainter', modelName: 'LaMa Manga', acceleratorId: 'cuda', preference: 'auto', backendStatus: [] }],
}

describe('Measurement', () => {
  it('marks the fastest, says why a backend could not run, and measures again on a press', async () => {
    const backend = fake({
      readMeasurement: vi.fn(async () => saved()),
      measureBackends: vi.fn(async () => saved()),
    })
    const onmeasured = vi.fn()
    const rendered = render(Measurement, { accelerators: PLACED, ready: true, onmeasured })
    await waitFor(() => expect(rendered.getByText('LaMa Manga')).toBeTruthy())
    const fastest = rendered.getByText(t('settings.runtime.measure.fastest')).closest('tr')
    expect(fastest?.querySelector('th')?.textContent).toBe(t('accel.cuda'))
    expect(rendered.getByText(t('accel.declined.memory'))).toBeTruthy()
    // The model already runs on its winner: nothing to explain.
    expect(rendered.queryByText(t('settings.runtime.measure.autoUses', { backend: t('accel.cuda') }))).toBeNull()

    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.measure.again') }))
    await waitFor(() => expect(onmeasured).toHaveBeenCalledTimes(1))
    expect(backend.measureBackends).toHaveBeenCalledTimes(1)
  })

  it('says a result from another setup is not followed, and where a hand-picked model runs', async () => {
    fake({ readMeasurement: vi.fn(async () => saved({ current: false })) })
    const placed = structuredClone(PLACED)
    placed.models[0].acceleratorId = 'cpu'
    placed.models[0].preference = 'cpu'
    const rendered = render(Measurement, { accelerators: placed, ready: true })
    await waitFor(() => expect(rendered.getByText(t('settings.runtime.measure.stale'))).toBeTruthy())
    expect(rendered.getByText(t('settings.runtime.measure.chosenUses', { backend: t('accel.cpu') }))).toBeTruthy()
  })

  it('settles a run this screen did not start when its done event arrives', async () => {
    const backend = fake({
      readMeasurement: vi.fn(async () => ({ saved: null, current: null, running: true })),
    })
    const onmeasured = vi.fn()
    const rendered = render(Measurement, { accelerators: PLACED, ready: true, onmeasured })
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.measure.stop') })).toBeTruthy())
    backend.emit({ type: 'measure-progress', model: 'inpainter', backend: 'cuda', step: 2, total: 3, done: false })
    await waitFor(() =>
      expect(rendered.getByText(t('settings.runtime.measure.progress', { model: 'LaMa Manga', backend: t('accel.cuda'), step: 2, total: 3 }))).toBeTruthy(),
    )

    backend.readMeasurement.mockImplementation(async () => saved())
    backend.emit({ type: 'measure-progress', model: '', backend: '', step: 3, total: 3, done: true })
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.measure.again') })).toBeTruthy())
    expect(rendered.getByText('LaMa Manga')).toBeTruthy()
    expect(onmeasured).toHaveBeenCalledTimes(1)
  })

  it('brings Measure back after a Stop on a run this screen did not start, though the read after it fails', async () => {
    const backend = fake({
      readMeasurement: vi.fn(async () => ({ saved: null, current: null, running: true })),
      cancelMeasurement: vi.fn(async () => true),
    })
    const rendered = render(Measurement, { accelerators: PLACED, ready: true })
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.measure.stop') })).toBeTruthy())
    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.measure.stop') }))
    await waitFor(() => expect(/** @type {HTMLButtonElement} */ (rendered.getByRole('button', { name: t('settings.runtime.measure.stop') })).disabled).toBe(true))

    backend.readMeasurement.mockImplementation(async () => Promise.reject(new Error('io')))
    backend.emit({ type: 'measure-progress', model: '', backend: '', step: 1, total: 3, done: true })
    await waitFor(() => expect(/** @type {HTMLButtonElement} */ (rendered.getByRole('button', { name: t('settings.runtime.measure.start') })).disabled).toBe(false))
    expect(backend.readMeasurement).toHaveBeenCalledTimes(2)
  })

  it('says a Stop plainly, not as a failure, and keeps the earlier result', async () => {
    /** @type {(reason: unknown) => void} */
    let reject = () => {}
    const backend = fake({
      readMeasurement: vi.fn(async () => saved()),
      measureBackends: vi.fn(() => new Promise((_, no) => (reject = no))),
      cancelMeasurement: vi.fn(async () => {
        reject('notice.measure.cancelled')
        return true
      }),
    })
    const rendered = render(Measurement, { accelerators: PLACED, ready: true })
    await waitFor(() => expect(rendered.getByRole('button', { name: t('settings.runtime.measure.again') })).toBeTruthy())
    const button = rendered.getByRole('button', { name: t('settings.runtime.measure.again') })
    await fireEvent.click(button)
    // The same button, now Stop, so focus stays on it.
    expect(rendered.getByRole('button', { name: t('settings.runtime.measure.stop') })).toBe(button)
    await fireEvent.click(button)
    await waitFor(() => expect(rendered.getByText(t('notice.measure.cancelled'))).toBeTruthy())
    expect(backend.cancelMeasurement).toHaveBeenCalledTimes(1)
    expect(rendered.queryByRole('alert')).toBeNull()
    expect(rendered.getByText('LaMa Manga')).toBeTruthy()
  })

  it('shows a refused measurement as an alert, and goes back to Measure', async () => {
    fake({
      readMeasurement: vi.fn(async () => ({ saved: null, current: null, running: false })),
      measureBackends: vi.fn(async () => Promise.reject('notice.measure.busy')),
    })
    const rendered = render(Measurement, { accelerators: PLACED, ready: true })
    await waitFor(() => expect(/** @type {HTMLButtonElement} */ (rendered.getByRole('button', { name: t('settings.runtime.measure.start') })).disabled).toBe(false))
    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.measure.start') }))
    expect((await rendered.findByRole('alert')).textContent).toBe(t('notice.measure.busy'))
    expect(rendered.getByRole('button', { name: t('settings.runtime.measure.start') })).toBeTruthy()
  })

  it('clears a saved result on Clear', async () => {
    const backend = fake({
      readMeasurement: vi.fn(async () => saved()),
      forgetMeasurement: vi.fn(async () => ({ saved: null, current: null, running: false })),
    })
    const onmeasured = vi.fn()
    const rendered = render(Measurement, { accelerators: PLACED, ready: true, onmeasured })
    await waitFor(() => expect(rendered.getByText('LaMa Manga')).toBeTruthy())
    await fireEvent.click(rendered.getByRole('button', { name: t('settings.runtime.measure.forget') }))
    await waitFor(() => expect(rendered.getByText(t('settings.runtime.measure.none'))).toBeTruthy())
    expect(rendered.queryByText('LaMa Manga')).toBeNull()
    expect(backend.forgetMeasurement).toHaveBeenCalledTimes(1)
    expect(onmeasured).toHaveBeenCalledTimes(1)
  })

  it('offers Measure only once a runtime and a model are here', async () => {
    fake({ readMeasurement: vi.fn(async () => ({ saved: null, current: null, running: false })) })
    const rendered = render(Measurement, { accelerators: null, ready: false })
    await waitFor(() => expect(rendered.getByText(t('settings.runtime.measure.needsRuntime'))).toBeTruthy())
    expect(/** @type {HTMLButtonElement} */ (rendered.getByRole('button', { name: t('settings.runtime.measure.start') })).disabled).toBe(true)
  })
})
