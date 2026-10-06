/**
 * The tool shell, mounted: the pill every drawing tool uses, and the Text
 * cleanup panel.
 *
 * `tools.test.js` asserts the *specs* - every parameter is in a group, the
 * order is unchanged, a hidden one takes no group with it, and which choices
 * carry icons - and the rendering of them is this file's: which control a
 * parameter draws, whether it is on the bar or behind the Adjustments button,
 * whether pressing it writes the parameter, and how the shell changes shape
 * between the two.
 *
 * The `.dom` in the name is the whole of the environment change. The repo's
 * vitest runs `environment: 'node'` and stays there: a DOM per test file costs
 * a second of start-up, and
 * every other test in `src/lib` is arithmetic that does not need one. This file
 * asks for a document - and for Svelte's browser build, which is the half a
 * pragma cannot ask for - because the thing under test *is* the document.
 *
 * The three stores are stubbed rather than driven. `session`, `capabilities`
 * and `editor` are the bar's whole input, and a stub of each is what lets a
 * spec be mounted with every engine available and nothing blocked - which is
 * the case where the control a parameter draws is decided by the parameter and
 * not by a missing download.
 */

import { describe, expect, it, afterEach, vi } from 'vitest'
import { render, cleanup, fireEvent, screen, within } from '@testing-library/svelte'
import { tick } from 'svelte'
import { PANEL_WIDTH, TOOL_SPECS, activeParams, toolSpec } from './tools.js'
import { clampPosition, clampSize, TOOL_BOTTOM_RESERVE } from '../model/windows.js'
import { t } from '../i18n/index.js'
import { setBackend } from '../api/backend.js'
import { resetCloudOffer } from '../state/cloudtargets.svelte.js'
import { model as pipelineModel } from '../model/pipelines.js'

import tauriConf from '../../../src-tauri/tauri.conf.json'

/** The smallest window the application allows, from its own configuration. */
const MIN_WINDOW = {
  width: Number(tauriConf.app.windows[0].minWidth),
  height: Number(tauriConf.app.windows[0].minHeight),
}

/**
 * The stubs, hoisted so `vi.mock`'s factories can close over them.
 *
 * `session` and `capabilities` are plain objects: nothing in them mutates
 * while a component is mounted. `editor` is the **real** store - the module's
 * own `$state` proxy, with its functions replaced by spies - because the tests
 * that switch tool under a mounted bar need the switch to re-render it, and a
 * plain object would change without the bar noticing.
 */
const { editor } = await vi.importActual('../state/editor.svelte.js')

const stores = vi.hoisted(() => ({
  session: {
    cloudAllowed: true,
    /** @type {'local'|'cloud'} */
    cleanTarget: 'local',
    cleanLocalFirst: false,
    analysisTargets: { rtFull: 'local', samTs: 'local' },
    /** @type {string[]} */
    detectorModels: ['ctd', 'rtSmall'],
    /** @type {'legacy_gate'|'all_text'|undefined} */
    textPolicy: undefined,
    /** @type {Record<string, {x: number, y: number, w: number, h: number|null, open: boolean, fold: boolean}>} */
    windows: { tool: { x: 16, y: 62, w: 560, h: null, open: true, fold: false } },
    stacking: { tool: 1 },
  },
  capabilities: {
    /** @type {Record<string, boolean>} */
    engines: {},
    sidecar: true,
    /** @type {string|null} */
    sidecarReasonKey: null,
    runtime: true,
    autoClean: true,
  },
  /** @type {string[]|undefined} What the run is missing here, when a test says. */
  missing: undefined,
  /** @type {string[]|undefined} What a cloud run would miss here (CTD, the reader), when a test says. */
  cloudMissing: undefined,
  cloud: { checked: true, configured: true, readiness: null },
  openCloudSettings: vi.fn(),
  /** @type {Array<{value: string, choice: any}>|null} Every profile's cloud entry, when a test lists them. */
  cloudEntries: null,
  pickCloudChoice: vi.fn(async () => true),
  setToolParam: vi.fn(),
  startRun: vi.fn(),
  cancelRun: vi.fn(),
  setWindowBox: vi.fn(),
  setWindowOpen: vi.fn(),
  commitWindows: vi.fn(),
  setTextPolicy: vi.fn(),
  setCleanLocalFirst: vi.fn(),
}))

stores.editor = editor

vi.mock('../state/editor.svelte.js', () => ({
  editor,
  setToolParam: stores.setToolParam,
  startRun: stores.startRun,
  claimRunStart: () => ({}),
  releaseRunStart: () => {},
  cancelRun: stores.cancelRun,
  currentPage: () => ({ id: 'p1', number: 1 }),
  runningPage: () => null,
}))

vi.mock('../state/session.svelte.js', () => ({
  session: stores.session,
  setWindowBox: stores.setWindowBox,
  commitWindows: stores.commitWindows,
  raiseWindow: vi.fn(),
  foldWindow: vi.fn(),
  setWindowOpen: stores.setWindowOpen,
  resetWindowBox: vi.fn(),
  setAnalysisTarget: (stage, target) => { stores.session.analysisTargets[stage] = target },
  setCleanTarget: (target) => { stores.session.cleanTarget = target },
  setTextPolicy: stores.setTextPolicy,
  setCleanLocalFirst: stores.setCleanLocalFirst,
}))

vi.mock('../state/capabilities.svelte.js', () => ({
  capabilities: stores.capabilities,
  currentWorkflowAvailable: () => stores.capabilities.autoClean,
  // Asked with `{analysisTargets}` it answers for Detect on set the other way.
  currentWorkflowMissing: (/** @type {any} */ overrides) => overrides?.analysisTargets
    ? stores.cloudMissing ?? []
    : stores.missing ?? (stores.capabilities.autoClean ? [] : null),
}))

vi.mock('../state/cloud.svelte.js', () => ({
  cloud: stores.cloud,
  cloudUsable: () => stores.session.cloudAllowed && stores.cloud.configured,
  cloudCleanAvailable: () => true,
  openCloudSettings: stores.openCloudSettings,
  refreshCloudReadiness: vi.fn(async () => null),
  cloudEntries: () => stores.cloudEntries,
  pickCloudChoice: stores.pickCloudChoice,
}))

// Imported after the mocks so the component picks the stubs up.
const { default: ToolBar } = await import('./ToolBar.svelte')

/**
 * The values a spec starts on: what the bar would read out of
 * `editor.toolParams` on a fresh install. Written from the spec rather than
 * copied, so a parameter added to a tool is covered by this file without it
 * being edited - and so the bar's own reconciling effect finds nothing to
 * correct and calls the setter zero times before a test does.
 *
 * @param {import('./tools.js').ToolSpec} spec
 * @returns {Record<string, unknown>}
 */
function startingValues(spec) {
  /** @type {Record<string, unknown>} */
  const values = {}
  for (const param of spec.params) {
    if (param.kind === 'range') values[param.key] = param.min
    else if (param.kind === 'choice') values[param.key] = param.options[0].value
    else values[param.key] = param.default ?? '#000000'
  }
  return values
}

/**
 * Mount the bar with a tool selected.
 *
 * @param {import('./tools.js').ToolSpec} spec
 * @param {Record<string, unknown>} [overrides] - parameters this mount starts on
 *   instead of the spec's own opening values
 */
function mountTool(spec, overrides) {
  stores.editor.tool = spec.id
  stores.editor.toolParams = { [spec.id]: { ...startingValues(spec), ...overrides } }
  const view = render(ToolBar)
  stores.setToolParam.mockClear()
  return { view, values: stores.editor.toolParams[spec.id] }
}

/** A choice the bar draws as icon cells rather than as a dropdown. */
const iconChoice = (param) => param.options.every((option) => option.icon)

/** Everything but `size` lives behind the Adjustments button, and so does the hex field. */
const onBar = (param) => param.kind !== 'range' || param.key === 'size'

/** The tools drawn as a pill, and the one drawn as a panel. */
const PILL_SPECS = TOOL_SPECS.filter((spec) => spec.shell === 'pill')
const textCleanup = toolSpec('autoClean')

/** @param {import('@testing-library/svelte').RenderResult} view */
function shell(view) {
  return /** @type {HTMLElement} */ (view.container.querySelector('section'))
}

/**
 * Open the panel's collapsed group of engine picks.
 *
 * @param {import('@testing-library/svelte').RenderResult} view
 */
async function openAdvanced(view) {
  await fireEvent.click(view.getByRole('button', { name: new RegExp(t('tools.action.advanced')) }))
}

/** @param {import('@testing-library/svelte').RenderResult} view @param {'detect'|'clean'} key */
const place = (view, key) => /** @type {HTMLSelectElement|null} */ (view.container.querySelector(`[data-run-place="${key}"] select`))

/** @param {import('@testing-library/svelte').RenderResult} view */
const runAction = (view) => /** @type {HTMLButtonElement} */ (view.container.querySelector('[data-run-action]'))

/** @param {import('@testing-library/svelte').RenderResult} view */
function openAdjustments(view) {
  const button = view.getByLabelText(t('tools.action.adjustments'))
  return fireEvent.click(button)
}

/**
 * The element a parameter draws, wherever it drew it. The accessible name is
 * what finds it - which is also an assertion that every control has one.
 *
 * @param {import('@testing-library/svelte').RenderResult} view
 * @param {any} param
 * @param {Record<string, unknown>} values
 */
function controlFor(view, param, values) {
  if (param.kind === 'range' || param.kind === 'color') {
    return view.getByLabelText(t(param.labelKey))
  }
  if (iconChoice(param)) return view.getByLabelText(t(param.labelKey))
  // A dropdown's trigger is named for the parameter *and* the value it holds.
  const current = param.options.find((option) => option.value === values[param.key])
  return view.getByLabelText(
    t('tools.label.choice', { labelKey: param.labelKey, value: t(current.labelKey) }),
  )
}

afterEach(() => {
  cleanup()
  vi.clearAllMocks()
})

/**
 * The fill colour is read by a clean on this computer, and paints a *saved*
 * Solid pick as well as a fresh one (`run.rs#clean_stored`), so it is drawn
 * wherever such a clean happens, whatever the rows say now, and nowhere else:
 * detection does not read it, and neither does the cloud GPU.
 */
describe('Text cleanup solid fill color', () => {
  afterEach(() => {
    stores.session.cleanTarget = 'local'
  })

  it('shows one shared color picker wherever this computer cleans, Solid picked or not', async () => {
    for (const overrides of [
      { bubbleEngine: 'solid', outsideEngine: 'lama' },
      { bubbleEngine: 'lama', outsideEngine: 'solid' },
      { bubbleEngine: 'lama', outsideEngine: 'lama' },
    ]) {
      const { view } = mountTool(textCleanup, overrides)
      await openAdvanced(view)
      expect(view.getAllByLabelText(t('tools.param.bubbleColor'))).toHaveLength(1)
      cleanup()
    }
  })

  it('draws no color where nothing on this computer cleans', async () => {
    const { view: detect } = mountTool(textCleanup, { step: 'detect', bubbleEngine: 'solid' })
    expect(detect.queryByRole('button', { name: new RegExp(t('tools.action.advanced')) })).toBeNull()
    expect(detect.queryByLabelText(t('tools.param.bubbleColor'))).toBeNull()
    cleanup()

    stores.session.cleanTarget = 'cloud'
    for (const step of ['auto', 'clean']) {
      const { view } = mountTool(textCleanup, { step, bubbleEngine: 'solid' })
      const more = view.queryByRole('button', { name: new RegExp(t('tools.action.advanced')) })
      if (more) await fireEvent.click(more)
      expect(view.queryByLabelText(t('tools.param.bubbleColor')), step).toBeNull()
      cleanup()
    }
  })
})

describe('every pill tool draws the control each of its parameters asks for', () => {
  for (const spec of PILL_SPECS) {
    it(`draws ${spec.id}'s parameters, each with a name of its own`, async () => {
      const { view, values } = mountTool(spec)
      const params = activeParams(spec, values)
      // Nothing is behind a press until the press is made.
      if (params.some((param) => !onBar(param))) await openAdjustments(view)

      for (const param of params) {
        const control = controlFor(view, param, values)

        if (param.kind === 'range') {
          expect(control.tagName, param.key).toBe('INPUT')
          expect(control.getAttribute('type'), param.key).toBe('range')
        } else if (param.kind === 'color') {
          expect(control.tagName, param.key).toBe('BUTTON')
          expect(control.getAttribute('aria-haspopup'), param.key).toBe('dialog')
        } else if (iconChoice(param)) {
          expect(control.getAttribute('role'), param.key).toBe('radiogroup')
          const cells = [...control.querySelectorAll('[role="radio"]')]
          expect(cells).toHaveLength(param.options.length)
          // A glyph and no words: the label is the cell's accessible name.
          for (const [index, cell] of cells.entries()) {
            expect(cell.querySelector('svg'), param.key).not.toBeNull()
            expect(cell.textContent?.trim()).toBe('')
            expect(cell.getAttribute('aria-label')).toBe(t(param.options[index].labelKey))
          }
        } else {
          expect(control.tagName, param.key).toBe('BUTTON')
          expect(control.getAttribute('aria-haspopup'), param.key).toBe('menu')
          // The short label is what is drawn, where the spec asked for one.
          expect(control.textContent).toContain(t(param.shortKey ?? param.labelKey))
        }
      }
    })
  }
})

describe('a change on any pill control writes that parameter, once', () => {
  for (const spec of PILL_SPECS) {
    it(`writes every parameter of ${spec.id}`, async () => {
      const { view, values } = mountTool(spec)
      const params = activeParams(spec, values)
      if (params.some((param) => !onBar(param))) await openAdjustments(view)

      for (const param of params) {
        stores.setToolParam.mockClear()
        const control = controlFor(view, param, values)

        if (param.kind === 'range') {
          const next = Math.min(param.max, param.min + param.step)
          await fireEvent.input(control, { target: { value: String(next) } })
          expect(stores.setToolParam).toHaveBeenCalledTimes(1)
          expect(stores.setToolParam).toHaveBeenCalledWith(spec.id, param.key, next)
        } else if (param.kind === 'color') {
          await fireEvent.click(control)
          const hexInput = within(control.closest('.anchor')).getByLabelText(t('tools.color.hex'))
          await fireEvent.input(hexInput, { target: { value: '#123456' } })
          expect(stores.setToolParam).toHaveBeenCalledWith(spec.id, param.key, '#123456')
        } else {
          const next = param.options[1]?.value
          expect(next, `${param.key} has a second option to move to`).toBeTruthy()
          if (iconChoice(param)) {
            const cells = [...control.querySelectorAll('[role="radio"]')]
            await fireEvent.click(cells[1])
          } else {
            await fireEvent.click(control)
            const items = [...view.container.querySelectorAll('[role="menuitemradio"]')]
            expect(items.length).toBe(param.options.length)
            await fireEvent.click(items[1])
          }
          expect(stores.setToolParam).toHaveBeenCalledTimes(1)
          expect(stores.setToolParam).toHaveBeenCalledWith(spec.id, param.key, next)
        }
      }
    })
  }
})

describe('the hex field in ColorPicker says when what is in it is not a colour', () => {
  const shapes = toolSpec('shapes')

  /** @returns {Promise<HTMLInputElement>} */
  async function hexField(view) {
    const swatch = view.getByLabelText(t('tools.param.color'))
    await fireEvent.click(swatch)
    return /** @type {HTMLInputElement} */ (view.getByLabelText(t('tools.color.hex')))
  }

  it('starts clean, with the committed colour in it', async () => {
    const { view } = mountTool(shapes)
    const field = await hexField(view)
    expect(field.value).toBe('#ffffff')
    expect(field.getAttribute('aria-invalid')).toBeNull()
  })

  it('marks the field as invalid while the text is not a colour', async () => {
    const { view } = mountTool(shapes)
    const field = await hexField(view)
    await fireEvent.input(field, { target: { value: '#ab' } })

    expect(field.getAttribute('aria-invalid')).toBe('true')
    // Nothing was written: the swatch still holds the colour in force.
    expect(stores.setToolParam).not.toHaveBeenCalled()
  })

  it('clears invalid state and commits when typing valid 6-digit hex', async () => {
    const { view } = mountTool(shapes)
    const field = await hexField(view)
    await fireEvent.input(field, { target: { value: '#ab' } })
    await fireEvent.input(field, { target: { value: '#123456' } })

    expect(field.getAttribute('aria-invalid')).toBeNull()
    expect(stores.setToolParam).toHaveBeenCalledWith('shapes', 'color', '#123456')
  })

  it('commits 3-digit shorthand expanding to 6 digits on Enter or blur', async () => {
    const { view } = mountTool(shapes)
    const field = await hexField(view)
    await fireEvent.input(field, { target: { value: '#fff' } })
    stores.setToolParam.mockClear()

    await fireEvent.keyDown(field, { key: 'Enter' })
    expect(stores.setToolParam).toHaveBeenCalledWith('shapes', 'color', '#ffffff')
  })

  it('reverts invalid hex text to the committed colour on blur without writing', async () => {
    const { view } = mountTool(shapes)
    const field = await hexField(view)
    stores.setToolParam.mockClear()
    await fireEvent.input(field, { target: { value: '#ab' } })
    expect(field.getAttribute('aria-invalid')).toBe('true')

    await fireEvent.blur(field)
    expect(stores.setToolParam).not.toHaveBeenCalled()
    expect(field.value).toBe('#ffffff')
    expect(field.getAttribute('aria-invalid')).toBeNull()
  })
})

describe('the eyedropper is drawn only where the platform has one', () => {
  const shapes = toolSpec('shapes')

  it('is absent without `EyeDropper`, which is every platform but Chromium', async () => {
    const { view } = mountTool(shapes)
    const swatch = view.getByLabelText(t('tools.param.color'))
    await fireEvent.click(swatch)
    expect(view.queryByLabelText(t('tools.action.eyedropper'))).toBeNull()
  })

  it('writes the colour it picked when the platform has one', async () => {
    const opened = vi.fn(async () => ({ sRGBHex: '#0a0b0c' }))
    // @ts-expect-error - the global exists on Chromium alone
    globalThis.EyeDropper = class {
      open() {
        return opened()
      }
    }
    try {
      const { view } = mountTool(shapes)
      const swatch = view.getByLabelText(t('tools.param.color'))
      await fireEvent.click(swatch)
      await fireEvent.click(view.getByLabelText(t('tools.action.eyedropper')))
      expect(opened).toHaveBeenCalledTimes(1)
      expect(stores.setToolParam).toHaveBeenCalledWith('shapes', 'color', '#0a0b0c')
    } finally {
      // @ts-expect-error - putting the platform back as it was
      delete globalThis.EyeDropper
    }
  })
})

/**
 * A blocked option is shown disabled **and says why**, because a `title` on a
 * disabled control reaches nobody - browsers fire no hover events on one. The
 * reason has two homes, and which one it takes is decided by the control: a
 * dropdown holds it inside itself, under its items; an icon group has no
 * inside, so it sits beside the group on the bar.
 */
describe('a gated option says what it is costing the user', () => {
  it('keeps a selected missing LaMa visible and disabled instead of selecting Cloud', async () => {
    stores.capabilities.engines = { lama: false, flux: false }
    stores.session.cloudAllowed = false
    try {
      const spec = toolSpec('aiMaskBrush')
      const { view, values } = mountTool(spec)
      const trigger = controlFor(view, spec.params[0], values)
      expect(trigger.textContent).toContain('LaMa')
      await fireEvent.click(trigger)
      expect(/** @type {HTMLButtonElement} */ (view.getByRole('menuitemradio', { name: 'LaMa Manga' })).disabled).toBe(true)
      expect(view.getByText(t('tools.option.engineMissing'))).not.toBeNull()
      expect(stores.setToolParam).not.toHaveBeenCalledWith('aiMaskBrush', 'engine', 'cloud')
    } finally {
      stores.capabilities.engines = {}
      stores.session.cloudAllowed = true
    }
  })

  it('names verified cloud and local FLUX models in the same brush list', async () => {
    stores.session.fluxModel = 'flux2-klein-4b'
    stores.cloud.readiness = {
      target: { type: 'modal', profile_id: 'studio' },
      profile: { modelId: 'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32' },
    }
    try {
      const spec = toolSpec('aiMaskBrush')
      const { view, values } = mountTool(spec)
      await fireEvent.click(controlFor(view, spec.params[0], values))
      expect(view.getByRole('menuitemradio', { name: 'FLUX.2 Klein 4B · Local' })).not.toBeNull()
      expect(view.getByRole('menuitemradio', { name: '☁ FLUX.2 Klein 9B · Cloud' })).not.toBeNull()
    } finally {
      delete stores.session.fluxModel
      delete stores.cloud.readiness
    }
  })

  it('lists every cloud profile by its model, and a pick switches the default before storing Cloud', async () => {
    stores.cloudEntries = [
      { value: 'cloud', choice: { provider: 'modal', profileId: 'klein', name: 'Studio', modelId: 'flux2-klein-9b', selected: true } },
      { value: 'cloud@modal:qwen', choice: { provider: 'modal', profileId: 'qwen', name: 'Edits', modelId: 'qwen-image-edit-2511', selected: false } },
    ]
    stores.setToolParam.mockClear()
    stores.pickCloudChoice.mockClear()
    try {
      const spec = toolSpec('aiMaskBrush')
      const { view, values } = mountTool(spec)
      await fireEvent.click(controlFor(view, spec.params[0], values))
      expect(view.getByRole('menuitemradio', { name: '☁ FLUX.2 Klein 9B · Studio' })).not.toBeNull()
      await fireEvent.click(view.getByRole('menuitemradio', { name: '☁ Qwen-Image-Edit-2511 · Edits' }))
      await vi.waitFor(() => expect(stores.setToolParam).toHaveBeenCalledWith(spec.id, spec.params[0].key, 'cloud'))
      expect(stores.pickCloudChoice).toHaveBeenCalledWith('cloud@modal:qwen')
      expect(stores.setToolParam).not.toHaveBeenCalledWith(spec.id, spec.params[0].key, 'cloud@modal:qwen')
    } finally {
      stores.cloudEntries = null
    }
  })

  it('disables the cloud model inside the brush engine list when cloud is off', async () => {
    stores.session.cloudAllowed = false
    try {
      const spec = toolSpec('aiMaskBrush')
      const { view, values } = mountTool(spec)
      await fireEvent.click(controlFor(view, spec.params[0], values))
      expect(/** @type {HTMLButtonElement} */ (view.getByRole('menuitemradio', { name: /☁ Cloud/ })).disabled).toBe(true)
      expect(view.getByText(t('editor.state.cloudBlocked'))).not.toBeNull()
    } finally {
      stores.session.cloudAllowed = true
    }
  })

  it('disables the cloud model while no endpoint is set up, and explains why', async () => {
    stores.cloud.configured = false
    try {
      const spec = toolSpec('aiMaskBrush')
      const { view, values } = mountTool(spec)
      await fireEvent.click(controlFor(view, spec.params[0], values))
      expect(/** @type {HTMLButtonElement} */ (view.getByRole('menuitemradio', { name: /☁ Cloud/ })).disabled).toBe(true)
      expect(view.getByText(t('tools.option.engineCloudNotReady'))).not.toBeNull()
    } finally {
      stores.cloud.configured = true
    }
  })

  it('prints a withheld rung’s reason inside the dropdown that withheld it', async () => {
    stores.capabilities.sidecar = false
    stores.capabilities.sidecarReasonKey = 'decline.reason.sidecarMachine'
    stores.capabilities.engines = { flux: false }
    try {
      const spec = toolSpec('aiMaskBrush')
      const { view, values } = mountTool(spec)
      const trigger = controlFor(view, spec.params[0], values)
      await fireEvent.click(trigger)

      // Beside the list rather than in it - a `role="menu"` may hold menu
      // items and separators and nothing else - and the list is described by
      // it, which is what carries the sentence to a screen reader now that it
      // is not a child that could be read on the way past.
      const menu = /** @type {HTMLElement} */ (view.container.querySelector('[role="menu"]'))
      expect(menu.querySelector('p')).toBeNull()
      const note = document.getElementById(String(menu.getAttribute('aria-describedby')))
      expect(note?.textContent?.trim()).toBe(t('decline.reason.sidecarMachine'))
      expect(note?.parentElement?.contains(menu)).toBe(true)
      // The trigger carries it too, for a pointer that never opens the menu.
      expect(trigger.getAttribute('title')).toBe(t('decline.reason.sidecarMachine'))
    } finally {
      stores.capabilities.sidecar = true
      stores.capabilities.sidecarReasonKey = null
      stores.capabilities.engines = {}
    }
  })
})

describe('the run action', () => {
  it('keeps an all-text run blocked while its selected model is missing', () => {
    stores.session.textPolicy = 'all_text'
    stores.capabilities.autoClean = false
    try {
      const { view } = mountTool(toolSpec('autoClean'))
      expect(view.getByText(t('editor.state.modelsMissing'))).not.toBeNull()
    } finally {
      delete stores.session.textPolicy
      stores.capabilities.autoClean = true
    }
  })

  it('is Text cleanup’s alone', () => {
    const { view } = mountTool(toolSpec('autoClean'))
    expect(view.getByText(t('editor.action.run.autoPage'))).not.toBeNull()

    cleanup()
    const { view: brush } = mountTool(toolSpec('brush'))
    expect(brush.queryByText(t('editor.action.run.autoPage'))).toBeNull()
  })

  // LaMa on disk says nothing about a detector, so the note names the one
  // that is missing, and where it has a cloud version offers that instead.
  it('names the missing detection model, and offers the cloud GPU where it runs there', async () => {
    const writeSettings = vi.fn(async () => ({}))
    const listRemoteAnalysisCapabilities = vi.fn(async () => ({
      capabilities: [{ capability: 'text_mask_sam_ts@1' }, { capability: 'text_regions_rt@1' }],
    }))
    setBackend(/** @type {any} */ ({ writeSettings, listRemoteAnalysisCapabilities }))
    stores.session.detectorModels = ['ctd', 'samTs']
    stores.missing = ['samTs']
    const models = String(pipelineModel('samTs')?.product)
    try {
      const { view: offline } = mountTool(toolSpec('autoClean'))
      expect(offline.getByText(t('editor.state.detectModelsMissing', { models }))).not.toBeNull()
      expect(offline.queryByRole('button', { name: t('tools.target.useCloud') })).toBeNull()
      cleanup()

      stores.cloud.readiness = /** @type {any} */ ({ target: { type: 'modal', profile_id: 'p1' } })
      const { view } = mountTool(toolSpec('autoClean'))
      const useCloud = await vi.waitFor(() => view.getByRole('button', { name: t('tools.target.useCloud') }))
      expect(view.getByText(t('editor.state.detectModelsCloud', { models }))).not.toBeNull()
      await fireEvent.click(useCloud)
      await tick()
      expect(writeSettings).toHaveBeenCalledWith({ analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } })
      cleanup()

      // A cloud run needs CTD and the text reader here: missing those, it is
      // not offered as the way out.
      stores.session.analysisTargets = { rtFull: 'local', samTs: 'local' }
      stores.cloudMissing = ['hayaiOcr']
      const { view: short } = mountTool(toolSpec('autoClean'))
      await tick()
      expect(short.getByText(t('editor.state.detectModelsMissing', { models }))).not.toBeNull()
      expect(short.queryByRole('button', { name: t('tools.target.useCloud') })).toBeNull()
    } finally {
      stores.missing = undefined
      stores.cloudMissing = undefined
      stores.cloud.readiness = null
      stores.session.detectorModels = ['ctd', 'rtSmall']
      stores.session.analysisTargets = { rtFull: 'local', samTs: 'local' }
    }
  })

  it('is disabled with a reason when the models are not downloaded', () => {
    stores.capabilities.autoClean = false
    try {
      const { view } = mountTool(toolSpec('autoClean'))
      const action = /** @type {HTMLButtonElement} */ (
        view.getByText(t('editor.action.run.autoPage')).closest('button')
      )
      expect(action.disabled).toBe(true)
      const note = view.getByText(t('editor.state.modelsMissing'))
      // And the note is the button's description - a control refused with the
      // reason only on screen is a control refused for no stated reason.
      expect(action.getAttribute('aria-describedby')).toBe(note.id)
      expect(note.id).not.toBe('')
    } finally {
      stores.capabilities.autoClean = true
    }
  })

  it('starts the run on the scope the parameter holds', async () => {
    const spec = toolSpec('autoClean')
    const { view } = mountTool(spec)

    await fireEvent.click(view.getByText(t('editor.action.run.autoPage')))
    expect(stores.startRun).toHaveBeenCalledTimes(1)
    expect(stores.startRun).toHaveBeenCalledWith('page', expect.objectContaining({ mode: 'auto' }))

    // The label follows the scope, and so does what the press starts. The
    // stub stores are plain objects, so the second scope is a second mount
    // rather than a write the bar would react to.
    cleanup()
    stores.startRun.mockClear()
    const { view: project } = mountTool(spec, { scope: 'project' })
    await fireEvent.click(project.getByText(t('editor.action.run.autoProject')))
    expect(stores.startRun).toHaveBeenCalledTimes(1)
    expect(stores.startRun).toHaveBeenCalledWith('project', expect.objectContaining({ mode: 'auto' }))
    expect(stores.cancelRun).not.toHaveBeenCalled()
  })

  it('cancels rather than starts while a run is going, and says which page', async () => {
    stores.editor.run.active = true
    try {
      const { view } = mountTool(toolSpec('autoClean'))
      // `runningPage()` is null in these stubs, so the line names the page on
      // screen - which is the honest answer for the moment before the first
      // `page-started`.
      const live = /** @type {HTMLElement} */ (
        view.container.querySelector('[aria-live="polite"]')
      )
      expect(live.textContent).toBe(t('editor.status.cleaning', { page: 1 }))

      await fireEvent.click(view.getByText(t('editor.action.cancelRun')))
      expect(stores.cancelRun).toHaveBeenCalledTimes(1)
      expect(stores.startRun).not.toHaveBeenCalled()
    } finally {
      stores.editor.run.active = false
    }
  })

  it('says it is detecting, not cleaning, while a Detect run is going', () => {
    stores.editor.run.active = true
    stores.editor.run.mode = 'detect'
    try {
      const { view } = mountTool(toolSpec('autoClean'), { step: 'detect' })
      const live = /** @type {HTMLElement} */ (view.container.querySelector('[aria-live="polite"]'))
      expect(live.textContent).toBe(t('editor.status.detecting', { page: 1 }))
      expect(live.textContent).not.toBe(t('editor.status.cleaning', { page: 1 }))
    } finally {
      stores.editor.run.active = false
      stores.editor.run.mode = 'auto'
    }
  })

  // `run-finished` can land between the last frame the user saw, which said
  // Cancel, and their press on it. That press must not start a new run.
  it('does not start a run from a press that lands just after one finished', async () => {
    let now = 10_000
    const clock = vi.spyOn(performance, 'now').mockImplementation(() => now)
    stores.editor.run.active = true
    try {
      const { view } = mountTool(toolSpec('autoClean'))
      expect(runAction(view).textContent?.trim()).toBe(t('editor.action.cancelRun'))
      stores.editor.run.active = false
      await tick()
      now += 120
      await fireEvent.click(runAction(view))
      expect(stores.startRun).not.toHaveBeenCalled()
      expect(stores.cancelRun).not.toHaveBeenCalled()
      // A deliberate press, once the change has been on screen a moment, runs.
      now += 600
      await fireEvent.click(runAction(view))
      expect(stores.startRun).toHaveBeenCalledTimes(1)
    } finally {
      stores.editor.run.active = false
      clock.mockRestore()
    }
  })

  // The line beside the button is a live region, so it is on the page before
  // there is anything in it - an `aria-live` element added at the moment its
  // text arrives announces nothing.
  it('keeps an empty live region while nothing is running', () => {
    const { view } = mountTool(toolSpec('autoClean'))
    const live = view.container.querySelector('[aria-live="polite"]')
    expect(live).not.toBeNull()
    expect(live?.textContent).toBe('')
  })

  it('names the step on the button and runs it as the mode', async () => {
    const spec = toolSpec('autoClean')
    const { view } = mountTool(spec, { step: 'detect', scope: 'chapter' })
    await fireEvent.click(view.getByText(t('editor.action.run.detectChapter')))
    expect(stores.startRun).toHaveBeenCalledWith('chapter', expect.objectContaining({ mode: 'detect' }))

    cleanup()
    stores.startRun.mockClear()
    const { view: clean } = mountTool(spec, { step: 'clean' })
    await fireEvent.click(clean.getByText(t('editor.action.run.cleanPage')))
    expect(stores.startRun).toHaveBeenCalledWith('page', expect.objectContaining({ mode: 'clean' }))
  })
})

/**
 * The Text cleanup panel is the single home of a run's choices: mode, scope,
 * Detect on, Clean on, which text it takes, what it does outside bubbles, and
 * the two per-run engine picks. Detect on and Clean on are the two settings
 * the run reads (`analysisTargets`, `cleanTarget`), with Cloud GPU disabled
 * for the reason Settings gives; the text policy is `session.textPolicy`. A
 * row that cannot change what the chosen mode does is absent.
 */
describe('the Text cleanup panel', () => {
  /** @param {HTMLSelectElement} select */
  const cloudOption = (select) => /** @type {HTMLOptionElement} */ (select.querySelector('option[value="cloud"]'))
  /** @param {HTMLSelectElement} select */
  const noteOf = (select) => document.getElementById(String(select.getAttribute('aria-describedby')))?.textContent?.trim()
  /** @param {import('@testing-library/svelte').RenderResult} view @param {string} name */
  const radios = (view, name) => [...view.getByRole('radiogroup', { name }).querySelectorAll('[role="radio"]')]
    .map((cell) => cell.textContent?.trim())

  afterEach(() => {
    stores.session.cloudAllowed = true
    stores.session.cleanTarget = 'local'
    stores.session.analysisTargets = { rtFull: 'local', samTs: 'local' }
    stores.session.detectorModels = ['ctd', 'rtSmall']
    stores.session.textPolicy = undefined
    resetCloudOffer()
    setBackend(null)
  })

  it('is a panel named Text cleanup, and every other tool is a pill', () => {
    const { view } = mountTool(textCleanup)
    expect(shell(view).dataset.shell).toBe('panel')
    expect(shell(view).classList.contains('panel')).toBe(true)
    expect(view.getByRole('region', { name: 'Text cleanup' })).toBe(shell(view))
    expect(view.getByRole('heading', { name: 'Text cleanup' })).not.toBeNull()
    expect(view.getByLabelText(t('editor.window.move', { windowName: 'Text cleanup' })).tagName).toBe('BUTTON')
    cleanup()
    for (const spec of PILL_SPECS) {
      const { view: pill } = mountTool(spec)
      expect(shell(pill).dataset.shell, spec.id).toBe('pill')
      cleanup()
    }
  })

  it('reaches every run choice: mode, scope, both places, the text taken and the outside-bubble opt-in', async () => {
    const { view } = mountTool(textCleanup)
    expect(radios(view, t('tools.param.step'))).toEqual(['Detect', 'Clean', 'Detect & clean'])
    expect(radios(view, t('tools.param.scope'))).toEqual(['Page', 'Chapter', 'Project'])
    expect(view.getByLabelText(t('tools.param.detectOn')).tagName).toBe('SELECT')
    expect(view.getByLabelText(t('tools.param.cleanOn')).tagName).toBe('SELECT')
    expect(radios(view, t('tools.param.textPolicy'))).toEqual([t('tools.option.policyLegacy'), t('tools.option.policyAll')])
    expect(radios(view, t('tools.param.outsideBubbles'))).toEqual([t('tools.option.outsideReview'), t('tools.option.outsideClean')])
    // The engine picks are collapsed, and say what they hold without opening.
    const more = view.getByRole('button', { name: new RegExp(t('tools.action.advanced')) })
    expect(more.getAttribute('aria-expanded')).toBe('false')
    expect(more.textContent).toContain(t('masks.engineChoice.fill'))
    expect(view.queryByLabelText(t('tools.param.bubbleText'))).toBeNull()
    await openAdvanced(view)
    expect(more.getAttribute('aria-expanded')).toBe('true')
    expect(view.getByLabelText(t('tools.param.bubbleText')).tagName).toBe('SELECT')
    expect(view.getByLabelText(t('tools.param.outsideText')).tagName).toBe('SELECT')
  })

  it('writes each tool parameter from its row, once', async () => {
    const { view } = mountTool(textCleanup)
    await openAdvanced(view)
    /** @type {Array<[string, () => Promise<unknown>, unknown]>} */
    const writes = [
      ['step', () => fireEvent.click(view.getByRole('radio', { name: 'Detect' })), 'detect'],
      ['scope', () => fireEvent.click(view.getByRole('radio', { name: 'Chapter' })), 'chapter'],
      ['outsideBubbles', () => fireEvent.click(view.getByRole('radio', { name: t('tools.option.outsideClean') })), 'clean'],
      ['bubbleEngine', () => fireEvent.change(view.getByLabelText(t('tools.param.bubbleText')), { target: { value: 'lama' } }), 'lama'],
      ['outsideEngine', () => fireEvent.change(view.getByLabelText(t('tools.param.outsideText')), { target: { value: 'solid' } }), 'solid'],
    ]
    for (const [key, act, value] of writes) {
      stores.setToolParam.mockClear()
      await act()
      expect(stores.setToolParam, key).toHaveBeenCalledTimes(1)
      expect(stores.setToolParam).toHaveBeenCalledWith('autoClean', key, value)
    }
  })

  it('sets the text policy where the run reads it, and drops the outside-bubble row under all text', async () => {
    const { view } = mountTool(textCleanup)
    await fireEvent.click(view.getByRole('radio', { name: t('tools.option.policyAll') }))
    expect(stores.setTextPolicy).toHaveBeenCalledWith('all_text')
    cleanup()

    stores.session.textPolicy = 'all_text'
    const { view: all } = mountTool(textCleanup)
    expect(all.getByRole('radio', { name: t('tools.option.policyAll') }).getAttribute('aria-checked')).toBe('true')
    // The all-text run cleans outside bubbles regardless (`run.rs`).
    expect(all.queryByRole('radiogroup', { name: t('tools.param.outsideBubbles') })).toBeNull()
  })

  it('draws only the rows the mode reads', async () => {
    const { view: detect } = mountTool(textCleanup, { step: 'detect' })
    expect(place(detect, 'detect')).not.toBeNull()
    expect(place(detect, 'clean')).toBeNull()
    // Which text is cleaned is Detect & clean's question: Detect alone finds
    // all text for the review.
    expect(detect.queryByRole('radiogroup', { name: t('tools.param.textPolicy') })).toBeNull()
    expect(detect.queryByRole('radiogroup', { name: t('tools.param.outsideBubbles') })).toBeNull()
    cleanup()

    const { view: auto } = mountTool(textCleanup, { step: 'auto' })
    expect(place(auto, 'detect')).not.toBeNull()
    expect(place(auto, 'clean')).not.toBeNull()
    expect(auto.queryByRole('radiogroup', { name: t('tools.param.textPolicy') })).not.toBeNull()
    expect(auto.queryByRole('radiogroup', { name: t('tools.param.outsideBubbles') })).not.toBeNull()
    cleanup()

    const { view: clean } = mountTool(textCleanup, { step: 'clean' })
    expect(place(clean, 'detect')).toBeNull()
    expect(place(clean, 'clean')).not.toBeNull()
    // Clean detects nothing, so neither the text taken nor the opt-in apply.
    expect(clean.queryByRole('radiogroup', { name: t('tools.param.textPolicy') })).toBeNull()
    expect(clean.queryByRole('radiogroup', { name: t('tools.param.outsideBubbles') })).toBeNull()
  })

  /** @param {import('@testing-library/svelte').RenderResult} view */
  const picksNote = (view) => view.container.querySelector('[data-picks-note]')

  // A clean starts every region from the panel's picks, including regions
  // detected earlier, and a cloud clean saves them onto its regions first
  // (`cloud_clean.rs#PrepareRequest`). A pick the panel hides must not be one
  // that decides.
  it('shows the engine picks wherever the run cleans, the cloud cleaning or not', async () => {
    for (const [step, cleanTarget, localFirst, noteKey] of /** @type {const} */ ([
      ['clean', 'local', false, 'tools.picks.local'],
      ['auto', 'local', false, 'tools.picks.local'],
      // The cloud clean is strictly remote unless the mixed box is ticked,
      // and then Fill and Solid are tried here first.
      ['clean', 'cloud', false, 'tools.picks.cloud'],
      ['auto', 'cloud', false, 'tools.picks.cloud'],
      ['auto', 'cloud', true, 'tools.picks.mixed'],
    ])) {
      stores.session.cleanTarget = cleanTarget
      const { view } = mountTool(textCleanup, { step, bubbleEngine: 'lama', localFirst })
      const more = view.getByRole('button', { name: new RegExp(t('tools.action.advanced')) })
      // Collapsed, the summary still says what they hold.
      expect(more.textContent, `${step}/${cleanTarget}`).toContain('LaMa')
      await fireEvent.click(more)
      const bubble = /** @type {HTMLSelectElement} */ (view.getByLabelText(t('tools.param.bubbleText')))
      expect(bubble.value).toBe('lama')
      expect(view.getByLabelText(t('tools.param.outsideText')).tagName).toBe('SELECT')
      // And says which regions they reach, as the selects' description.
      const note = /** @type {HTMLElement} */ (picksNote(view))
      expect(note.textContent, `${step}/${cleanTarget}`).toBe(t(noteKey))
      expect(bubble.getAttribute('aria-describedby')?.split(' ')).toContain(note.id)
      cleanup()
    }
  })

  // Detect cleans nothing, so no pick of the panel's decides anything there.
  // A clean starts every region from the panel's picks (`run.rs`), so they
  // are drawn for Clean.
  it('draws no engine pick for Detect, which cleans nothing', () => {
    for (const cleanTarget of /** @type {const} */ (['local', 'cloud'])) {
      stores.session.cleanTarget = cleanTarget
      const { view } = mountTool(textCleanup, { step: 'detect' })
      expect(view.queryByRole('button', { name: new RegExp(t('tools.action.advanced')) }), cleanTarget).toBeNull()
      expect(view.queryByLabelText(t('tools.param.bubbleText'))).toBeNull()
      expect(view.queryByLabelText(t('tools.param.outsideText'))).toBeNull()
      expect(view.queryByLabelText(t('tools.param.bubbleColor'))).toBeNull()
      expect(picksNote(view)).toBeNull()
      cleanup()
    }
  })

  // Mixed execution is an explicit choice (`cloudrun.js#cleanLocalFirst`):
  // offered only while the clean goes to the cloud GPU, off unless ticked.
  it('offers the mixed choice only while the cloud GPU cleans, off by default', async () => {
    /** @param {import('@testing-library/svelte').RenderResult} view */
    const box = (view) => /** @type {HTMLInputElement|null} */ (
      view.container.querySelector('[data-run-row="localFirst"] input[type="checkbox"]'))
    for (const [cleanTarget, step] of /** @type {const} */ ([['local', 'auto'], ['local', 'clean'], ['cloud', 'detect']])) {
      stores.session.cleanTarget = cleanTarget
      const { view } = mountTool(textCleanup, { step })
      expect(box(view), `${cleanTarget}/${step}`).toBeNull()
      cleanup()
    }

    stores.session.cleanTarget = 'cloud'
    for (const step of ['auto', 'clean']) {
      const { view } = mountTool(textCleanup, { step })
      const checkbox = /** @type {HTMLInputElement} */ (box(view))
      expect(checkbox.checked, step).toBe(false)
      expect(view.getByLabelText(t('cloud.clean.mixed.label'))).toBe(checkbox)
      expect(document.getElementById(String(checkbox.getAttribute('aria-describedby')))?.textContent)
        .toBe(t('tools.target.mixedHint'))
      await fireEvent.click(checkbox)
      expect(stores.setToolParam).toHaveBeenCalledWith('autoClean', 'localFirst', true)
      expect(stores.setCleanLocalFirst).toHaveBeenCalledWith(true)
      cleanup()
      stores.setToolParam.mockClear()
    }

    const { view: ticked } = mountTool(textCleanup, { localFirst: true })
    expect(/** @type {HTMLInputElement} */ (box(ticked)).checked).toBe(true)
    await fireEvent.click(/** @type {HTMLInputElement} */ (box(ticked)))
    expect(stores.setToolParam).toHaveBeenCalledWith('autoClean', 'localFirst', false)
    expect(stores.setCleanLocalFirst).toHaveBeenCalledWith(false)
  })

  it('offers Project only while the whole run stays on this computer', async () => {
    stores.session.cleanTarget = 'cloud'
    const { view } = mountTool(textCleanup, { scope: 'project' })
    expect(radios(view, t('tools.param.scope'))).toEqual(['Page', 'Chapter'])
    expect(view.getByRole('radio', { name: 'Chapter' }).getAttribute('aria-checked')).toBe('true')
    expect(runAction(view).textContent).toContain(t('editor.action.run.autoChapter'))
    await fireEvent.click(runAction(view))
    expect(stores.startRun).not.toHaveBeenCalledWith('project', expect.anything())
  })

  // A Project kept aside while a cloud half is on would come back unasked the
  // moment that half went away, and the next run would cover every chapter.
  // What the panel shows is what is stored: Chapter, written back at once.
  it('writes a Project it cannot run back as the Chapter it shows', () => {
    stores.session.cleanTarget = 'cloud'
    stores.editor.tool = 'autoClean'
    stores.editor.toolParams = { autoClean: { ...startingValues(textCleanup), scope: 'project' } }
    render(ToolBar)
    expect(stores.setToolParam).toHaveBeenCalledTimes(1)
    expect(stores.setToolParam).toHaveBeenCalledWith('autoClean', 'scope', 'chapter')
    cleanup()
    stores.setToolParam.mockClear()

    // A Project that can run is the user's pick, and stays.
    stores.session.cleanTarget = 'local'
    render(ToolBar)
    expect(stores.setToolParam).not.toHaveBeenCalled()
  })

  it('names the operation and the scope on the one action', () => {
    const { view } = mountTool(textCleanup, { step: 'auto', scope: 'chapter' })
    expect(runAction(view).textContent?.trim()).toBe('Detect & clean chapter')
  })

  it('disables Cloud GPU with the Settings reason while cloud engines are off', () => {
    stores.session.cloudAllowed = false
    const { view } = mountTool(textCleanup)
    const select = /** @type {HTMLSelectElement} */ (place(view, 'clean'))
    expect(cloudOption(select).disabled).toBe(true)
    expect(noteOf(select)).toBe(t('settings.detection.runOn.off'))
    expect(view.getAllByRole('button', { name: t('tools.option.engineCloudSettings') }).length).toBeGreaterThan(0)
  })

  it('says once, under both places, that cloud engines are off', () => {
    stores.session.cloudAllowed = false
    const { view } = mountTool(textCleanup)
    const detect = /** @type {HTMLSelectElement} */ (place(view, 'detect'))
    const clean = /** @type {HTMLSelectElement} */ (place(view, 'clean'))
    expect(cloudOption(detect).disabled).toBe(true)
    expect(cloudOption(clean).disabled).toBe(true)
    expect(detect.getAttribute('aria-describedby')).toBe(clean.getAttribute('aria-describedby'))
    expect(noteOf(detect)).toBe(t('settings.detection.runOn.off'))
    expect(view.getAllByText(t('settings.detection.runOn.off'))).toHaveLength(1)
    expect(view.getAllByRole('button', { name: t('tools.option.engineCloudSettings') })).toHaveLength(1)
  })

  // A cloud run uses its own fixed combination, so Detect on: Cloud GPU is
  // offered whatever this computer selected, Small and CTD included.
  it('offers Detect on: Cloud GPU whatever this computer selected', async () => {
    const listRemoteAnalysisCapabilities = vi.fn(async () => ({
      capabilities: [{ capability: 'text_mask_sam_ts@1' }, { capability: 'text_regions_rt@1' }],
    }))
    setBackend(/** @type {any} */ ({ writeSettings: vi.fn(async () => ({})), listRemoteAnalysisCapabilities }))
    stores.session.detectorModels = ['ctd', 'rtSmall']
    stores.cloud.readiness = /** @type {any} */ ({ target: { type: 'modal', profile_id: 'p1' } })
    try {
      const { view } = mountTool(textCleanup)
      const select = /** @type {HTMLSelectElement} */ (place(view, 'detect'))
      await vi.waitFor(() => expect(cloudOption(select).disabled).toBe(false))
      expect(select.getAttribute('aria-describedby')).toBeNull()
    } finally {
      stores.cloud.readiness = null
    }
  })

  it('says a stranded cloud choice will not run, and keeps it selectable', () => {
    stores.session.cloudAllowed = false
    stores.session.cleanTarget = 'cloud'
    const { view } = mountTool(textCleanup)
    const stranded = `${t('tools.target.stranded')} ${t('settings.detection.runOn.off')}`
    const select = /** @type {HTMLSelectElement} */ (place(view, 'clean'))
    expect(select.value).toBe('cloud')
    expect(cloudOption(select).disabled).toBe(false)
    expect(noteOf(select)).toBe(stranded)
  })

  // Detect on: Cloud GPU uses all four models whatever this computer
  // selected (`pipelines.js#runDetection`); the row says so as its
  // description, and nothing about the selection blocks the run.
  it('says which models a cloud run uses while Detect on is Cloud GPU, and never blocks on the selection', async () => {
    stores.session.detectorModels = ['ctd', 'rtSmall', 'samTs']
    stores.session.analysisTargets = { rtFull: 'cloud', samTs: 'cloud' }
    const { view } = mountTool(textCleanup)
    const select = /** @type {HTMLSelectElement} */ (place(view, 'detect'))
    expect(select.value).toBe('cloud')
    const note = /** @type {HTMLElement} */ (view.container.querySelector('[data-cloud-combo]'))
    expect(note.textContent?.trim()).toBe(t('pipelines.cloudCombo'))
    expect(select.getAttribute('aria-describedby')?.split(' ')).toContain(note.id)
    expect(runAction(view).disabled).toBe(false)
    cleanup()

    // Nothing to say while detection stays here, or in Clean, which detects nothing.
    for (const [models, targets, step] of /** @type {const} */ ([
      [['ctd', 'samTs'], { rtFull: 'local', samTs: 'local' }, 'auto'],
      [['ctd', 'samTs'], { rtFull: 'cloud', samTs: 'cloud' }, 'clean'],
    ])) {
      stores.session.detectorModels = [...models]
      stores.session.analysisTargets = { ...targets }
      const { view: quiet } = mountTool(textCleanup, { step })
      expect(quiet.container.querySelector('[data-cloud-combo]'), `${models}/${step}`).toBeNull()
      cleanup()
    }
  })

  it('saves Clean on where the native run reads it', async () => {
    const writeSettings = vi.fn(async () => ({}))
    setBackend(/** @type {any} */ ({ writeSettings }))
    const { view } = mountTool(textCleanup)
    const select = /** @type {HTMLSelectElement} */ (place(view, 'clean'))
    await fireEvent.change(select, { target: { value: 'cloud' } })
    await tick()
    expect(writeSettings).toHaveBeenCalledWith({ cleanTarget: 'cloud' })
    expect(stores.session.cleanTarget).toBe('cloud')
  })

  it('lets a long-strip run clean on the cloud', () => {
    stores.session.cleanTarget = 'cloud'
    const previous = stores.editor.project
    stores.editor.project = /** @type {any} */ ({ mode: 'longstrip' })
    try {
      const { view } = mountTool(textCleanup)
      expect(runAction(view).getAttribute('aria-describedby')).toBeNull()
    } finally {
      stores.editor.project = previous
    }
  })

  it('saves Detect on for both cloud-capable models at once, once the endpoint offers them', async () => {
    const writeSettings = vi.fn(async () => ({}))
    const listRemoteAnalysisCapabilities = vi.fn(async () => ({
      capabilities: [{ capability: 'text_mask_sam_ts@1' }, { capability: 'text_regions_rt@1' }],
    }))
    setBackend(/** @type {any} */ ({ writeSettings, listRemoteAnalysisCapabilities }))
    stores.session.detectorModels = ['ctd', 'samTs']
    stores.cloud.readiness = /** @type {any} */ ({ target: { type: 'modal', profile_id: 'p1' } })
    try {
      const { view } = mountTool(textCleanup)
      const select = /** @type {HTMLSelectElement} */ (place(view, 'detect'))
      await vi.waitFor(() => expect(cloudOption(select).disabled).toBe(false))
      expect(listRemoteAnalysisCapabilities).toHaveBeenCalledWith({ provider: 'modal', profileId: 'p1' })
      await fireEvent.change(select, { target: { value: 'cloud' } })
      await tick()
      expect(writeSettings).toHaveBeenCalledWith({ analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } })
    } finally {
      stores.cloud.readiness = null
    }
  })

  it('shows progress in front of Cancel while running, on the button that was pressed', () => {
    stores.editor.run.active = true
    stores.editor.run.queued = 12
    stores.editor.run.pagesDone = 3
    try {
      const { view } = mountTool(textCleanup)
      const bar = view.getByRole('progressbar', { name: t('tools.label.progress') })
      expect(bar.getAttribute('aria-valuenow')).toBe('3')
      expect(bar.getAttribute('aria-valuemax')).toBe('12')
      expect(bar.getAttribute('aria-valuetext')).toBe(t('editor.run.progress', { done: 3, total: 12 }))
      expect(runAction(view).textContent?.trim()).toBe(t('editor.action.cancelRun'))
      // The progress comes first, then the one action.
      expect(bar.compareDocumentPosition(runAction(view)) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
    } finally {
      stores.editor.run.active = false
      stores.editor.run.queued = 0
      stores.editor.run.pagesDone = 0
    }
  })
})

/**
 * Keyboard use of the panel: Tab walks the controls in reading order and ends
 * on the one action; Enter or Space on that action is the browser's own
 * activation of a native button; Escape closes a popover inside the panel and
 * leaves the panel alone.
 */
describe('the Text cleanup panel from the keyboard', () => {
  /** Everything Tab stops on, in document order, the way a browser walks it. */
  function tabStops(/** @type {HTMLElement} */ root) {
    return /** @type {HTMLElement[]} */ ([...root.querySelectorAll('button, select, input, [tabindex]')])
      .filter((node) => !(/** @type {any} */ (node).disabled) && node.getAttribute('tabindex') !== '-1')
  }

  it('tabs through header, choices and engines, and ends on the action', async () => {
    const { view } = mountTool(textCleanup)
    await openAdvanced(view)
    const names = tabStops(shell(view)).map((node) => node.getAttribute('aria-label') ?? node.id ?? '')
    const stops = tabStops(shell(view))
    expect(names[0]).toBe(t('editor.window.move', { windowName: 'Text cleanup' }))
    expect(names[1]).toBe(t('editor.window.close', { windowName: 'Text cleanup' }))
    // One stop per radio group (roving tabindex), then the selects, in rows.
    const kinds = stops.slice(2).map((node) => node.getAttribute('role') ?? node.tagName.toLowerCase())
    // Mode, Scope, Detect on and the Cloud settings press under its reason
    // (the endpoint's offer is still being read here), Clean on, Text,
    // Outside bubbles, Mask padding and its Apply, the models toggle, the two
    // engines, the fill colour's swatch, then the action.
    expect(kinds).toEqual(['radio', 'radio', 'select', 'button', 'select', 'radio', 'radio', 'input', 'button', 'button', 'select', 'select', 'button', 'button'])
    expect(stops.at(-1)).toBe(runAction(view))
    // The name takes focus from script, never from Tab.
    expect(view.getByRole('heading', { name: 'Text cleanup' }).getAttribute('tabindex')).toBe('-1')
  })

  it('runs from its one action, a native button that Enter and Space activate', async () => {
    const { view } = mountTool(textCleanup, { scope: 'chapter' })
    const action = runAction(view)
    expect(action.tagName).toBe('BUTTON')
    expect(action.getAttribute('type')).toBe('button')
    action.focus()
    expect(document.activeElement).toBe(action)
    // jsdom synthesises no click from a key; the browser does, for a button.
    await fireEvent.click(action)
    expect(stores.startRun).toHaveBeenCalledWith('chapter', expect.objectContaining({ mode: 'auto' }))
  })

  it('closes a popover inside the panel on Escape, and nothing else', async () => {
    const { view } = mountTool(textCleanup, { bubbleEngine: 'solid' })
    await openAdvanced(view)
    const swatch = view.getByLabelText(t('tools.param.bubbleColor'))
    await fireEvent.click(swatch)
    const dialog = /** @type {HTMLElement} */ (shell(view).querySelector('[role="dialog"]'))
    expect(dialog).not.toBeNull()
    const inside = /** @type {HTMLElement} */ (dialog.querySelector('input, button'))
    const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })
    inside.dispatchEvent(escape)
    await tick()
    expect(shell(view).querySelector('[role="dialog"]')).toBeNull()
    expect(stores.setWindowOpen).not.toHaveBeenCalled()
    expect(view.getByRole('radiogroup', { name: t('tools.param.step') })).not.toBeNull()
  })
})

describe('the shell is as large as its contents', () => {
  it('sets no size of its own, on any tool', () => {
    for (const spec of TOOL_SPECS) {
      const { view } = mountTool(spec)
      const bar = /** @type {HTMLElement} */ (view.container.querySelector('section'))
      expect(bar.style.width, spec.id).toBe('')
      expect(bar.style.height, spec.id).toBe('')
      // Position and stacking it does own; the width is the element's.
      expect(bar.style.left).toBe('16px')
      expect(bar.style.top).toBe('62px')
      cleanup()
    }
  })

  /**
   * jsdom ships no `ResizeObserver`, so the bar's measuring effect does
   * nothing there unless one is put in front of it. This is the smallest one
   * that is still the contract: it hands the callback back and the test fires
   * it with a box.
   */
  it('writes the border box it measured back through `setWindowBox`', async () => {
    /** @type {((entries: unknown[]) => void)|null} */
    let notify = null
    const global = /** @type {any} */ (globalThis)
    const previous = global.ResizeObserver
    global.ResizeObserver = class {
      constructor(/** @type {(entries: unknown[]) => void} */ callback) {
        notify = callback
      }
      observe() {}
      disconnect() {}
    }
    try {
      mountTool(toolSpec('autoClean'))
      expect(notify).not.toBeNull()

      // The **border** box: `contentRect` stops inside the shell's padding,
      // and the numbers `clampPosition` places the shell by are the whole box.
      // Both dimensions, from where the user put it.
      notify?.([{ borderBoxSize: [{ inlineSize: 412.4, blockSize: 380.6 }], contentRect: { width: 400 } }])
      expect(stores.setWindowBox).toHaveBeenCalledWith('tool', { x: 16, y: 62, w: 412, h: 381 })

      // Older WebKit hands the box over as one object rather than an array.
      stores.setWindowBox.mockClear()
      notify?.([{ borderBoxSize: { inlineSize: 300, blockSize: 44 }, contentRect: { width: 290 } }])
      expect(stores.setWindowBox).toHaveBeenCalledWith('tool', { x: 16, y: 62, w: 300, h: 44 })

      // A bar being torn down measures 0, and that is not a width to place
      // anything by.
      stores.setWindowBox.mockClear()
      notify?.([{ borderBoxSize: [{ inlineSize: 0 }], contentRect: { width: 0 } }])
      expect(stores.setWindowBox).not.toHaveBeenCalled()
    } finally {
      global.ResizeObserver = previous
    }
  })
})

/**
 * `Tab` ends a menu, because a menu is one control. It must **not** end the
 * Adjustments popover, which is a panel of several: the first Tab off the
 * first slider used to take the whole panel with it. What ends the popover is
 * focus leaving the anchor.
 */
describe('the Adjustments popover keeps Tab to itself', () => {
  /**
   * The open panel and the controls in it. Shapes is the tool with the fullest
   * one - two sliders and the hex field - which is what makes a Tab *between*
   * controls a thing that can happen at all.
   *
   * jsdom gives every element a null `offsetParent`, so `ui/focus.js#focusable`
   * finds nothing and the panel's opening focus never moves; the first control
   * is focused here instead, which is the state a browser would already be in.
   *
   * @param {import('@testing-library/svelte').RenderResult} view
   */
  async function openPanel(view) {
    await openAdjustments(view)
    const panel = /** @type {HTMLElement} */ (view.container.querySelector('[role="dialog"]'))
    const controls = /** @type {HTMLElement[]} */ ([
      ...panel.querySelectorAll('input, button, select, textarea'),
    ])
    expect(controls.length, 'the panel holds more than one control').toBeGreaterThan(1)
    controls[0].focus()
    return controls
  }

  it('stays open while Tab moves between its own controls', async () => {
    const { view } = mountTool(toolSpec('shapes'))
    const controls = await openPanel(view)

    await fireEvent.keyDown(controls[0], { key: 'Tab' })
    // jsdom moves no focus of its own on Tab, so the move the browser would
    // make is made here - and it is that move, not the key, that the popover
    // reads.
    controls[1].focus()
    await fireEvent.focusOut(controls[0], { relatedTarget: controls[1] })

    expect(view.container.querySelector('[role="dialog"]')).not.toBeNull()
    expect(document.activeElement).toBe(controls[1])
  })

  it('closes when focus lands outside the anchor', async () => {
    const { view } = mountTool(toolSpec('shapes'))
    const controls = await openPanel(view)

    const outside = document.createElement('button')
    document.body.append(outside)
    try {
      outside.focus()
      await fireEvent.focusOut(controls.at(-1) ?? controls[0], { relatedTarget: outside })
      expect(view.container.querySelector('[role="dialog"]')).toBeNull()
    } finally {
      outside.remove()
    }
  })

  // WebKit - the engine under the shipped Tauri application - does not focus
  // a button or a range input on click, so a press on the panel's own slider
  // thumb, label or padding blurs the focused control with a null
  // `relatedTarget`. That is not focus leaving for somewhere else, and the
  // panel must stay; a press that really is outside is the outside
  // pointerdown listener's to answer, exactly as it is for a menu.
  it('stays open when focus is dropped rather than moved elsewhere', async () => {
    const { view } = mountTool(toolSpec('shapes'))
    const controls = await openPanel(view)

    await fireEvent.focusOut(controls[0], { relatedTarget: null })

    expect(view.container.querySelector('[role="dialog"]')).not.toBeNull()
  })
})

describe('tool switching resets transient state and closes overlays', () => {
  it('closes an open dropdown menu when the tool changes', async () => {
    const { view } = mountTool(toolSpec('shapes'), { mode: 'lama' })
    const dropdown = view.getByLabelText(
      t('tools.label.choice', {
        labelKey: 'tools.param.mode',
        value: t('masks.engineChoice.lama'),
      }),
    )
    await fireEvent.click(dropdown)
    expect(view.container.querySelector('[role="menu"]')).not.toBeNull()

    stores.editor.tool = 'autoClean'
    stores.editor.toolParams = {
      ...stores.editor.toolParams,
      autoClean: startingValues(textCleanup),
    }
    await tick()

    expect(view.container.querySelector('[role="menu"]')).toBeNull()
  })

  it('closes an open Adjustments popover when the tool changes', async () => {
    const { view } = mountTool(toolSpec('shapes'))
    await openAdjustments(view)
    expect(view.container.querySelector('[role="dialog"]')).not.toBeNull()

    stores.editor.tool = 'autoClean'
    stores.editor.toolParams = {
      ...stores.editor.toolParams,
      autoClean: startingValues(toolSpec('autoClean')),
    }
    await tick()

    expect(view.container.querySelector('[role="dialog"]')).toBeNull()
  })

  it('does not leak half-typed hex text across tool switches', async () => {
    const { view } = mountTool(toolSpec('shapes'))
    const swatch = view.getByLabelText(t('tools.param.color'))
    await fireEvent.click(swatch)
    const field = /** @type {HTMLInputElement} */ (view.getByLabelText(t('tools.color.hex')))
    await fireEvent.input(field, { target: { value: '#123456' } })
    expect(field.value).toBe('#123456')

    stores.editor.tool = 'brush'
    stores.editor.toolParams = {
      ...stores.editor.toolParams,
      brush: { ...startingValues(toolSpec('brush')), color: '#ff0000' },
    }
    await tick()

    const brushSwatch = view.getByLabelText(t('tools.param.color'))
    await fireEvent.click(brushSwatch)
    const brushField = /** @type {HTMLInputElement} */ (
      view.getByLabelText(t('tools.color.hex'))
    )
    expect(brushField.value).toBe('#ff0000')
  })
})

describe('the Brush tool in paint mode draws and writes color, opacity, and flow', () => {
  it('shows swatch on the bar and opacity/flow in Adjustments', async () => {
    const brush = toolSpec('brush')
    const { view } = mountTool(brush, { color: '#336699', opacity: 80, flow: 90 })

    const swatch = view.getByLabelText(t('tools.param.color'))
    expect(swatch).not.toBeNull()
    expect(swatch.tagName).toBe('BUTTON')

    await openAdjustments(view)

    const opacity = view.getByLabelText(t('tools.param.opacity'))
    expect(opacity).not.toBeNull()
    await fireEvent.input(opacity, { target: { value: '50' } })
    expect(stores.setToolParam).toHaveBeenCalledWith('brush', 'opacity', 50)

    const flow = view.getByLabelText(t('tools.param.flow'))
    expect(flow).not.toBeNull()
    await fireEvent.input(flow, { target: { value: '60' } })
    expect(stores.setToolParam).toHaveBeenCalledWith('brush', 'flow', 60)
  })
})

describe('toolbar gestures and window controls', () => {
  it('moves the bar with arrow keys on the grip and prevents default', async () => {
    const { view } = mountTool(toolSpec('autoClean'))
    const grip = view.getByLabelText(
      t('editor.window.move', { windowName: t('tools.name.autoClean') }),
    )

    const event = new KeyboardEvent('keydown', {
      key: 'ArrowRight',
      bubbles: true,
      cancelable: true,
    })
    const preventDefaultSpy = vi.spyOn(event, 'preventDefault')
    grip.dispatchEvent(event)

    expect(preventDefaultSpy).toHaveBeenCalled()
    expect(stores.setWindowBox).toHaveBeenCalledWith(
      'tool',
      expect.objectContaining({ x: expect.any(Number) }),
    )
    expect(stores.commitWindows).toHaveBeenCalled()
  })

  it('closes the tool bar when clicking the close button', async () => {
    const { view } = mountTool(toolSpec('autoClean'))
    const closeBtn = view.getByLabelText(
      t('editor.window.close', { windowName: t('tools.name.autoClean') }),
    )
    await fireEvent.click(closeBtn)
    expect(stores.setWindowOpen).toHaveBeenCalledWith('tool', false)
  })

  it('closes the Adjustments popover on Escape and stops propagation', async () => {
    const { view } = mountTool(toolSpec('shapes'))
    await openAdjustments(view)
    expect(view.container.querySelector('[role="dialog"]')).not.toBeNull()

    const panel = /** @type {HTMLElement} */ (view.container.querySelector('[role="dialog"]'))
    const slider = panel.querySelector('input')
    expect(slider).not.toBeNull()

    const escapeEvent = new KeyboardEvent('keydown', {
      key: 'Escape',
      bubbles: true,
      cancelable: true,
    })
    const stopPropagationSpy = vi.spyOn(escapeEvent, 'stopPropagation')
    slider?.dispatchEvent(escapeEvent)
    await tick()

    expect(stopPropagationSpy).toHaveBeenCalled()
    expect(view.container.querySelector('[role="dialog"]')).toBeNull()
  })
})

describe('sync effect handles edge cases safely', () => {
  it('does not write to store when choice options list is empty', async () => {
    stores.capabilities.engines = {
      fill: false,
      migan: false,
      lama: false,
      ldm: false,
      flux: false,
    }
    try {
      mountTool(toolSpec('aiMaskBrush'))
      await tick()
      expect(stores.setToolParam).not.toHaveBeenCalled()
    } finally {
      stores.capabilities.engines = {}
    }
  })
})

/**
 * The shell changes shape when the tool does: a pill for the drawing tools,
 * the panel for Text cleanup. jsdom lays nothing out, so each test here puts
 * sizes on the shell the way a browser would report them - by the shape it
 * is in - and asks what the shell did with them.
 */
describe('the shell between pill and panel', () => {
  const SIZES = { pill: { w: 312, h: 44 }, panel: { w: PANEL_WIDTH, h: 452 } }
  /** @type {Array<() => void>} */
  let restore = []

  /**
   * Report a browser's layout: the shell measures by its current shape, or by
   * the size pinned on it inline while it changes shape.
   */
  function layOut() {
    const proto = HTMLElement.prototype
    const width = Object.getOwnPropertyDescriptor(proto, 'offsetWidth')
    const height = Object.getOwnPropertyDescriptor(proto, 'offsetHeight')
    /** @param {HTMLElement} node @param {'w'|'h'} axis */
    const measure = (node, axis) => {
      if (node.tagName !== 'SECTION') return 0
      return SIZES[node.classList.contains('panel') ? 'panel' : 'pill'][axis]
    }
    Object.defineProperty(proto, 'offsetWidth', { configurable: true, get() { return measure(this, 'w') } })
    Object.defineProperty(proto, 'offsetHeight', { configurable: true, get() { return measure(this, 'h') } })
    restore.push(() => {
      if (width) Object.defineProperty(proto, 'offsetWidth', width)
      if (height) Object.defineProperty(proto, 'offsetHeight', height)
    })
  }

  /**
   * The Web Animations API, which jsdom does not have: every call recorded,
   * with the handle it returned, so a test can finish or inspect it.
   */
  function animations() {
    /** @type {Array<{node: HTMLElement, keyframes: any[], options: any, cancel: import('vitest').Mock, onfinish: (() => void)|null}>} */
    const calls = []
    const proto = /** @type {any} */ (HTMLElement.prototype)
    const previous = proto.animate
    proto.animate = function (/** @type {any[]} */ keyframes, /** @type {any} */ options) {
      const handle = { node: this, keyframes, options, cancel: vi.fn(), onfinish: null }
      calls.push(handle)
      return handle
    }
    restore.push(() => {
      if (previous) proto.animate = previous
      else delete proto.animate
    })
    return calls
  }

  /** @param {boolean} reduce */
  function motion(reduce) {
    const previous = globalThis.matchMedia
    globalThis.matchMedia = /** @type {any} */ ((/** @type {string} */ query) => ({
      matches: reduce && query.includes('prefers-reduced-motion: reduce'),
      media: query,
      addEventListener() {},
      removeEventListener() {},
    }))
    restore.push(() => { globalThis.matchMedia = previous })
  }

  /**
   * The session's own geometry, in a window of this size: the stub store
   * placed exactly as `setWindowBox` places it, so what the shell asked for
   * and where it ends up are both visible.
   *
   * @param {number} vw
   * @param {number} vh
   */
  function viewport(vw, vh) {
    const previous = { w: globalThis.innerWidth, h: globalThis.innerHeight }
    globalThis.innerWidth = vw
    globalThis.innerHeight = vh
    stores.setWindowBox.mockImplementation((/** @type {string} */ id, /** @type {any} */ patch) => {
      const win = stores.session.windows[id]
      const size = clampSize({ w: patch.w ?? win.w, h: patch.h === undefined ? win.h : patch.h }, vh, id)
      const position = clampPosition({ x: patch.x ?? win.x, y: patch.y ?? win.y, w: size.w, h: size.h }, vw, vh, id)
      Object.assign(win, position, size)
    })
    restore.push(() => {
      globalThis.innerWidth = previous.w
      globalThis.innerHeight = previous.h
      stores.setWindowBox.mockReset()
    })
  }

  /** @param {string} id */
  async function switchTo(id) {
    stores.editor.tool = id
    stores.editor.toolParams = { ...stores.editor.toolParams, [id]: startingValues(toolSpec(id)) }
    await tick()
  }

  const settle = () => new Promise((resolve) => setTimeout(resolve, 320))

  afterEach(() => {
    for (const undo of restore.reverse()) undo()
    restore = []
    stores.session.windows.tool = { x: 16, y: 62, w: 560, h: null, open: true, fold: false }
  })

  it('animates width, height, radius and place from the pill to the panel, then lets go', async () => {
    layOut()
    motion(false)
    const calls = animations()
    const { view } = mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    const bar = shell(view)
    expect(calls).toHaveLength(1)
    const [{ node, keyframes, options }] = calls
    expect(node).toBe(bar)
    // From the pill it was to the panel it measures, in both dimensions.
    expect(keyframes[0]).toMatchObject({ width: `${SIZES.pill.w}px`, height: `${SIZES.pill.h}px`, left: '16px', top: '62px' })
    expect(keyframes[1]).toMatchObject({ width: `${PANEL_WIDTH}px`, height: `${SIZES.panel.h}px`, left: '16px', top: '62px' })
    for (const frame of keyframes) expect(frame).toHaveProperty('borderRadius')
    // Brief: the app's slow duration, and never longer.
    expect(options.duration).toBeGreaterThan(0)
    expect(options.duration).toBeLessThanOrEqual(200)
    // Clipped while it grows, so the panel's rows do not spill past its edge.
    expect(bar.style.overflow).toBe('hidden')
    // Placed once, for the size it is growing to.
    expect(stores.setWindowBox).toHaveBeenCalledWith('tool', { x: 16, y: 62, w: PANEL_WIDTH, h: SIZES.panel.h })
    // Landed: nothing is left on the element, and it sizes itself again.
    calls[0].onfinish?.()
    expect(bar.style.overflow).toBe('')
    expect(bar.style.width).toBe('')
    expect(bar.style.height).toBe('')
  })

  it('lands on its own when no frame finishes the animation, as in a hidden window', async () => {
    layOut()
    motion(false)
    const calls = animations()
    const { view } = mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    expect(shell(view).style.overflow).toBe('hidden')
    await settle()
    expect(calls[0].cancel).toHaveBeenCalled()
    expect(shell(view).style.overflow).toBe('')
  })

  it('carries a switch made mid-animation on from where it was', async () => {
    layOut()
    motion(false)
    const calls = animations()
    mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    await switchTo('shapes')
    expect(calls).toHaveLength(2)
    expect(calls[0].cancel).toHaveBeenCalled()
    expect(calls[1].keyframes[1]).toMatchObject({ width: `${SIZES.pill.w}px`, height: `${SIZES.pill.h}px` })
  })

  // In the middle of a change of shape the store already holds the last
  // target and the element is short of it. A second switch runs on from the
  // frame on screen, not from the store.
  it('runs a second switch on from the frame on screen, not from the stored target', async () => {
    layOut()
    motion(false)
    viewport(MIN_WINDOW.width, MIN_WINDOW.height)
    const calls = animations()
    stores.session.windows.tool = { x: 640, y: 580, w: SIZES.pill.w, h: SIZES.pill.h, open: true, fold: false }
    const { view } = mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    expect(calls).toHaveLength(1)
    // The store is at the panel's place already; the frame is halfway there.
    expect(stores.session.windows.tool).toMatchObject({ x: MIN_WINDOW.width - PANEL_WIDTH, y: MIN_WINDOW.height - SIZES.panel.h - TOOL_BOTTOM_RESERVE })
    const bar = shell(view)
    const computed = globalThis.getComputedStyle
    globalThis.getComputedStyle = /** @type {any} */ ((/** @type {Element} */ node, /** @type {any} */ pseudo) => {
      const style = computed(node, pseudo)
      if (node !== bar) return style
      return new Proxy(style, {
        get: (target, key) => (key === 'left' ? '628px' : key === 'top' ? '382px' : Reflect.get(target, key)),
      })
    })
    restore.push(() => { globalThis.getComputedStyle = computed })
    await switchTo('shapes')
    expect(calls).toHaveLength(2)
    expect(calls[1].keyframes[0]).toMatchObject({ left: '628px', top: '382px' })
    expect(calls[1].keyframes[1]).toMatchObject({ left: '640px', top: '580px' })
  })

  // The grip holds the pointer capture of a drag it started, and it is part
  // of the content a switch replaces: the gesture has to end there, or it
  // stays open and every later drag is refused.
  it('ends a grip drag that a switch of tool cuts short, so the next drag moves', async () => {
    layOut()
    motion(true)
    const { view } = mountTool(toolSpec('brush'))
    const grip = view.getByLabelText(t('editor.window.move', { windowName: t('tools.name.brush') }))
    await fireEvent.pointerDown(grip, { button: 0, pointerId: 1, clientX: 30, clientY: 80 })
    await switchTo('autoClean')
    // Its release lands outside the shell, where nothing hears it.
    document.body.dispatchEvent(new PointerEvent('pointerup', { pointerId: 1, bubbles: true }))
    expect(stores.commitWindows).toHaveBeenCalled()
    stores.setWindowBox.mockClear()
    const header = view.getByLabelText(t('editor.window.move', { windowName: 'Text cleanup' }))
    await fireEvent.pointerDown(header, { button: 0, pointerId: 2, clientX: 30, clientY: 80 })
    await fireEvent.pointerMove(header, { pointerId: 2, clientX: 70, clientY: 100 })
    expect(stores.setWindowBox).toHaveBeenCalledWith('tool', { x: 56, y: 82 })
  })

  it('swaps instantly under reduced motion', async () => {
    layOut()
    motion(true)
    const calls = animations()
    const { view } = mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    const bar = shell(view)
    expect(bar.dataset.shell).toBe('panel')
    expect(calls).toHaveLength(0)
    expect(bar.style.width).toBe('')
    expect(bar.style.height).toBe('')
    expect(bar.style.overflow).toBe('')
    // Placed for its new size at once, not after an animation.
    expect(stores.setWindowBox).toHaveBeenCalledWith('tool', { x: 16, y: 62, w: PANEL_WIDTH, h: SIZES.panel.h })
    await switchTo('brush')
    expect(bar.dataset.shell).toBe('pill')
    expect(bar.style.width).toBe('')
    expect(stores.setWindowBox).toHaveBeenLastCalledWith('tool', { x: 16, y: 62, w: SIZES.pill.w, h: SIZES.pill.h })
  })

  it('moves focus the old content took with it to the new header', async () => {
    layOut()
    motion(true)
    const { view } = mountTool(toolSpec('brush'))
    view.getByLabelText(t('editor.window.move', { windowName: t('tools.name.brush') })).focus()
    await switchTo('autoClean')
    expect(document.activeElement).toBe(view.getByRole('heading', { name: 'Text cleanup' }))
  })

  it('leaves focus alone when it was never inside', async () => {
    layOut()
    motion(true)
    const outside = document.createElement('button')
    document.body.append(outside)
    try {
      mountTool(toolSpec('brush'))
      outside.focus()
      await switchTo('autoClean')
      expect(document.activeElement).toBe(outside)
    } finally {
      outside.remove()
    }
  })

  it('keeps focus on the control that changed the panel', async () => {
    layOut()
    motion(true)
    const { view } = mountTool(textCleanup)
    const clean = view.getByRole('radio', { name: 'Clean' })
    clean.focus()
    stores.editor.toolParams = { ...stores.editor.toolParams, autoClean: { ...stores.editor.toolParams.autoClean, step: 'clean' } }
    await tick()
    expect(document.activeElement).toBe(clean)
  })

  it('fits the smallest window, held whole on screen from the bottom-right corner', async () => {
    layOut()
    motion(true)
    viewport(MIN_WINDOW.width, MIN_WINDOW.height)
    // The plan's width, and inside the smallest window the app allows.
    expect(PANEL_WIDTH).toBeGreaterThanOrEqual(360)
    expect(PANEL_WIDTH).toBeLessThanOrEqual(400)
    stores.session.windows.tool = { x: 600, y: 580, w: SIZES.pill.w, h: SIZES.pill.h, open: true, fold: false }
    const { view } = mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    const bar = shell(view)
    // Where the session put it. (The stub store is a plain object, so the
    // element does not follow it here; in the application it is `$state`.)
    const { x: left, y: top } = stores.session.windows.tool
    expect(left).toBeGreaterThanOrEqual(0)
    expect(top).toBeGreaterThanOrEqual(4)
    expect(left + PANEL_WIDTH).toBeLessThanOrEqual(MIN_WINDOW.width)
    expect(top + SIZES.panel.h).toBeLessThanOrEqual(MIN_WINDOW.height)
    // The panel's width is capped by the window as well as set.
    expect(bar.style.getPropertyValue('--panel-width')).toBe(`${PANEL_WIDTH}px`)
  })

  it('puts the pill back where the user had it once the panel no longer needs the room', async () => {
    layOut()
    motion(true)
    viewport(MIN_WINDOW.width, MIN_WINDOW.height)
    stores.session.windows.tool = { x: 640, y: 580, w: SIZES.pill.w, h: SIZES.pill.h, open: true, fold: false }
    mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    // Pushed up and left, as far as the panel needs and no further.
    expect(stores.session.windows.tool).toMatchObject({ x: MIN_WINDOW.width - PANEL_WIDTH, y: MIN_WINDOW.height - SIZES.panel.h - TOOL_BOTTOM_RESERVE })
    await switchTo('brush')
    expect(stores.session.windows.tool).toMatchObject({ x: 640, y: 580 })
  })

  it('keeps the chosen anchor through a smaller viewport and restores it when room returns', async () => {
    layOut()
    motion(true)
    viewport(1000, 900)
    const global = /** @type {any} */ (globalThis)
    const previous = global.ResizeObserver
    global.ResizeObserver = class {
      observe() {}
      disconnect() {}
    }
    restore.push(() => { global.ResizeObserver = previous })
    stores.session.windows.tool = { x: 640, y: 580, w: SIZES.pill.w, h: SIZES.pill.h, open: true, fold: false }
    mountTool(toolSpec('brush'))
    await switchTo('autoClean')
    expect(stores.session.windows.tool.y).toBe(900 - SIZES.panel.h - TOOL_BOTTOM_RESERVE)

    viewport(1000, 640)
    stores.session.windows.tool.y = 640 - SIZES.panel.h - TOOL_BOTTOM_RESERVE
    globalThis.dispatchEvent(new Event('resize'))
    expect(stores.setWindowBox).toHaveBeenLastCalledWith('tool', { x: 640, y: 580, w: SIZES.panel.w, h: SIZES.panel.h })

    viewport(1000, 900)
    globalThis.dispatchEvent(new Event('resize'))
    expect(stores.session.windows.tool.y).toBe(900 - SIZES.panel.h - TOOL_BOTTOM_RESERVE)
  })

  it('takes a drag as the new place to grow from', async () => {
    layOut()
    motion(true)
    viewport(MIN_WINDOW.width, MIN_WINDOW.height)
    const { view } = mountTool(toolSpec('brush'))
    const grip = view.getByLabelText(t('editor.window.move', { windowName: t('tools.name.brush') }))
    grip.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }))
    const moved = { x: stores.session.windows.tool.x, y: stores.session.windows.tool.y }
    expect(moved.y).toBe(62 + 8)
    await switchTo('autoClean')
    expect(stores.session.windows.tool).toMatchObject(moved)
  })

  it('scrolls its rows, between a header and an action that stay, when the window is too short', async () => {
    /** @type {((entries: unknown[]) => void)|null} */
    let notify = null
    const global = /** @type {any} */ (globalThis)
    const previous = global.ResizeObserver
    global.ResizeObserver = class {
      constructor(/** @type {(entries: unknown[]) => void} */ callback) { notify = callback }
      observe() {}
      disconnect() {}
    }
    restore.push(() => { global.ResizeObserver = previous })
    viewport(MIN_WINDOW.width, 480)
    // A panel 560 tall whose rows hold 440 of it, in a window 480 tall.
    const proto = /** @type {any} */ (HTMLElement.prototype)
    /** @type {Record<string, (node: HTMLElement) => number>} */
    const sizes = {
      offsetHeight: (node) => (node.tagName === 'SECTION' ? 560 : 0),
      clientHeight: (node) => (node.classList.contains('rows') ? 440 : 0),
      scrollHeight: (node) => (node.classList.contains('rows') ? 440 : 0),
    }
    for (const [key, measure] of Object.entries(sizes)) {
      const own = Object.getOwnPropertyDescriptor(proto, key)
      Object.defineProperty(proto, key, { configurable: true, get() { return measure(this) } })
      restore.push(() => {
        if (own) Object.defineProperty(proto, key, own)
        else delete proto[key]
      })
    }
    const { view } = mountTool(textCleanup)
    notify?.([{ borderBoxSize: [{ inlineSize: PANEL_WIDTH, blockSize: 560 }] }])
    await tick()
    expect(shell(view).classList.contains('tight')).toBe(true)
    // Held at the top, the header and its grip on screen.
    expect(stores.session.windows.tool.y).toBe(4)
    await openAdvanced(view)
    await fireEvent.click(view.getByLabelText(t('tools.param.bubbleColor')))
    const picker = screen.getByRole('dialog', { name: t('tools.param.bubbleColor') })
    expect(picker.classList.contains('unclipped')).toBe(true)
    expect(picker.parentElement).toBe(document.body)
    expect(shell(view).contains(picker)).toBe(false)
    await fireEvent.pointerDown(picker)
    expect(screen.getByRole('dialog', { name: t('tools.param.bubbleColor') })).toBe(picker)
    await fireEvent.keyDown(picker, { key: 'Escape' })
    expect(screen.queryByRole('dialog', { name: t('tools.param.bubbleColor') })).toBeNull()
    await fireEvent.click(view.getByLabelText(t('tools.param.bubbleColor')))
    expect(screen.getByRole('dialog', { name: t('tools.param.bubbleColor') })).toBeTruthy()
    await fireEvent.pointerDown(document.body)
    expect(screen.queryByRole('dialog', { name: t('tools.param.bubbleColor') })).toBeNull()
    await fireEvent.click(view.getByLabelText(t('tools.param.bubbleColor')))
    view.unmount()
    expect(screen.queryByRole('dialog', { name: t('tools.param.bubbleColor') })).toBeNull()
  })

  it('drags the panel by its header, and not by the rows under it', async () => {
    const { view } = mountTool(textCleanup)
    const rows = /** @type {HTMLElement} */ (shell(view).querySelector('.rows'))
    await fireEvent.pointerDown(rows, { button: 0, pointerId: 1, clientX: 100, clientY: 200 })
    await fireEvent.pointerMove(shell(view), { pointerId: 1, clientX: 180, clientY: 260 })
    expect(stores.setWindowBox).not.toHaveBeenCalledWith('tool', expect.objectContaining({ x: expect.any(Number), y: expect.any(Number) }))

    const name = view.getByRole('heading', { name: 'Text cleanup' })
    await fireEvent.pointerDown(name, { button: 0, pointerId: 2, clientX: 100, clientY: 70 })
    await fireEvent.pointerMove(shell(view), { pointerId: 2, clientX: 140, clientY: 90 })
    expect(stores.setWindowBox).toHaveBeenCalledWith('tool', { x: 56, y: 82 })
  })
})
