/**
 * The two predicates the tool specs answer about themselves. `toolSpendsCloud`
 * is core logic in the strictest sense - it is the question the interface asks
 * before it lets anything be sent, and a wrong answer either spends a user's
 * money without asking or refuses a request they allowed.
 */

import { describe, expect, it } from 'vitest'
import {
  DRAWING_TOOLS,
  PANEL_WIDTH,
  SOLID,
  TOOL_SPECS,
  activeParams,
  draftShape,
  effectiveChoice,
  hexInvalid,
  hexOnCommit,
  hexOnInput,
  isDrawingTool,
  isSolidFill,
  paramGroups,
  segmentedTabStop,
  toolShell,
  toolSpec,
  toolSpendsCloud,
} from './tools.js'
import { ROW_ENGINES } from '../model/masks.js'
import { t } from '../i18n/index.js'
import tauriConf from '../../../src-tauri/tauri.conf.json'
import { iconNames } from '../icons/paths.js'
import { hexToRgb, hsbToHex, hsbToRgb, rgbToHex, rgbToHsb } from '../ui/color.js'

describe('isDrawingTool', () => {
  it('is the five tools whose gesture is a drag', () => {
    expect([...DRAWING_TOOLS]).toEqual(['brush', 'shapes', 'aiMaskBrush', 'cloneHeal', 'maskSelect'])
  })

  it('excludes the two that act on a region that already exists', () => {
    expect(isDrawingTool('autoClean')).toBe(false)
    expect(isDrawingTool('contentAwareFill')).toBe(false)
  })

  it('excludes anything it does not recognise', () => {
    expect(isDrawingTool('nonesuch')).toBe(false)
    expect(isDrawingTool(undefined)).toBe(false)
  })
})

describe('toolSpendsCloud', () => {
  it('is true only for the option the spec itself marks as cloud', () => {
    expect(toolSpendsCloud('contentAwareFill', { engine: 'cloud' })).toBe(true)
    expect(toolSpendsCloud('contentAwareFill', { engine: 'local' })).toBe(false)
  })

  // Text cleanup used to carry an engine ceiling whose upper rung was the
  // cloud, and a run at that ceiling sent pages to it without the consent each cloud
  // render asks for first, because `needs-confirmation` lives on `applyTool`
  // and `runClean` has no equivalent. The rung is gone and a run is
  // local-only; this is the assertion that says so, whatever it is passed.
  it('is false for Text cleanup, which can no longer reach the cloud rung', () => {
    expect(toolSpendsCloud('autoClean', { engineCeiling: 'cloud' })).toBe(false)
    expect(toolSpendsCloud('autoClean', { engineCeiling: 'lama' })).toBe(false)
    expect(toolSpendsCloud('autoClean', { scope: 'project' })).toBe(false)
  })

  it('is false for a tool with no cloud option at all, whatever it is passed', () => {
    expect(toolSpendsCloud('brush', { engine: 'cloud', mode: 'paint' })).toBe(false)
    expect(toolSpendsCloud('cloneHeal', { engine: 'cloud' })).toBe(false)
  })

  it('is false with no parameters, and for an unknown tool', () => {
    expect(toolSpendsCloud('contentAwareFill')).toBe(false)
    expect(toolSpendsCloud('nonesuch', { engine: 'cloud' })).toBe(false)
  })

  it('covers every cloud option the specs declare - no spec is unreachable', () => {
    const cloudOptions = TOOL_SPECS.flatMap((spec) =>
      spec.params.flatMap((param) =>
        param.kind === 'choice'
          ? param.options
              .filter((option) => option.cloud)
              .map((option) => ({ id: spec.id, key: param.key, value: option.value }))
          : [],
      ),
    )
    expect(cloudOptions.length).toBeGreaterThan(0)
    for (const option of cloudOptions) {
      expect(toolSpendsCloud(option.id, { [option.key]: option.value })).toBe(true)
    }
  })
})

describe('toolSpec', () => {
  it('falls back to Text cleanup rather than to nothing', () => {
    expect(toolSpec('nonesuch').id).toBe('autoClean')
  })

  // Renamed on screen only: saved sessions, the `1` shortcut, the tool
  // parameters and the native run all key on the id.
  it('calls the `autoClean` tool Text cleanup, under its old id and slot', () => {
    const spec = toolSpec('autoClean')
    expect(spec.slot).toBe(1)
    expect(spec.nameKey).toBe('tools.name.autoClean')
    expect(t(spec.nameKey)).toBe('Text cleanup')
    expect(TOOL_SPECS.map((entry) => t(entry.nameKey)).join(' ')).not.toMatch(/auto ?clean/i)
  })

  it('gives the brush its paint parameters unconditionally, and no mode row', () => {
    const brush = toolSpec('brush')
    // The three-way `add` / `erase` / `paint` row is gone: the brush paints.
    expect(brush.params.find((p) => p.key === 'mode')).toBeUndefined()

    const colorParam = brush.params.find((p) => p.key === 'color')
    expect(colorParam).toMatchObject({
      kind: 'color',
      key: 'color',
      labelKey: 'tools.param.color',
      default: '#ffffff',
      group: 'paint',
    })

    // Nothing is conditional any more - there is no other mode to hide them in.
    for (const key of ['color', 'opacity', 'flow']) {
      expect(brush.params.find((p) => p.key === key)?.when).toBeUndefined()
    }

    expect(brush.params.find((p) => p.key === 'opacity')).toMatchObject({
      kind: 'range', key: 'opacity', min: 0, max: 100, step: 5, unit: '%',
    })
    expect(brush.params.find((p) => p.key === 'flow')).toMatchObject({
      kind: 'range', key: 'flow', min: 0, max: 100, step: 5, unit: '%',
    })
  })

  // The three that went are `snap`, `confidence` and `fallback`: all three
  // described a snapping mechanism that no longer exists, and a control that
  // cannot change what happens is worse than no control.
  it('offers the AI mask brush an engine and a size, and nothing about snapping', () => {
    const keys = toolSpec('aiMaskBrush').params.map((param) => param.key)
    expect(keys).toEqual(['engine', 'size'])
  })

  it('declares opacity and flow for cloneHeal', () => {
    const cloneHeal = toolSpec('cloneHeal')
    const opacityParam = cloneHeal.params.find((p) => p.key === 'opacity')
    expect(opacityParam).toMatchObject({ kind: 'range', key: 'opacity', min: 0, max: 100, step: 5, unit: '%' })

    const flowParam = cloneHeal.params.find((p) => p.key === 'flow')
    expect(flowParam).toMatchObject({ kind: 'range', key: 'flow', min: 0, max: 100, step: 5, unit: '%' })
  })
})

/**
 * **Shapes has a mode, and the mode is one list of three.** A drawn shape is
 * either paint - a colour the user picked, laid down flat - or a clean by one
 * of the two available redraw models, and it is never both. The alternatives
 * than settings of one another, which is why they share a row.
 *
 * Fill does not appear in this mode list; the solid option is `solid` and
 * reads "Solid colour".
 */
describe('the Shapes tool', () => {
  const shapes = toolSpec('shapes')
  /** @param {string} key */
  const param = (key) => shapes.params.find((candidate) => candidate.key === key)

  it('offers the four area shapes and a line', () => {
    expect(param('shape')?.options.map((option) => option.value)).toEqual([
      'rect',
      'ellipse',
      'lasso',
      'polygon',
      'line',
    ])
  })

  it('offers a solid colour and the two redraw models', () => {
    const mode = param('mode')
    expect(mode?.kind).toBe('choice')
    expect(mode?.options.map((option) => option.value)).toEqual([SOLID, 'lama', 'flux'])
    // The AI mask brush has the stronger two local models only.
    const brushEngines = toolSpec('aiMaskBrush').params.find((p) => p.key === 'engine')
    for (const rung of ROW_ENGINES) {
      const here = mode?.options.find((option) => option.value === rung)
      const there = brushEngines?.options.find((option) => option.value === rung)
      if (rung === 'fill') {
        expect(here).toBeUndefined()
        expect(there).toBeUndefined()
        continue
      }
      expect(here?.labelKey).toBe(there?.labelKey)
      // Including rung 3a's gate, so the tool bar drops it on a machine
      // with no sidecar exactly as it does there.
      expect(here?.sidecar).toBe(there?.sidecar)
      // And the rung each option *is*, which is what `ToolBar#optionsFor`
      // checks `capabilities.engines` against.
      expect(here?.engine).toBe(rung)
      expect(there?.engine).toBe(rung)
    }
  })

  /**
   * Every option a tool bar can draw for an engine names the rung it is, so
   * a machine that has not downloaded that rung's weights can be told to hide
   * it. An option with no `engine` - Solid colour, Clone / heal's modes  - 
   * is never gated, which is what makes the check safe to apply everywhere.
   */
  it('names the rung on every engine option and on no other option', () => {
    const gated = ['autoClean', 'shapes', 'aiMaskBrush']
    for (const id of gated) {
      for (const param of toolSpec(id).params) {
        if (param.kind !== 'choice') continue
        for (const option of param.options) {
          if (ROW_ENGINES.includes(option.value)) {
            expect(option.engine, `${id}.${param.key}.${option.value}`).toBe(option.value)
          } else {
            expect(option.engine, `${id}.${param.key}.${option.value}`).toBeUndefined()
          }
        }
      }
    }
  })

  it('names the solid option and omits the fill engine', () => {
    const mode = param('mode')
    const solid = mode?.options.find((option) => option.value === SOLID)
    const fill = mode?.options.find((option) => option.value === 'fill')
    expect(solid?.labelKey).toBe('tools.option.modeSolid')
    expect(fill).toBeUndefined()
  })

  it('carries a colour and an opacity, live only while the mode is solid', () => {
    expect(param('color')?.kind).toBe('color')
    expect(param('opacity')).toMatchObject({ kind: 'range', min: 0, max: 100, unit: '%' })
    for (const key of ['color', 'opacity']) {
      expect(param(key)?.when({ mode: SOLID })).toBe(true)
      expect(param(key)?.when({ mode: 'fill' })).toBe(false)
      expect(param(key)?.when({ mode: 'lama' })).toBe(false)
    }
  })

  it('shows the colour only in solid mode, and the rest of the rows always', () => {
    const solid = activeParams(shapes, { mode: SOLID }).map((p) => p.key)
    expect(solid).toEqual(['shape', 'mode', 'color', 'opacity', 'outlineColor', 'outlineWidth', 'feather'])

    const engine = activeParams(shapes, { mode: 'lama' }).map((p) => p.key)
    expect(engine).toEqual(['shape', 'mode', 'feather'])
  })

  it('reads its own mode the same way everywhere', () => {
    // The word itself, pinned: `paint.js`, `api/tools.js`, `DraftPreview` and
    // `region.rs#paint_plan` all spell it as a literal, and this is the one
    // assertion that would catch a rename that reached only some of them.
    expect(SOLID).toBe('solid')
    expect(isSolidFill({ mode: SOLID })).toBe(true)
    expect(isSolidFill({ mode: 'fill' })).toBe(false)
    expect(isSolidFill({})).toBe(false)
    expect(isSolidFill(undefined)).toBe(false)
  })

  // A parameter with no predicate is live for every value, including none -
  // the filter must not drop the rows that never had a condition. Text
  // cleanup's fill colour is one: a clean paints a *saved* Solid pick with it
  // too, so whether the rows read Solid now is not the question
  // (`ToolBar.svelte` decides where a clean reads it).
  it('leaves an unconditional row alone', () => {
    const all = ['step', 'scope', 'outsideBubbles', 'maskPadding', 'bubbleEngine', 'outsideEngine', 'bubbleColor']
    expect(activeParams(toolSpec('autoClean'), { bubbleEngine: SOLID }).map((p) => p.key)).toEqual(all)
    expect(activeParams(toolSpec('autoClean'), { bubbleEngine: 'lama' }).map((p) => p.key)).toEqual(all)
    expect(activeParams(toolSpec('autoClean'), {}).map((p) => p.key)).toEqual(all)
    expect(toolSpec('autoClean').params.find((p) => p.key === 'outsideEngine')?.options.map((o) => o.value))
      .toContain(SOLID)
  })

  // Denoise fill is gone: pages are denoised whole now, so each kind of text
  // starts on Fill, LaMa or a solid colour.
  it('offers Fill, LaMa and Solid colour for each kind of text', () => {
    for (const key of ['bubbleEngine', 'outsideEngine']) {
      expect(toolSpec('autoClean').params.find((p) => p.key === key)?.options.map((o) => o.value))
        .toEqual(['fill', 'lama', SOLID])
    }
  })
})

/**
 * **The selection tool edits the detected masks, and nothing else.** Slot 6,
 * a pill like every drawing tool, no run button and no engine: what happens to
 * the area is Clean's decision, later. Its brush has a size; its lasso and
 * rectangle do not, so the row is there only for the brush.
 */
describe('the selection tool', () => {
  const spec = toolSpec('maskSelect')
  /** @param {string} key */
  const param = (key) => spec.params.find((candidate) => candidate.key === key)

  it('is the sixth tool, a pill with no run action', () => {
    expect(spec.id).toBe('maskSelect')
    expect(spec.slot).toBe(6)
    expect(spec.shell).toBe('pill')
    expect(spec.runnable).toBe(false)
    expect(spec.nameKey).toBe('tools.name.maskSelect')
    expect(t(spec.hintKey)).not.toBe('')
    expect(TOOL_SPECS.map((entry) => entry.slot)).toEqual([1, 2, 3, 4, 5, 6])
    expect(isDrawingTool('maskSelect')).toBe(true)
  })

  it('adds or removes, and draws with a brush, a lasso or a rectangle', () => {
    expect(param('mode')?.options.map((option) => option.value)).toEqual(['add', 'remove'])
    expect(param('shape')?.options.map((option) => option.value)).toEqual(['brush', 'lasso', 'rect'])
    for (const option of [...(param('mode')?.options ?? []), ...(param('shape')?.options ?? [])]) {
      expect(iconNames, option.value).toContain(option.icon)
      expect(t(option.labelKey), option.value).not.toBe('')
    }
  })

  it('shows the size for the brush alone', () => {
    expect(activeParams(spec, { mode: 'add', shape: 'brush' }).map((p) => p.key)).toEqual(['mode', 'shape', 'size'])
    expect(activeParams(spec, { mode: 'add', shape: 'lasso' }).map((p) => p.key)).toEqual(['mode', 'shape'])
    expect(activeParams(spec, { mode: 'remove', shape: 'rect' }).map((p) => p.key)).toEqual(['mode', 'shape'])
    // A fresh record with no shape yet is a brush, as the defaults say.
    expect(activeParams(spec, {}).map((p) => p.key)).toContain('size')
  })

  it('never reaches the cloud, whatever it is passed', () => {
    expect(toolSpendsCloud('maskSelect', { mode: 'add', engine: 'cloud' })).toBe(false)
    for (const choice of spec.params.filter((p) => p.kind === 'choice')) {
      expect(choice.options.some((option) => option.cloud)).toBe(false)
    }
  })
})

/**
 * Which drag a tool draws: an area of some shape, or a round stroke. Shapes
 * is always an area; the selection tool is one only when its row says lasso
 * or rectangle; every other drawing tool strokes.
 */
describe('draftShape', () => {
  it('is the Shapes row, a rectangle when it says nothing', () => {
    expect(draftShape('shapes', { shape: 'ellipse' })).toBe('ellipse')
    expect(draftShape('shapes', { shape: 'polygon' })).toBe('polygon')
    expect(draftShape('shapes', {})).toBe('rect')
  })

  it('is the selection tool’s lasso or rectangle, and a stroke for its brush', () => {
    expect(draftShape('maskSelect', { shape: 'lasso' })).toBe('lasso')
    expect(draftShape('maskSelect', { shape: 'rect' })).toBe('rect')
    expect(draftShape('maskSelect', { shape: 'brush' })).toBeNull()
    expect(draftShape('maskSelect', {})).toBeNull()
    // Shapes' other shapes are not the selection tool's.
    expect(draftShape('maskSelect', { shape: 'polygon' })).toBeNull()
  })

  it('is a stroke for every brush tool', () => {
    for (const tool of ['brush', 'aiMaskBrush', 'cloneHeal']) expect(draftShape(tool, { shape: 'rect' })).toBeNull()
  })
})

/**
 * The groups the tool bar separates with hairlines and names to a screen
 * reader. They are a statement about the parameters - which of them are the
 * same decision - so they live on the spec rather than in the component, and
 * the component draws whatever it is handed.
 */
/**
 * The shape of the tool shell per tool: Text cleanup is a compact panel of
 * rows, and every drawing tool keeps the one-row pill.
 */
describe('toolShell', () => {
  it('is the panel for Text cleanup and the pill for every drawing tool', () => {
    expect(toolShell('autoClean')).toBe('panel')
    for (const id of DRAWING_TOOLS) expect(toolShell(id), id).toBe('pill')
    expect(TOOL_SPECS.filter((spec) => spec.shell === 'panel').map((spec) => spec.id)).toEqual(['autoClean'])
  })

  it('is the pill for a tool it does not know, which cannot outgrow a small window', () => {
    expect(toolShell('nonesuch')).toBe('pill')
    expect(toolShell(undefined)).toBe('pill')
  })

  it('draws the panel at the plan’s width, inside the smallest window the app allows', () => {
    expect(PANEL_WIDTH).toBeGreaterThanOrEqual(360)
    expect(PANEL_WIDTH).toBeLessThanOrEqual(400)
    expect(PANEL_WIDTH).toBeLessThan(Number(tauriConf.app.windows[0].minWidth))
  })

  it('names the three modes the panel offers, and keeps both halves the default', () => {
    const step = toolSpec('autoClean').params.find((p) => p.key === 'step')
    expect(step?.options.map((option) => t(option.labelKey)).sort()).toEqual(['Clean', 'Detect', 'Detect & clean'])
    expect(step?.options[0].value).toBe('auto')
  })
})

describe('paramGroups', () => {
  // A plain name and not an i18n key: the bar draws a hairline between groups
  // and no heading, so there is nothing to translate.
  it('gives every parameter a group named by a plain id, on every tool', () => {
    for (const spec of TOOL_SPECS) {
      for (const group of paramGroups(spec)) {
        expect(group.key, spec.id).toMatch(/^[a-z]+$/)
      }
    }
  })

  it('loses no row and reorders none', () => {
    for (const spec of TOOL_SPECS) {
      const flat = paramGroups(spec).flatMap((group) => group.params.map((p) => p.key))
      expect(flat, spec.id).toEqual(activeParams(spec).map((p) => p.key))
    }
  })

  it('reads Text cleanup as the run, the text it takes, the mask padding, then the engines', () => {
    for (const bubbleEngine of [SOLID, 'lama']) {
      expect(paramGroups(toolSpec('autoClean'), { bubbleEngine }).map((group) => [group.key, group.params.length]))
        .toEqual([
          ['run', 2],
          ['text', 1],
          ['mask', 1],
          ['engines', 3],
        ])
    }
  })

  // A parameter hidden by its `when` predicate takes no group with it: Shapes
  // in an engine mode loses its colour and its opacity, and *Mode* stays,
  // because `mode` and `feather` are still in it.
  it('keeps a group whose other parameters went away', () => {
    const engine = paramGroups(toolSpec('shapes'), { mode: 'lama' })
    expect(engine.map((group) => group.key)).toEqual(['shape', 'mode'])
    expect(engine[1].params.map((p) => p.key)).toEqual(['mode', 'feather'])

    const solid = paramGroups(toolSpec('shapes'), { mode: SOLID })
    expect(solid[1].params.map((p) => p.key)).toEqual(['mode', 'color', 'opacity', 'outlineColor', 'outlineWidth', 'feather'])
  })
})

describe('segmentedTabStop', () => {
  it('uses the selected option index when that option is enabled', () => {
    const items = [
      { value: 'a', disabled: false },
      { value: 'b', disabled: false },
      { value: 'c', disabled: false },
    ]
    expect(segmentedTabStop(items, 1)).toBe(1)
    expect(segmentedTabStop(items, 0)).toBe(0)
    expect(segmentedTabStop(items, 2)).toBe(2)
  })

  it('falls back to the first enabled option when the selected option is disabled', () => {
    const items = [
      { value: 'a', disabled: true },
      { value: 'b', disabled: false },
      { value: 'c', disabled: false },
    ]
    expect(segmentedTabStop(items, 0)).toBe(1)
  })

  it('falls back to the first enabled option when nothing is selected yet', () => {
    const items = [
      { value: 'a', disabled: true },
      { value: 'b', disabled: false },
      { value: 'c', disabled: false },
    ]
    expect(segmentedTabStop(items, -1)).toBe(1)
  })

  it('returns -1 when all options are disabled or items list is empty', () => {
    const items = [
      { value: 'a', disabled: true },
      { value: 'b', disabled: true },
    ]
    expect(segmentedTabStop(items, 0)).toBe(-1)
    expect(segmentedTabStop(items, -1)).toBe(-1)
    expect(segmentedTabStop([], -1)).toBe(-1)
  })
})

describe('effectiveChoice', () => {
  const param = {
    kind: /** @type {const} */ ('choice'),
    key: 'mode',
    labelKey: 'test.mode',
    options: [
      { value: 'solid', labelKey: 'test.solid' },
      { value: 'fill', labelKey: 'test.fill' },
      { value: 'lama', labelKey: 'test.lama' },
      { value: 'flux', labelKey: 'test.flux' },
    ],
  }

  it('keeps the stored value when it is present in available options', () => {
    const available = [{ value: 'solid' }, { value: 'fill' }, { value: 'lama' }]
    expect(effectiveChoice(param, available, 'lama')).toBe('lama')
    expect(effectiveChoice(param, available, 'solid')).toBe('solid')
  })

  it('falls back to the first available option when the stored value is filtered out', () => {
    const available = [{ value: 'solid' }, { value: 'fill' }, { value: 'lama' }]
    expect(effectiveChoice(param, available, 'flux')).toBe('solid')
  })

  it('returns the stored value (or param default) when no options are available', () => {
    expect(effectiveChoice(param, [], 'flux')).toBe('flux')
    expect(effectiveChoice(param, [], undefined)).toBe('solid')
  })

  it('supports string-based options arrays', () => {
    expect(effectiveChoice(undefined, ['page', 'project'], 'project')).toBe('project')
    expect(effectiveChoice(undefined, ['page', 'project'], 'stale')).toBe('page')
  })
})

describe('hexOnInput', () => {
  it('commits only when the text is exactly 6 hex digits', () => {
    expect(hexOnInput('#aabbcc')).toBe('#aabbcc')
    expect(hexOnInput('aabbcc')).toBe('#aabbcc')
    expect(hexOnInput('#AABBCC')).toBe('#aabbcc')
    expect(hexOnInput('#000000')).toBe('#000000')
  })

  it('returns null for 3-digit shorthand while typing', () => {
    expect(hexOnInput('#abc')).toBeNull()
    expect(hexOnInput('abc')).toBeNull()
    expect(hexOnInput('#aab')).toBeNull()
  })

  it('returns null for partial or invalid hex text', () => {
    expect(hexOnInput('#a')).toBeNull()
    expect(hexOnInput('#ab')).toBeNull()
    expect(hexOnInput('#aabb')).toBeNull()
    expect(hexOnInput('#aabbc')).toBeNull()
    expect(hexOnInput('#aabbccd')).toBeNull()
    expect(hexOnInput('xyz123')).toBeNull()
    expect(hexOnInput('')).toBeNull()
    expect(hexOnInput(null)).toBeNull()
  })
})

describe('hexOnCommit', () => {
  it('commits exactly 6 hex digits in canonical lowercase form', () => {
    expect(hexOnCommit('#aabbcc')).toBe('#aabbcc')
    expect(hexOnCommit('aabbcc')).toBe('#aabbcc')
    expect(hexOnCommit('#AABBCC')).toBe('#aabbcc')
  })

  it('expands 3-digit shorthand to 6 digits on commit', () => {
    expect(hexOnCommit('#abc')).toBe('#aabbcc')
    expect(hexOnCommit('abc')).toBe('#aabbcc')
    expect(hexOnCommit('#ABC')).toBe('#aabbcc')
    expect(hexOnCommit('#aab')).toBe('#aaaabb')
    expect(hexOnCommit('#fff')).toBe('#ffffff')
  })

  it('returns null for partial or invalid hex values', () => {
    expect(hexOnCommit('#ab')).toBeNull()
    expect(hexOnCommit('#aabb')).toBeNull()
    expect(hexOnCommit('#aabbc')).toBeNull()
    expect(hexOnCommit('#aabbccd')).toBeNull()
    expect(hexOnCommit('invalid')).toBeNull()
    expect(hexOnCommit('')).toBeNull()
    expect(hexOnCommit(undefined)).toBeNull()
  })
})


describe('hexInvalid', () => {
  it('is silent about a value that is a colour', () => {
    expect(hexInvalid('#aabbcc')).toBe(false)
    expect(hexInvalid('aabbcc')).toBe(false)
    expect(hexInvalid('#ABC')).toBe(false)
    expect(hexInvalid('  #abc  ')).toBe(false)
  })

  it('is silent about an empty field, which has rejected nothing', () => {
    expect(hexInvalid('')).toBe(false)
    expect(hexInvalid('   ')).toBe(false)
    expect(hexInvalid(null)).toBe(false)
    expect(hexInvalid(undefined)).toBe(false)
  })

  it('says so about text that is not a colour, half-typed or otherwise', () => {
    expect(hexInvalid('#ab')).toBe(true)
    expect(hexInvalid('#aabb')).toBe(true)
    expect(hexInvalid('#aabbccd')).toBe(true)
    expect(hexInvalid('zzz')).toBe(true)
    expect(hexInvalid('rebeccapurple')).toBe(true)
  })
})

describe('custom color picker conversions', () => {
  it('maps the HSB primaries and canonical hex values exactly', () => {
    expect(hsbToRgb(0, 100, 100)).toEqual({ r: 255, g: 0, b: 0 })
    expect(hsbToRgb(120, 100, 100)).toEqual({ r: 0, g: 255, b: 0 })
    expect(hsbToRgb(240, 100, 100)).toEqual({ r: 0, g: 0, b: 255 })
    expect(hsbToHex(210, 67, 60)).toBe('#326699')
  })

  it('round-trips RGB through HSB within one byte per channel', () => {
    for (const rgb of [
      { r: 0, g: 0, b: 0 },
      { r: 255, g: 255, b: 255 },
      { r: 18, g: 52, b: 86 },
      { r: 240, g: 91, b: 17 },
    ]) {
      const hsb = rgbToHsb(rgb.r, rgb.g, rgb.b)
      const roundTrip = hsbToRgb(hsb.h, hsb.s, hsb.b)
      expect(Math.abs(roundTrip.r - rgb.r)).toBeLessThanOrEqual(1)
      expect(Math.abs(roundTrip.g - rgb.g)).toBeLessThanOrEqual(1)
      expect(Math.abs(roundTrip.b - rgb.b)).toBeLessThanOrEqual(1)
      expect(hexToRgb(rgbToHex(rgb.r, rgb.g, rgb.b))).toEqual(rgb)
    }
  })
})

/**
 * **How the bar draws a choice is the spec's own statement.** A parameter whose
 * options *all* carry an icon is a group of icon cells; one whose options carry
 * none is a dropdown. A mixture would be a group of cells where some are
 * pictures and some are words, so it is asserted away here rather than guarded
 * against in the component (`editor/ToolBar.svelte#iconChoice`).
 */
describe('the icons and the short labels the bar draws a choice with', () => {
  /** Every choice parameter there is, tool by tool. */
  const choices = TOOL_SPECS.flatMap((spec) =>
    spec.params
      .filter((param) => param.kind === 'choice')
      .map((param) => ({ id: spec.id, param })),
  )

  it('has each choice either all icons or none, never a mixture', () => {
    for (const { id, param } of choices) {
      const withIcon = param.options.filter((option) => option.icon).length
      expect([0, param.options.length], `${id}.${param.key}`).toContain(withIcon)
    }
  })

  it('names a glyph that exists for every icon option', () => {
    const drawn = choices.flatMap(({ id, param }) =>
      param.options.filter((option) => option.icon).map((option) => ({
        where: `${id}.${param.key}.${option.value}`,
        icon: option.icon,
      })),
    )
    expect(drawn.length).toBeGreaterThan(0)
    for (const { where, icon } of drawn) {
      expect(iconNames, where).toContain(icon)
    }
  })

  // Text cleanup's scope is words now: its panel has room for them.
  it('draws the three pictured choices as icons and the ladders as dropdowns', () => {
    const pictured = choices
      .filter(({ param }) => param.options.every((option) => option.icon))
      .map(({ id, param }) => `${id}.${param.key}`)
    expect(pictured).toEqual([
      'shapes.shape',
      'cloneHeal.alignment',
      'cloneHeal.mode',
      'maskSelect.mode',
      'maskSelect.shape',
    ])
  })

  // A dropdown draws its short label and its value side by side, so the ones
  // whose full label is a sentence carry a short form. An icon group draws no
  // words at all and never needs one.
  it('gives a short label to the dropdowns whose full one is a sentence', () => {
    const short = choices
      .filter(({ param }) => param.shortKey)
      .map(({ id, param }) => [`${id}.${param.key}`, param.shortKey])
    // Text cleanup's panel labels its rows in full, and needs none.
    expect(short).toEqual([
      ['shapes.mode', 'tools.short.mode'],
      ['aiMaskBrush.engine', 'tools.short.cleanWith'],
    ])
    // Never on a choice the bar draws as icons: there is no trigger to put it on.
    for (const { id, param } of choices) {
      if (param.options.every((option) => option.icon)) {
        expect(param.shortKey, `${id}.${param.key}`).toBeUndefined()
      }
    }
  })
})
