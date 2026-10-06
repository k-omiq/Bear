/**
 * Settings > Models' four detection choices, and where each one runs.
 *
 * Where a run goes is the Text cleanup panel's choice (*Detect on*, *Clean
 * on*); Settings holds no run-target control of its own. What it does say,
 * under each choice, is whether the model has a cloud version. While
 * detection is sent to the cloud GPU the combination is fixed to all four
 * (CTD + Full + SAM-TS-L + the text reader): the boxes show it checked and
 * cannot be changed, each row says where its model runs, and one note says
 * why. Full and Small are one detector's two profiles and exclude each
 * other; the last model selected cannot be cleared.
 *
 * A hand-written seam stub, as in `SettingsDialog.readiness.dom.test.js`.
 */

import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { DETECTOR_MODEL_NAMES } from '../model/model-names.js'
import { session, setAnalysisTarget, setCleanTarget, setDetectorModels, setOcrRescue } from '../state/session.svelte.js'
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
  }
  setBackend(/** @type {any} */ (api))
  const rendered = render(SettingsDialog, { props: { spec: { ...SPEC, props: { tab: 'models' } } } })
  const tab = rendered.getByRole('tab', { name: t('settings.section.models') })
  const panel = /** @type {HTMLElement} */ (rendered.container.querySelector(`#${tab.getAttribute('aria-controls')}`))
  return { api, rendered, panel, scoped: within(panel) }
}

/** @param {ReturnType<typeof within>} scoped @param {keyof typeof DETECTOR_MODEL_NAMES} id */
const choice = (scoped, id) => /** @type {HTMLInputElement} */ (scoped.getByRole('checkbox', { name: DETECTOR_MODEL_NAMES[id] }))

/** Open the collapsed Language filtering, which holds the text reader switch. @param {ReturnType<typeof within>} scoped */
async function openFiltering(scoped) {
  const summary = scoped.getByRole('button', { name: new RegExp(t('settings.detection.capability.japanese')) })
  if (summary.getAttribute('aria-expanded') !== 'true') await fireEvent.click(summary)
}

/** @param {HTMLElement} panel @param {string} id */
const where = (panel, id) => /** @type {HTMLElement} */ (panel.querySelector(`[data-model="${id}"] .row-where`))

afterEach(() => {
  cleanup()
  resetFirstLaunch()
  setBackend(null)
  setAnalysisTarget('samTs', 'local')
  setAnalysisTarget('rtFull', 'local')
  setDetectorModels(['ctd', 'rtSmall'])
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

it('lists the four choices by their names, in one group, with the cloud-capable two said as such', async () => {
  const { panel, scoped } = await openModels()
  const group = /** @type {HTMLElement} */ (panel.querySelector('[data-settings-anchor="detection"]'))
  const names = [...group.querySelectorAll('.row.choice .row-name')].map((name) => name.textContent?.trim())
  expect(names).toEqual([DETECTOR_MODEL_NAMES.ctd, DETECTOR_MODEL_NAMES.rtFull, DETECTOR_MODEL_NAMES.rtSmall, DETECTOR_MODEL_NAMES.samTs])
  expect(scoped.getByText(t('settings.detection.profiles'))).toBeTruthy()

  expect(where(panel, 'ctd').textContent?.trim()).toBe(t('settings.models.where.localOnly'))
  expect(where(panel, 'rtSmall').textContent?.trim()).toBe(t('settings.models.where.localOnly'))
  expect(where(panel, 'rtFull').textContent?.trim()).toBe(t('settings.models.where.either'))
  expect(where(panel, 'samTs').textContent?.trim()).toBe(t('settings.models.where.either'))
  // The line is the checkbox's description, so a screen reader hears it too.
  expect(choice(scoped, 'ctd').getAttribute('aria-describedby')).toBe(where(panel, 'ctd').id)
})

it('shows the cloud GPU’s fixed four, checked and not editable, and keeps this computer’s own choice', async () => {
  setDetectorModels(['ctd', 'rtSmall'])
  setOcrRescue(false)
  setAnalysisTarget('samTs', 'cloud')
  setAnalysisTarget('rtFull', 'cloud')
  const { panel, scoped } = await openModels()

  for (const [id, checked] of /** @type {const} */ ([['ctd', true], ['rtFull', true], ['rtSmall', false], ['samTs', true]])) {
    expect(choice(scoped, id).checked, id).toBe(checked)
    expect(choice(scoped, id).disabled, id).toBe(true)
  }
  await openFiltering(scoped)
  const reader = /** @type {HTMLInputElement} */ (panel.querySelector('#settings-ocr-rescue'))
  expect(reader.checked).toBe(true)
  expect(reader.disabled).toBe(true)

  expect(where(panel, 'rtFull').textContent?.trim()).toBe(t('settings.models.where.onCloud'))
  expect(where(panel, 'samTs').textContent?.trim()).toBe(t('settings.models.where.onCloud'))
  expect(where(panel, 'ctd').textContent?.trim()).toBe(t('settings.models.where.cloudLocal'))
  expect(where(panel, 'rtSmall').textContent?.trim()).toBe(t('settings.models.where.cloudUnused'))
  // One note under the four says why, and each box is described by it.
  const group = /** @type {HTMLElement} */ (panel.querySelector('[data-settings-anchor="detection"]'))
  const notes = group.querySelectorAll(':scope > [data-cloud-combo]')
  expect(notes).toHaveLength(1)
  expect(notes[0].textContent?.trim()).toBe(t('pipelines.cloudCombo'))
  expect(choice(scoped, 'ctd').getAttribute('aria-describedby')?.split(' ')).toContain(notes[0].id)

  // A click changes nothing, and the stored choice waits for This computer.
  await fireEvent.click(choice(scoped, 'samTs'))
  expect(session.detectorModels).toEqual(['ctd', 'rtSmall'])
  expect(session.ocrRescue).toBe(false)
})

it('says nothing about the cloud combination while detection stays on this computer', async () => {
  setDetectorModels(['ctd', 'samTs'])
  const { panel, scoped } = await openModels()
  expect(panel.querySelectorAll('[data-cloud-combo]')).toHaveLength(0)
  expect(choice(scoped, 'rtFull').checked).toBe(false)
  expect(choice(scoped, 'rtFull').disabled).toBe(false)
  await openFiltering(scoped)
  expect(/** @type {HTMLInputElement} */ (panel.querySelector('#settings-ocr-rescue')).disabled).toBe(false)
})

it('clears Small when Full is chosen, and Full when Small is', async () => {
  setDetectorModels(['ctd', 'rtSmall'])
  const { scoped } = await openModels()

  await fireEvent.click(choice(scoped, 'rtFull'))
  expect(session.detectorModels).toEqual(['ctd', 'rtFull'])
  expect(choice(scoped, 'rtSmall').checked).toBe(false)
  expect(choice(scoped, 'rtFull').checked).toBe(true)

  await fireEvent.click(choice(scoped, 'rtSmall'))
  expect(session.detectorModels).toEqual(['ctd', 'rtSmall'])
  expect(choice(scoped, 'rtFull').checked).toBe(false)

  // CTD and the lettering mask combine with either.
  await fireEvent.click(choice(scoped, 'samTs'))
  expect(session.detectorModels).toEqual(['ctd', 'rtSmall', 'samTs'])
})

it('will not clear the last model selected', async () => {
  setDetectorModels(['samTs'])
  const { scoped } = await openModels()
  expect(choice(scoped, 'samTs').disabled).toBe(true)
  expect(choice(scoped, 'ctd').disabled).toBe(false)
  await fireEvent.click(choice(scoped, 'ctd'))
  await waitFor(() => expect(choice(scoped, 'samTs').disabled).toBe(false))
})
