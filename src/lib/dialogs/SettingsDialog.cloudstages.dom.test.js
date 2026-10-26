/**
 * Settings > Models' detection, and where it runs.
 *
 * Where a run goes is the Text cleanup panel's choice (*Detect on*, *Clean
 * on*); Settings holds no run-target control of its own. Detection is one
 * choice, Normal or Advanced; the model files sit behind More options with
 * their status and install or remove, never a box that builds a combination.
 * While detection is sent to the cloud GPU the text reader is fixed on, as
 * the cloud run uses it.
 *
 * A hand-written seam stub, as in `SettingsDialog.readiness.dom.test.js`.
 */

import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, within } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { DETECTOR_MODEL_NAMES } from '../model/model-names.js'
import { session, setAnalysisTarget, setCleanTarget, setDetectionMode, setOcrRescue } from '../state/session.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'
import { resetFirstLaunch } from './firstlaunch.svelte.js'

const SPEC = {
  id: 'modal-1', kind: 'settings', titleKey: 'modal.title.settings', props: {},
  actions: [{ id: 'close', labelKey: 'shell.action.close' }], blocking: false, dismissable: true, onresolve: null,
}

async function openModels() {
  const api = {
    listModels: vi.fn(async () => null),
    listAccelerators: vi.fn(async () => ({ providers: [], models: [] })),
    listSidecarModels: vi.fn(async () => []),
    sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
    about: vi.fn(async () => ({ appVersion: '0.0.0-test', facts: [] })),
    diagnostics: vi.fn(async () => ({ appVersion: '0.0.0-test', components: [] })),
    subscribe: vi.fn(() => () => {}),
    writeSettings: vi.fn(async () => ({})),
    listWorkflowCapabilities: vi.fn(async () => null),
    readInferenceConfig: async () => ({ schemaVersion: 1, selectedTarget: { type: 'local' }, beamProfiles: {}, modalProfiles: {} }),
    readMeasurement: vi.fn(async () => ({ saved: null, current: null, running: false })),
  }
  setBackend(/** @type {any} */ (api))
  const rendered = render(SettingsDialog, { props: { spec: { ...SPEC, props: { tab: 'models' } } } })
  const tab = rendered.getByRole('tab', { name: t('settings.section.models') })
  const panel = /** @type {HTMLElement} */ (rendered.container.querySelector(`#${tab.getAttribute('aria-controls')}`))
  return { api, rendered, panel, scoped: within(panel) }
}

/** Open More options under the mode cards, which holds the model rows and the reader. @param {HTMLElement} panel */
async function openMore(panel) {
  const summary = /** @type {HTMLElement} */ (panel.querySelector('[data-settings-anchor="detection-more"] button[aria-expanded]'))
  if (summary.getAttribute('aria-expanded') !== 'true') await fireEvent.click(summary)
}

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  setBackend(null)
  setAnalysisTarget('samTs', 'local')
  setAnalysisTarget('rtFull', 'local')
  setDetectionMode('normal')
  setOcrRescue(false)
  setCleanTarget('local')
  vi.clearAllMocks()
})

it('holds no run-target control: Detect on and Clean on are the Text cleanup panel’s', async () => {
  const { rendered } = await openModels()
  const screen = rendered.container
  expect(screen.querySelector('.run-on, .clean-on, #settings-clean-on, [data-stage]')).toBeNull()
  expect(screen.textContent).not.toContain(t('settings.detection.cleanOn.saveFailed'))
  // No select anywhere in Settings offers Cloud GPU as a place to run.
  for (const select of screen.querySelectorAll('select')) {
    expect([...select.options].map((option) => option.value)).not.toContain('cloud')
  }
})

it('lists the detection models behind More options by their names, with no box to tick', async () => {
  const { panel } = await openModels()
  const group = /** @type {HTMLElement} */ (panel.querySelector('[data-settings-anchor="detection"]'))
  expect(group.querySelector('[data-model]')).toBeNull()
  await openMore(panel)
  const names = [...group.querySelectorAll('[data-model] .row-name')].map((name) => name.textContent?.trim())
  for (const id of /** @type {const} */ (['ctd', 'rtFull', 'rtSmall', 'samTs'])) expect(names).toContain(DETECTOR_MODEL_NAMES[id])
  expect(group.querySelectorAll('[data-model] input[type="checkbox"]')).toHaveLength(0)
  expect(panel.textContent).not.toContain(t('pipelines.cloudCombo'))
})

it('fixes the text reader on while detection is sent to the cloud GPU, and keeps this computer’s own switch', async () => {
  setOcrRescue(false)
  setAnalysisTarget('samTs', 'cloud')
  setAnalysisTarget('rtFull', 'cloud')
  const { panel } = await openModels()
  await openMore(panel)
  const reader = /** @type {HTMLInputElement} */ (panel.querySelector('#settings-ocr-rescue'))
  expect(reader.checked).toBe(true)
  expect(reader.disabled).toBe(true)
  await fireEvent.click(reader)
  expect(session.ocrRescue).toBe(false)
})

it('leaves the text reader to the user while detection stays on this computer', async () => {
  setDetectionMode('advanced')
  const { panel } = await openModels()
  await openMore(panel)
  const reader = /** @type {HTMLInputElement} */ (panel.querySelector('#settings-ocr-rescue'))
  expect(reader.checked).toBe(true)
  expect(reader.disabled).toBe(false)
  await fireEvent.click(reader)
  expect(session.ocrRescue).toBe(false)
  expect(session.detectionMode).toBe('advanced')
})
