/**
 * What the tool shell shows for each tool: its name, the hint it carries as a
 * tooltip, the parameters themselves, and which shape the shell takes for it -
 * the one-row pill every drawing tool uses, or the compact panel Text cleanup
 * uses (`shell`).
 *
 * **No prose.** The bar carries controls and nothing that explains them: the
 * tools are for translators rather than for retouchers, and a paragraph over
 * every tool is a paragraph nobody reads twice. What a tool does belongs in the
 * help, which is one place rather than six.
 *
 * Every number here is the design file's (`defs` in `renderVals()`). Nothing in
 * this module is user-visible text - labels are i18n keys, and the values are
 * the ids `src/lib/state/editor.svelte.js` stores in `editor.toolParams`.
 *
 * The tool ids and their `1` to `6` order come from `TOOLS` in the state module;
 * this file must stay in step with it, which `toolSpec()` asserts by returning
 * the Text cleanup spec for anything it does not recognise.
 *
 * **Text cleanup is `autoClean` inside.** The tool was called Auto clean on
 * screen; only the words changed. The id is what saved sessions, the `1`
 * shortcut, `editor.toolParams`, `applyTool` and the native run all key on, so
 * it stays.
 */

import { MAX_MASK_PADDING, ROW_ENGINES, engineChoiceLabel } from '../model/masks.js'

/**
 * @typedef {Object} RangeParam
 * @property {'range'} kind
 * @property {string} key - the field in `editor.toolParams[tool]`
 * @property {string} labelKey
 * @property {number} min
 * @property {number} max
 * @property {number} step
 * @property {'px'|'%'} [unit]
 * @property {string} [group] - the group this parameter belongs to
 * @property {(values: Record<string, unknown>) => boolean} [when] - live only while this holds
 */

/**
 * @typedef {Object} ChoiceOption
 * @property {string} value
 * @property {string} labelKey
 * @property {string} [icon] - a glyph from `src/lib/icons/paths.js`. Where
 *   *every* option of a parameter carries one the bar draws the parameter as a
 *   group of icon cells and no words; where none does, it is a dropdown. A
 *   parameter is one or the other, never a mixture.
 * @property {boolean} [cloud]
 * @property {boolean} [sidecar]
 * @property {string} [engine]
 */

/**
 * @typedef {Object} ChoiceParam
 * @property {'choice'} kind
 * @property {string} key
 * @property {string} labelKey
 * @property {string} [shortKey] - what the dropdown's trigger calls this
 *   parameter, where the full label is too long to sit on a bar in front of its
 *   own value. The full label is still the control's accessible name.
 * @property {ChoiceOption[]} options
 * @property {string} [group]
 * @property {(values: Record<string, unknown>) => boolean} [when]
 */

/**
 * @typedef {Object} ColorParam
 * @property {'color'} kind
 * @property {string} key
 * @property {string} labelKey
 * @property {string} [default]
 * @property {string} [group]
 * @property {(values: Record<string, unknown>) => boolean} [when]
 */

/**
 * @typedef {Object} ToolSpec
 * @property {string} id
 * @property {number} slot - the number key that selects it
 * @property {string} nameKey
 * @property {string} hintKey - the bar's tooltip on the tool's own name
 * @property {Array<RangeParam|ChoiceParam|ColorParam>} params
 * @property {boolean} runnable - carries the run / cancel action button
 * @property {'pill'|'panel'} shell - the shape the tool shell takes for it:
 *   one row of controls, or a compact panel of labelled rows
 */

/**
 * A parameter that is only *live* under some other parameter's value.
 *
 * The colour a Shape is filled with means nothing while the Shape is being
 * cleaned by an engine rather than painted flat. A control that cannot change what happens is
 * worse than no control (the same rule that removed the AI mask brush's three
 * dead rows), so those rows are **absent** rather than disabled: nothing is
 * blocked and there is no reason to explain.
 *
 * The value is still kept in `editor.toolParams` while the row is away, so
 * switching back to the mode restores the colour that was picked for it.
 *
 * @template {RangeParam|ChoiceParam|ColorParam} P
 * @param {P} param
 * @param {(values: Record<string, unknown>) => boolean} predicate
 * @returns {P}
 */
function onlyWhen(param, predicate) {
  return { ...param, when: predicate }
}

/**
 * The parameters of a spec that are live for these values - what the tool bar
 * renders, and the one place the `when` predicates are read.
 *
 * @param {ToolSpec} spec
 * @param {Record<string, unknown>} values
 * @returns {Array<RangeParam|ChoiceParam|ColorParam>}
 */
export function activeParams(spec, values = {}) {
  return spec.params.filter(
    (param) => typeof (/** @type {any} */ (param).when) !== 'function' ||
      /** @type {any} */ (param).when(values ?? {}),
  )
}

/**
 * Put a run of parameters in one group.
 *
 * The group is the bar's only structure, and it is what the hairlines separate:
 * "Speech bubble text" and "Text outside bubbles" are the two halves of one
 * question and stand together, away from the scope beside them. Grouping lives
 * here rather than in the component because it is a statement about the
 * parameters - which of them are the same decision - and the component's job is
 * to draw whatever it is handed.
 *
 * The id is a **plain name and not an i18n key**. It was a key while the tool
 * window drew a small uppercase heading over each group; a bar has no second
 * line to put a heading on, and a group that is drawn as a hairline and read
 * out as nothing does not need a word.
 *
 * The order inside a group is the order it is written in, and the groups
 * themselves are read in spec order by `paramGroups`, so a group is a **run of
 * adjacent parameters** and never a filter that reshuffles them.
 *
 * @template {RangeParam|ChoiceParam|ColorParam} P
 * @param {string} groupKey
 * @param {P[]} params
 * @returns {P[]}
 */
function section(groupKey, params) {
  return params.map((param) => ({ ...param, group: groupKey }))
}

/**
 * The live rows of a spec, split into the sections the tool bar draws.
 *
 * A group ends where the next row names a different one, so a row hidden by
 * its `when` predicate takes no group with it - Shapes in an engine mode loses
 * its colour and its opacity and the *Mode* group stays, because `mode` and
 * `feather` are still in it.
 *
 * @param {ToolSpec} spec
 * @param {Record<string, unknown>} [values]
 * @returns {Array<{key: string|null, params: Array<RangeParam|ChoiceParam|ColorParam>}>}
 */
export function paramGroups(spec, values = {}) {
  /** @type {Array<{key: string|null, params: Array<RangeParam|ChoiceParam|ColorParam>}>} */
  const groups = []
  for (const param of activeParams(spec, values)) {
    const key = /** @type {any} */ (param).group ?? null
    const last = groups[groups.length - 1]
    if (last && last.key === key && key !== null) last.params.push(param)
    else groups.push({ key, params: [param] })
  }
  return groups
}

/**
 * @param {string} key
 * @param {string} labelKey
 * @param {number} min
 * @param {number} max
 * @param {number} step
 * @param {'px'|'%'} [unit]
 * @returns {RangeParam}
 */
function range(key, labelKey, min, max, step, unit) {
  return { kind: 'range', key, labelKey, min, max, step, unit }
}

/**
 * @param {string} key
 * @param {string} labelKey
 * @param {ChoiceOption[]} options
 * @param {string} [shortKey] - the trigger's own label, where the row is a
 *   dropdown and the full one is too long for a bar
 * @returns {ChoiceParam}
 */
function choice(key, labelKey, options, shortKey) {
  return shortKey
    ? { kind: 'choice', key, labelKey, shortKey, options }
    : { kind: 'choice', key, labelKey, options }
}

/**
 * @param {string} key
 * @param {string} labelKey
 * @param {string} [defaultValue]
 * @returns {ColorParam}
 */
export function color(key, labelKey, defaultValue = '#ffffff') {
  return { kind: 'color', key, labelKey, default: defaultValue }
}

/**
 * Which **model** a kind of text starts on, offered once per kind of text.
 *
 * These used to be two words - `fill` and `redraw` - deliberately named for
 * what the user watches happen rather than for the rung that does it. The user
 * ruled against that: an engine picker exists
 * so that somebody can move between models, and two verbs that each stand for
 * a family of two make that impossible. So the row is the ladder's own list,
 * under the ladder's own names - the same list and the same words a Layers
 * row's picker offers (`ROW_ENGINES`, `engineChoiceLabel`).
 *
 * **Rung 3a is not on it**, and that is not a naming decision: `runClean` has
 * no confirmation protocol, so `HIGHEST_AUTOMATIC` in `src-tauri/src/run.rs`
 * caps every automatic run at manga-LaMa whatever it is asked for. A FLUX
 * entry here would be a control that silently did nothing on every region of
 * every page. It stays reachable per region,
 * where a person chooses it and waits for it.
 *
 * A choice here is a **starting point, not a ceiling**: `clean_region` in
 * `src-tauri/src/run.rs` still escalates when the quality metric declines a
 * patch, so picking `fill` for a bubble that turns out to be screentone still
 * ends on the inpainter rather than on a hole.
 */
const ENGINES = ROW_ENGINES.filter((rung) => rung !== 'flux').map((rung) => ({
  value: rung,
  labelKey: engineChoiceLabel(rung),
  // Which rung this option *is*, so `ToolBar#optionsFor` can drop the ones
  // whose weights are not on this machine. Written here rather than inferred
  // from `value` because `value` is a tool parameter and could stop being a
  // rung name; the gate is about the ladder either way.
  engine: rung,
}))

/**
 * The shared engine options, weakest first. Drawing tools select the redraw
 * models from this list, while Layers can still offer all of them.
 *
 * **The Layers row's own list and the Layers row's own words** - `ROW_ENGINES`
 * and `engineChoiceLabel` from `src/lib/model/masks.js`, not a second list
 * beside them. The stroke and the row are asking the same question of the same
 * region ("clean this with *that*"), and a user who learns "LaMa" on a row
 * must not meet a different vocabulary for it on the canvas.
 *
 * Unlike `ENGINES` above these are **rungs named outright, not picks**: what
 * `src-tauri/src/region.rs#named_rung` reads from `params.engine`, and it runs
 * that rung rather than starting there. Text cleanup's two choices offer the
 * two local ones, because `ENGINES` above drops `flux`
 * from a run nobody is watching. The difference between naming a rung and
 * picking one is what a run does after the first patch - Text cleanup escalates
 * past a declined rung, a named one is committed as it came out.
 *
 * **A rung is offered only where it can run.** `flux` needs the sidecar and
 * rung 2 needs weights that are downloaded after install, so every option
 * carries the rung it is and `ToolBar.svelte` drops the ones
 * `state/capabilities.svelte.js` reports missing - hidden rather than
 * disabled, and with the sidecar's own reason rendered underneath where there
 * is anything to say.
 */
const MASK_ENGINES = ROW_ENGINES.map((rung) => ({
  value: rung,
  labelKey: engineChoiceLabel(rung),
  engine: rung,
  // Rung 3a alone, and it carries a *second* flag because it is the one rung
  // with something to say when it is missing: `capabilities.sidecarReasonKey`
  // is a sentence about a sidecar that is installed and unusable, which no
  // undownloaded weight has an equivalent of. `ToolBar#gateNote` renders it.
  sidecar: rung === 'flux',
}))

/**
 * The value of Shapes' `mode` that means **paint this shape flat**, rather than
 * clean what is under it.
 *
 * It is not called `fill`, and the distance between the two words is the whole
 * reason it has a name at all: `fill` is rung 0, the engine that measures the
 * paper just outside the mask and paints that one colour on it - and is
 * deliberately absent from the shape picker. `solid` covers the shape in the
 * colour the user picked, which is a different act with a different
 * outcome. The label says "Solid colour" for the same reason.
 */
export const SOLID = 'solid'

/**
 * @param {Record<string, unknown>} values
 * @returns {boolean} whether Shapes is set to paint rather than to clean
 */
export function isSolidFill(values) {
  return (values ?? {}).mode === SOLID
}

/**
 * What a drawn shape is *for*: a flat colour, or one of the cleaning engines.
 *
 * One row rather than two, because the two are alternatives rather than
 * settings of one another - a shape is either paint or a clean, never both -
 * and a second row would be a control that is dead half the time. Shapes
 * offer LaMa Manga and available FLUX alongside Solid; Fill is not a useful
 * mode for a drawn shape.
 */
const BRUSH_ENGINES = MASK_ENGINES.filter((option) => option.value !== 'fill')
const SHAPE_MODES = [{ value: SOLID, labelKey: 'tools.option.modeSolid' }, ...BRUSH_ENGINES]

/** @type {ToolSpec[]} */
export const TOOL_SPECS = [
  {
    id: 'autoClean',
    slot: 1,
    nameKey: 'tools.name.autoClean',
    hintKey: 'tools.hint.autoClean',
    params: [
      // What the run does, then over what. The mode names the run button
      // too ("Detect chapter", "Clean page"): Detect finds and stores the
      // regions and changes no pixel, Clean cleans the stored ones and finds
      // nothing new, and Detect & clean is both, which is what the tool always
      // did (docs/detect-clean.md). Where each half runs is not a parameter of
      // the tool but a setting, so `ToolBar.svelte` draws those two choices
      // beside these from `state/cloudtargets.svelte.js`.
      //
      // The panel has room for words, so the scope is three words rather than
      // three glyphs. Project is offered only while it can run: a cloud half
      // cannot cover chapters nobody looked at (`cloudrun.js#cloudScopeRefusal`),
      // and the panel drops the option rather than offering a run it refuses.
      ...section('run', [
        choice('step', 'tools.param.step', [
          { value: 'auto', labelKey: 'tools.option.stepAuto' },
          { value: 'detect', labelKey: 'tools.option.stepDetect' },
          { value: 'clean', labelKey: 'tools.option.stepClean' },
        ]),
        choice('scope', 'tools.param.scope', [
          { value: 'page', labelKey: 'tools.option.scopePage' },
          { value: 'chapter', labelKey: 'tools.option.scopeChapter' },
          { value: 'project', labelKey: 'tools.option.scopeProject' },
        ]),
      ]),
      // The pipeline's own rule: text outside a balloon is "cleaned only if
      // the user opts in". This is the opt-in. Off, the run
      // holds that text for review under "text outside a speech bubble" and
      // the engine row below only bites through *Clean anyway*; on, every such
      // region goes to the ladder starting on that row's rung. No
      // script is read for it either way - sound effects are conceded, not
      // classified - so the person turning this on is the one deciding the
      // chapter's free text is all safe to clean. The all-text policy cleans
      // it regardless (`run.rs`), so the panel draws it only under the
      // script-filtered one.
      ...section('text', [
        choice('outsideBubbles', 'tools.param.outsideBubbles', [
          { value: 'review', labelKey: 'tools.option.outsideReview' },
          { value: 'clean', labelKey: 'tools.option.outsideClean' },
        ]),
      ]),
      // How far every detected mask is grown past the fit, in page pixels.
      // Detect and Detect & clean grow the masks they fit by it; the panel's
      // Apply beside it re-pads masks already detected, from their unpadded
      // shape (`region.rs#set_detection_padding`).
      ...section('mask', [range('maskPadding', 'tools.param.maskPadding', 0, MAX_MASK_PADDING, 1, 'px')]),
      // Two rows rather than one, because the two kinds of text want opposite
      // engines and always did: a speech balloon is flat white or flat black
      // and the fill family covers it for nothing, where text over art has to
      // have the art put back. The run used to route both by the fit alone,
      // which sent bubble text to the inpainter whenever the fit was unsettled
      // - a 510 MB session and a second a region to redraw paper that a fill
      // would have matched exactly. `bubbleEngine` and `outsideEngine` travel
      // to `runClean` and are applied per region by whether the region is
      // inside a balloon.
      //
      // **The clean is where a pick is decided**, not detection. Every
      // clean a run makes - Clean, and Detect & clean, on regions found now
      // or detected earlier - starts each region from these rows, and a cloud
      // clean saves them onto its regions before it plans them, so a LaMa
      // pick is cleaned on this computer (`cloud_clean.rs#PrepareRequest`).
      // Detect still saves a pick with each region (`run.rs#stored_pick`),
      // which a Layers row's own clean starts from. So the panel draws the
      // two rows for Clean and for Detect & clean, and never for Detect,
      // where they would decide nothing.
      //
      // The fill colour is the other way round: a clean reads it, detection
      // does not, and it paints a saved Solid pick that holds no balloon tone
      // of its own as well as a fresh one. So it is drawn wherever this
      // computer cleans, whatever the rows say now, rather than only while one
      // of them reads Solid.
      ...section('engines', [
        choice('bubbleEngine', 'tools.param.bubbleText', [
          ...ENGINES,
          { value: SOLID, labelKey: 'tools.option.modeSolid' },
        ]),
        choice('outsideEngine', 'tools.param.outsideText', [
          ...ENGINES,
          { value: SOLID, labelKey: 'tools.option.modeSolid' },
        ]),
        color('bubbleColor', 'tools.param.bubbleColor', '#ffffff'),
      ]),
      // There is deliberately no engine-ceiling row. It offered two rungs of
      // src/lib/model/ladder.js - the highest local engine, or the whole ladder
      // including cloud - and the cloud rung sent pages off the machine
      // without the consent every cloud render asks for first:
      // `needs-confirmation` lives on `applyTool`, and `runClean` has no
      // equivalent. So a local run stays local: `startRun` pins the ceiling
      // to LOCAL_CEILING, which is enforced where the sending happens, not
      // merely unoffered here.
      //
      // The cloud came back as a *place*, not a rung, and with its consent.
      // The panel's Clean on choice (`cleanTarget`) routes the Clean half to
      // the cloud GPU, and that never goes through `runClean`: the run detects
      // here, then `prepare_cloud_clean` states the exact regions and cost,
      // the cloud clean consent asks, and only its grant starts the renders
      // (`editor/cloudrun.js`). The batch is bounded by what was detected and
      // shown, which is what the old ceiling could not offer.
    ],
    runnable: true,
    shell: 'panel',
  },
  {
    id: 'brush',
    slot: 2,
    nameKey: 'tools.name.brush',
    hintKey: 'tools.hint.brush',
    params: [
      ...section('brush', [
        range('size', 'tools.param.size', 4, 120, 2, 'px'),
        range('hardness', 'tools.param.hardness', 0, 100, 5, '%'),
        range('spacing', 'tools.param.spacing', 1, 50, 1, '%'),
      ]),
      // **The brush paints, and that is all it does.** It used to carry a
      // three-way `mode` row - add to a mask, erase from one, or paint - and
      // the first two were a second, worse route to work the rest of the
      // editor already owns: the AI mask brush strokes a mask an engine then
      // cleans, and a mask is removed from the Layers row that lists it or
      // from the region menu over it. What was left was a row whose two dead
      // options hid the colour, opacity and flow behind a chip press. The
      // parameters are unconditional now, and `mode` stays pinned to `paint`
      // in `editor.toolParams` because it is what the seam reads
      // (`paint.js#paintParamsOf`, `region.rs#paint_plan`).
      ...section('paint', [
        color('color', 'tools.param.color', '#ffffff'),
        range('opacity', 'tools.param.opacity', 0, 100, 5, '%'),
        range('flow', 'tools.param.flow', 0, 100, 5, '%'),
      ]),
    ],
    runnable: false,
    shell: 'pill',
  },
  {
    id: 'shapes',
    slot: 3,
    nameKey: 'tools.name.shapes',
    hintKey: 'tools.hint.shapes',
    params: [
      ...section('shape', [
        choice('shape', 'tools.param.shape', [
          { value: 'rect', labelKey: 'tools.option.rect', icon: 'shape-rect' },
          { value: 'ellipse', labelKey: 'tools.option.ellipse', icon: 'shape-ellipse' },
          { value: 'lasso', labelKey: 'tools.option.lasso', icon: 'shape-lasso' },
          { value: 'polygon', labelKey: 'tools.option.polygon', icon: 'shape-polygon' },
          { value: 'line', labelKey: 'tools.option.line', icon: 'shape-line' },
        ]),
      ]),
      // What the shape *does*. Shapes used to have no such row and always
      // cleaned with whatever the fill mode implied, so the one tool whose
      // gesture describes an area outright was the one tool that could not be
      // pointed at an engine - and could not lay down a colour at all, which
      // is what a shape is reached for when a credit block or a stray panel
      // gutter has to go flat.
      ...section('mode', [
        choice('mode', 'tools.param.mode', SHAPE_MODES, 'tools.short.mode'),
        onlyWhen(color('color', 'tools.param.color', '#ffffff'), isSolidFill),
        // The paint plan carries a stroke-level opacity, so a solid fill can be
        // a wash as well as a cover. Meaningless for the engine modes, which
        // replace what is under them outright.
        onlyWhen(range('opacity', 'tools.param.opacity', 0, 100, 5, '%'), isSolidFill),
        onlyWhen(color('outlineColor', 'tools.param.outlineColor', '#000000'), isSolidFill),
        onlyWhen(range('outlineWidth', 'tools.param.outlineWidth', 0, 30, 1, 'px'), isSolidFill),
        range('feather', 'tools.param.feather', 0, 20, 1, 'px'),
      ]),
    ],
    runnable: false,
    shell: 'pill',
  },
  {
    id: 'aiMaskBrush',
    slot: 4,
    nameKey: 'tools.name.aiMaskBrush',
    hintKey: 'tools.hint.aiMaskBrush',
    params: [
      // Which engine, then how wide the stroke is. The order is the order the
      // gesture is thought about: pick what should happen to the paint, set the
      // brush, then paint.
      //
      // **There used to be three more** - snap strength, show confidence,
      // fallback on/off - and all three described a snapping mechanism that no
      // longer exists. The stroke is the mask now, so there is nothing to snap
      // to, no confidence to show and nothing to fall back from. A control
      // that cannot change what happens is worse than no control.
      ...section('engines', [
        choice('engine', 'tools.param.cleanWith', [
          ...BRUSH_ENGINES,
          { value: 'cloud', labelKey: 'tools.option.engineCloud', cloud: true },
        ], 'tools.short.cleanWith'),
      ]),
      ...section('brush', [range('size', 'tools.param.size', 8, 160, 4, 'px')]),
    ],
    runnable: false,
    shell: 'pill',
  },
  {
    id: 'cloneHeal',
    slot: 5,
    nameKey: 'tools.name.cloneHeal',
    hintKey: 'tools.hint.cloneHeal',
    params: [
      ...section('brush', [
        range('size', 'tools.param.size', 4, 120, 2, 'px'),
        range('hardness', 'tools.param.hardness', 0, 100, 5, '%'),
        range('opacity', 'tools.param.opacity', 0, 100, 5, '%'),
        range('flow', 'tools.param.flow', 0, 100, 5, '%'),
      ]),
      ...section('mode', [
        choice('alignment', 'tools.param.alignment', [
          { value: 'aligned', labelKey: 'tools.option.aligned', icon: 'link' },
          { value: 'nonAligned', labelKey: 'tools.option.nonAligned', icon: 'link-off' },
        ]),
        choice('mode', 'tools.param.mode', [
          { value: 'clone', labelKey: 'tools.option.clone', icon: 'stamp' },
          { value: 'heal', labelKey: 'tools.option.heal', icon: 'bandage' },
        ]),
      ]),
    ],
    runnable: false,
    shell: 'pill',
  },
  {
    // The selection tool: it edits the detected masks that Clean erases,
    // after a Detect and before a Clean, and changes no pixel of the page.
    // Add merges the gesture into the detection it overlaps most, or makes a
    // new one; remove takes it out of every detection it touches
    // (`api/backend.js#editDetectionMask`). No engine and no cloud option:
    // what happens to the area is Clean's decision, later.
    id: 'maskSelect',
    slot: 6,
    nameKey: 'tools.name.maskSelect',
    hintKey: 'tools.hint.maskSelect',
    params: [
      ...section('mode', [
        choice('mode', 'tools.param.mode', [
          { value: 'add', labelKey: 'tools.option.maskAdd', icon: 'mask-add' },
          { value: 'remove', labelKey: 'tools.option.maskRemove', icon: 'mask-remove' },
        ]),
      ]),
      // A round brush for the edge of a balloon's text, a lasso for an
      // irregular area, a rectangle for a caption box. Size means something
      // for the brush alone, so it is there only then.
      ...section('shape', [
        choice('shape', 'tools.param.shape', [
          { value: 'brush', labelKey: 'tools.option.brush', icon: 'brush' },
          { value: 'lasso', labelKey: 'tools.option.lasso', icon: 'shape-lasso' },
          { value: 'rect', labelKey: 'tools.option.rect', icon: 'shape-rect' },
        ]),
        onlyWhen(range('size', 'tools.param.size', 4, 160, 2, 'px'), (values) => (values.shape ?? 'brush') === 'brush'),
      ]),
    ],
    runnable: false,
    shell: 'pill',
  },
]

/** The rail's icon per tool, in `TOOL_SPECS` order. */
export const TOOL_ICONS = /** @type {Record<string, string>} */ ({
  autoClean: 'sparkle',
  brush: 'brush',
  shapes: 'shapes',
  aiMaskBrush: 'wand',
  cloneHeal: 'stamp',
  maskSelect: 'selection',
})

/**
 * @param {string} id
 * @returns {ToolSpec} the Text cleanup spec for an unknown id - the tool shell
 *   always has something to show, and the mismatch is visible rather than blank
 */
export function toolSpec(id) {
  return TOOL_SPECS.find((spec) => spec.id === id) ?? TOOL_SPECS[0]
}

/**
 * The width of the Text cleanup panel, in CSS pixels.
 *
 * A number here rather than only in the stylesheet because two things have to
 * agree on it: `ToolBar.svelte` draws the panel at it (capped by the viewport),
 * and the tests hold it inside the plan's 360 to 400 and inside the smallest
 * window `src-tauri/tauri.conf.json` allows. Wide enough for a three-word
 * segmented row whose longest cell is "Detect & clean" at the chip's 11px.
 */
export const PANEL_WIDTH = 384

/**
 * The shell a tool is drawn in. `pill` for anything this module does not
 * know, which is the shape that cannot outgrow a small window.
 *
 * @param {string} id
 * @returns {'pill'|'panel'}
 */
export function toolShell(id) {
  return TOOL_SPECS.find((spec) => spec.id === id)?.shell ?? 'pill'
}

/**
 * The tools whose gesture on the page is a **drag**, and which therefore need
 * a drawing surface over the sheet.
 *
 * Text cleanup is not here: it runs a queue rather than drawing a gesture.
 */
export const DRAWING_TOOLS = /** @type {const} */ ([
  'brush',
  'shapes',
  'aiMaskBrush',
  'cloneHeal',
  'maskSelect',
])

/**
 * The area a drag draws with this tool and these parameters, or `null` where
 * the drag is a round brush stroke.
 *
 * Shapes is always an area, of whichever shape its row names. The selection
 * tool is a brush or an area by its own `shape` row: `brush` strokes, and
 * `lasso` and `rect` draw what Shapes draws under the same names, through the
 * same gestures. Every other drawing tool strokes.
 *
 * @param {string} tool
 * @param {Record<string, unknown>} [params]
 * @returns {'rect'|'ellipse'|'lasso'|'polygon'|'line'|null}
 */
export function draftShape(tool, params = {}) {
  if (tool === 'shapes') return /** @type {any} */ (String(params?.shape ?? 'rect'))
  if (tool === 'maskSelect') {
    const shape = params?.shape
    return shape === 'lasso' || shape === 'rect' ? shape : null
  }
  return null
}

/**
 * @param {string} id
 * @returns {boolean}
 */
export function isDrawingTool(id) {
  return DRAWING_TOOLS.includes(/** @type {any} */ (id))
}

/**
 * Would running this tool with these parameters send anything to the cloud?
 *
 * Derived from the specs' own `cloud` flags rather than from a second list of
 * tool-and-parameter names: the option that is disabled in the tool bar when
 * `session.cloudAllowed` is false is exactly the option that must not be sent,
 * and one source for both is what keeps them from drifting apart.
 *
 * @param {string} id
 * @param {Record<string, unknown>} [params]
 * @returns {boolean}
 */
export function toolSpendsCloud(id, params = {}) {
  // Retained only for replaying existing cloud operation intents.
  if (id === 'contentAwareFill') return params.engine === 'cloud'
  const spec = TOOL_SPECS.find((candidate) => candidate.id === id)
  if (!spec) return false
  return spec.params.some(
    (param) =>
      param.kind === 'choice' &&
      param.options.some((option) => option.cloud && option.value === params[param.key]),
  )
}

/**
 * Compute the roving tab stop index for a segmented control.
 *
 * The selected option holds the tab stop only if it is enabled. When nothing is
 * selected or the selected option is disabled, the first enabled option holds the
 * tab stop so the control remains reachable via Tab.
 *
 * @param {Array<{disabled?: boolean}>} items
 * @param {number} selected
 * @returns {number}
 */
export function segmentedTabStop(items, selected) {
  if (selected >= 0 && !items[selected]?.disabled) return selected
  return items.findIndex((o) => !o.disabled)
}

/**
 * Compute the effective value for a choice parameter given available options.
 *
 * When an engine or mode is filtered out (e.g. after a sidecar or model download
 * is removed), the stored value may point to an option that no longer exists in
 * the UI. In that case, fallback to the first available option. If the stored
 * value is still available, keep it. If no options are available, keep the stored
 * value (or parameter default).
 *
 * @param {ChoiceParam} [param]
 * @param {Array<string | {value: string}>} [availableOptions]
 * @param {unknown} [storedValue]
 * @returns {string}
 */
export function effectiveChoice(param, availableOptions = [], storedValue) {
  const fallback = param?.options?.[0]?.value ?? ''
  const current = storedValue !== undefined && storedValue !== null ? String(storedValue) : fallback
  if (!availableOptions || availableOptions.length === 0) {
    return current
  }
  const match = availableOptions.find((opt) =>
    typeof opt === 'string' ? opt === current : opt?.value === current,
  )
  if (match) {
    return typeof match === 'string' ? match : match.value
  }
  const first = availableOptions[0]
  return typeof first === 'string' ? first : (first?.value ?? current)
}

/**
 * Validate and format hex color text during user input.
 *
 * Commits only when the text is exactly 6 hex digits (with optional leading #).
 * Partial inputs and 3-digit shorthands return null while typing so the user can
 * finish typing 6-digit hex values without premature expansion or overwrites.
 *
 * @param {string} text
 * @returns {string|null}
 */
export function hexOnInput(text) {
  const raw = String(text ?? '').trim().replace(/^#/, '')
  return /^[0-9a-f]{6}$/i.test(raw) ? `#${raw.toLowerCase()}` : null
}

/**
 * Validate and format hex color text when the input commits (blur or Enter).
 *
 * Accepts 6-digit hex colors as well as 3-digit shorthand (expanding #abc to
 * #aabbcc). Incomplete or invalid text returns null.
 *
 * @param {string} text
 * @returns {string|null}
 */
export function hexOnCommit(text) {
  const raw = String(text ?? '').trim().replace(/^#/, '')
  if (/^[0-9a-f]{6}$/i.test(raw)) {
    return `#${raw.toLowerCase()}`
  }
  if (/^[0-9a-f]{3}$/i.test(raw)) {
    return `#${[...raw].map((digit) => digit + digit).join('').toLowerCase()}`
  }
  return null
}

/**
 * Is the text in the hex field something that will never become a colour?
 *
 * The field used to commit silently or not at all: `zzz` and `#ab` both wrote
 * nothing and both looked exactly like a value that had been accepted. This
 * is the question the row asks to decide
 * whether to say so - `aria-invalid` on the field and a muted line under it.
 *
 * **A half-typed value is not wrong.** `#ab` on the way to `#abc` answers
 * true, because it is not a colour *yet* and the row's note is what tells the
 * user the swatch has not moved; the note goes as soon as the third digit
 * lands. An **empty** field is not wrong either way: nothing has been typed,
 * so there is nothing to reject, and the swatch still shows the colour that is
 * in force.
 *
 * @param {string} text
 * @returns {boolean}
 */
export function hexInvalid(text) {
  const raw = String(text ?? '').trim()
  if (raw === '') return false
  return hexOnCommit(raw) === null
}
