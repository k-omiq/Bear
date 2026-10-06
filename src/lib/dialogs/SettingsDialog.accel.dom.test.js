import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { session, setAnalysisTarget, setModelAccelerator } from '../state/session.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'

const SPEC = {
  id: 'settings-test', kind: 'settings', titleKey: 'modal.title.settings', props: {},
  actions: [], blocking: false, dismissable: true, onresolve: null,
}

afterEach(() => {
  cleanup()
  setBackend(null)
  setModelAccelerator('samTs', 'inherit')
  setAnalysisTarget('samTs', 'local')
  setAnalysisTarget('rtFull', 'local')
})

it('shows the native model capability matrix and persists a backend choice for the next session', async () => {
  const writeSettings = vi.fn(async () => ({}))
  const accelerators = {
    preference: 'auto',
    providers: [
      { id: 'cpu', labelKey: 'accel.cpu', available: true, measured: true, active: true, selected: false },
      { id: 'webgpu', labelKey: 'accel.webgpu', available: true, measured: false, active: false, selected: false },
      { id: 'cuda', labelKey: 'accel.cuda', available: false, measured: false, active: false, selected: false },
    ],
    models: [{
      id: 'samTs', modelName: 'SAM-TS-L lettering mask', modelKey: 'models.kind.samTs', preference: 'auto',
      supportedIds: ['cpu', 'webgpu'], acceleratorId: 'cpu', labelKey: 'accel.cpu',
      noteKey: null, declinedKey: null, declinedId: null, neededBytes: null, roomBytes: null,
      backendStatus: [
        { id: 'cpu', supported: true, installed: true, available: true, verified: true, reasonKey: null },
        { id: 'webgpu', supported: true, installed: true, available: true, verified: false, reasonKey: null },
        { id: 'cuda', supported: false, installed: false, available: false, verified: false, reasonKey: null },
      ],
    }],
  }
  setBackend(/** @type {any} */ ({
    listModels: vi.fn(async () => null),
    listAccelerators: vi.fn(async () => structuredClone(accelerators)),
    listSidecarModels: vi.fn(async () => []),
    sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
    about: vi.fn(async () => ({ appVersion: 'test', facts: [] })),
    subscribe: vi.fn(() => () => {}),
    writeSettings,
    readInferenceConfig: vi.fn(async () => ({ schemaVersion: 1, selectedTarget: { type: 'local' }, beamProfiles: {}, modalProfiles: {} })),
  }))

  const rendered = render(SettingsDialog, { props: { spec: SPEC } })
  await fireEvent.click(rendered.getByRole('tab', { name: 'Performance' }))
  // Collapsed until asked for: one summary line with how many models it holds.
  const summary = await rendered.findByRole('button', { name: /Model execution/ })
  expect(summary.getAttribute('aria-expanded')).toBe('false')
  expect(summary.textContent).toContain('1 model')
  expect(rendered.queryByRole('combobox', { name: 'Local backend for SAM-TS-L lettering mask' })).toBeNull()
  await fireEvent.click(summary)
  const picker = /** @type {HTMLSelectElement} */ (await rendered.findByRole('combobox', { name: 'Local backend for SAM-TS-L lettering mask' }))
  const cuda = /** @type {HTMLOptionElement} */ (picker.querySelector('option[value="cuda"]'))
  expect(cuda.disabled).toBe(true)
  expect(picker.textContent).toContain('WebGPU · available; execution has not been verified')
  // Where it runs is Text cleanup's choice; this row only says so while it is the cloud.
  expect(rendered.queryByText(t('settings.detection.runOn.performance'))).toBeNull()
  setAnalysisTarget('samTs', 'cloud')
  await waitFor(() => expect(rendered.getByText(t('settings.detection.runOn.performance'))).not.toBeNull())
  setAnalysisTarget('samTs', 'local')

  await fireEvent.change(picker, { target: { value: 'webgpu' } })
  await waitFor(() => expect(writeSettings).toHaveBeenCalledWith(expect.objectContaining({
    modelAccelerators: { samTs: 'webgpu' },
  })))
  expect(session.modelAccelerators.samTs).toBe('webgpu')

  accelerators.models[0].preference = 'cuda'
  accelerators.models[0].declinedKey = 'accel.declined.unavailable'
  accelerators.models[0].declinedId = 'cuda'
  await fireEvent.change(picker, { target: { value: 'auto' } })
  await waitFor(() => expect(rendered.getByText(/The next run will stop until you choose another backend/)).not.toBeNull())
  expect(rendered.queryByText('Next session: CPU')).toBeNull()
  // A full Settings mount, a disclosure and three round trips: more than the
  // default five seconds on a loaded machine.
}, 15_000)
