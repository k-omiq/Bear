<script>
  /**
   * The tool shell: whichever tool the rail has selected, its parameters as
   * controls, and - for Text cleanup - everything a run is started with.
   *
   * **Two shapes, one shell.** Every drawing tool gets a **pill**: one row,
   * forty-four pixels tall, `width: max-content`, as long as the tool needs it
   * to be. Text cleanup (internally still `autoClean`) gets a compact **panel**:
   * `PANEL_WIDTH` wide, a header with the name and the grip, and one labelled
   * row per run choice. Which one a tool gets is its spec's `shell`
   * (`tools.js`). The pill could not hold a run's choices: they were squeezed
   * into a popover behind a trigger reading the step, which left Detect on,
   * Clean on and the text choices two presses from the button they decide.
   *
   * **Nothing stores the size.** The shell measures what the browser made of
   * it - both dimensions, since the panel is as tall as its rows - and writes
   * that back through `setWindowBox`, because `clampPosition` holds the whole
   * shell on screen and needs truthful numbers to do it with. The position the
   * user chose is kept as an **anchor**: a panel pushed up and left to fit near
   * the bottom-right corner goes back to where the pill was when the pill comes
   * back, instead of leaving the pill wherever the panel had to be.
   *
   * **The switch is animated, once.** When the tool changes, the shell's width,
   * height and corner radius run from what they were to what the new content
   * measures, the content fades in, and the position slides to its clamped
   * place with them; then the shell is back to sizing itself. Under
   * `prefers-reduced-motion` the swap is instant. Focus that was inside the
   * old content, and would be dropped on `<body>` with it, goes to the new
   * header's name. Any popover open in the old content goes with it.
   *
   * **Everything that can be a picture is one, on the pill.** A choice whose
   * options all carry an icon is a group of icon cells - four shapes, aligned
   * against non-aligned, clone against heal - and a choice whose options are
   * names is a dropdown showing the current one. What is left is Size, which is
   * inline because it is the control a hand reaches for constantly, the
   * colour's swatch, and *Adjustments*: every other slider and the colour's hex
   * field, one press away in a popover. A bar with six sliders on it would not
   * be a bar. The panel has room for words, so its choices are words.
   *
   * **A `<section>` with a name, not a `role="toolbar"`.** A toolbar promises a
   * single tab stop with the arrow keys moving between its controls, and the
   * arrows here already belong to the radio groups and the sliders inside it -
   * claiming the role would describe a keyboard model this does not implement.
   * A labelled region is what it actually is, and it is named by the tool's own
   * name on screen (`aria-labelledby`) rather than by a second copy of it.
   *
   * **Labels and controls, no prose.** The sentences here are not
   * explanations of a tool but reasons a control is unavailable - a blocked
   * option's gate note and the run's own - and each appears only while
   * something is blocked. Every cloud option is gated by `cloudUsable()`, the
   * permission and a configured endpoint together: a blocked rung is shown
   * disabled and says why, with the way to Settings > Cloud beside it, rather
   * than being hidden, so the user can see what the setting is costing them.
   *
   * The run lives here because the cancel belongs where the run was started.
   * The Pages window shows the progress too and grows no second cancel.
   *
   * **Text cleanup is the single home of a run's choices.** Mode, scope,
   * *Detect on*, *Clean on*, which text is taken (`session.textPolicy`) and
   * what happens outside bubbles, and the two per-run engine picks behind a
   * disclosure. *Detect on* and *Clean on* are not tool parameters but the
   * settings the native run reads (`analysisTargets`, `cleanTarget`), so they
   * are drawn from `state/cloudtargets.svelte.js` rather than from the spec,
   * and a Cloud GPU entry that cannot be chosen is disabled with its reason.
   * A row that cannot change what the chosen mode does is absent, not
   * disabled: Detect cleans nothing, Clean detects nothing, the all-text
   * policy cleans outside bubbles regardless, and a cloud clean reads no local
   * engine pick.
   */
  import { untrack } from 'svelte'
  import { session, raiseWindow, setWindowBox, setTextPolicy, setCleanLocalFirst } from '../state/session.svelte.js'
  import { capabilities, currentWorkflowMissing } from '../state/capabilities.svelte.js'
  import { cloud, cloudEntries, cloudUsable, cloudCleanAvailable, openCloudSettings, pickCloudChoice } from '../state/cloud.svelte.js'
  import {
    chooseCleanTarget,
    chooseDetectTarget,
    cleanReasonKey,
    detectReasonKey,
    detectTarget,
    syncCloudOffer,
    targets,
  } from '../state/cloudtargets.svelte.js'
  import { notify } from '../state/app.svelte.js'
  import {
    applyDetectionPadding,
    editor,
    setToolParam,
    cancelRun,
    currentPage,
    runningPage,
  } from '../state/editor.svelte.js'
  import {
    PANEL_WIDTH,
    TOOL_ICONS,
    activeParams,
    effectiveChoice,
    paramGroups,
    toolSpec,
  } from './tools.js'
  import { windowGesture, closeWindowFocusing } from './windowgesture.js'
  import { MIN_TOP, TOOL_BOTTOM_RESERVE } from '../model/windows.js'
  import { cleanLocalFirst, cloudParts, cloudScopeRefusal, runStep, startCleanRun } from './cloudrun.js'
  import {
    Button,
    ColorPicker,
    IconButton,
    Menu,
    Popover,
    Segmented,
    Select,
    Slider,
  } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import { t } from '../i18n/index.js'
  import { cloudProfileLabel, currentCloudModelId, engineModelLabel } from '../model/model-names.js'
  import { model as pipelineModel } from '../model/pipelines.js'
  import { openSettings } from '../dialogs/settingslinks.js'

  const spec = $derived(toolSpec(editor.tool))
  const values = $derived(editor.toolParams[spec.id] ?? {})
  const win = $derived(session.windows.tool)
  const rank = $derived(session.stacking.tool ?? 1)
  const isPanel = $derived(spec.shell === 'panel')

  const uid = $props.id()
  const titleId = `${uid}-name`
  const blockedId = `${uid}-blocked`

  /**
   * The pill's controls, in their groups.
   *
   * A parameter may be live only under another parameter's value - the Shape's
   * colour and opacity while it is a solid fill rather than an engine -
   * and those controls are absent rather than disabled: nothing is blocked, so
   * there is nothing to explain (`tools.js#activeParams`). The groups are the
   * spec's own (`tools.js#section`); this component draws whatever it is handed
   * and decides no grouping of its own.
   *
   * What it *does* decide is which side of the Adjustments button each
   * parameter falls on, and that is the one rule the pill owns: `size` and the
   * colour's swatch stay out on the bar, and every other slider goes behind the
   * button. A group left with nothing on the bar draws no
   * hairline of its own.
   */
  const layout = $derived.by(() => {
    /** @type {Array<{key: string|null, params: Array<any>}>} */
    const groups = []
    /** @type {Array<import('./tools.js').RangeParam>} */
    const behind = []

    for (const group of paramGroups(spec, values)) {
      /** @type {Array<any>} */
      const onBar = []
      for (const param of group.params) {
        if (param.kind === 'range' && param.key !== 'size') behind.push(param)
        else onBar.push(param)
      }
      if (onBar.length) groups.push({ key: group.key, params: onBar })
    }
    return { groups, behind }
  })

  /** Whether the Adjustments button has anything to open. */
  const adjustable = $derived(layout.behind.length > 0)

  /**
   * A choice is a group of icon cells when **every** option has a glyph, and a
   * dropdown otherwise. Never a mixture: a row of cells where some are pictures
   * and some are words has no rhythm, and the spec is written so the question
   * never arises (`tools.js#ChoiceOption`).
   *
   * @param {import('./tools.js').ChoiceParam} param
   */
  function iconChoice(param) {
    return param.options.every((option) => !!option.icon)
  }

  /**
   * @param {import('./tools.js').ChoiceParam} param
   */
  function optionsFor(param) {
    return param.options
      // A cloud option is shown disabled and says why - the user should be able
      // to see what the setting is costing them. A **missing engine** is the
      // opposite case and §4a's own rule: rung 3a needs a sidecar the
      // application ships no Python for, and rungs 2 and 3 need weights that
      // are downloaded after install. On a machine
      // without them there is no control worth drawing - the remedy is one
      // press in Settings › Models, and a disabled option here would be an
      // explanation in the wrong window. Hidden rather than disabled.
      .filter((option) => option.value === values[param.key] || option === param.options[0] ||
        !option.engine || capabilities.engines[option.engine] !== false)
      .flatMap((option) => {
        const entry = {
          value: option.value,
          label: option.cloud
            ? engineModelLabel('cloud', currentCloudModelId(cloud), t(option.labelKey))
            : option.value === 'flux'
              ? engineModelLabel('flux', session.fluxModel || 'flux2-klein-4b', t(option.labelKey))
              : t(option.labelKey),
          icon: option.icon,
          disabled: (option.cloud && (!cloudUsable() || !cloudCleanAvailable())) || (option.engine && capabilities.engines[option.engine] === false),
          title: option.cloud ? cloudNote() ?? undefined :
            option.engine && capabilities.engines[option.engine] === false ? t('tools.option.engineMissing') : undefined,
        }
        // Every cloud profile's model at once: picking another one makes its
        // profile the default (`set`).
        const profiles = option.cloud ? cloudEntries() : null
        if (!profiles) return [entry]
        return profiles.map(({ value, choice }) => ({
          ...entry,
          value,
          label: choice ? cloudProfileLabel(choice.modelId, choice.name) : entry.label,
          // Another profile can be picked even while the default cannot run.
          disabled: value === 'cloud' ? entry.disabled : !cloudCleanAvailable(choice),
        }))
      })
  }

  /**
   * Why the Cloud engine cannot be chosen now, or null when it can. Nothing is
   * said about readiness before it has been read once, so the note never
   * claims an endpoint is missing that is merely not asked about yet.
   *
   * @returns {string|null}
   */
  function cloudNote() {
    if (!session.cloudAllowed) return t('editor.state.cloudBlocked')
    if (!cloudCleanAvailable()) return t('cloud.configuration.cleanMissing')
    if (cloudUsable() || !cloud.checked) return null
    return t('tools.option.engineCloudNotReady')
  }

  /**
   * Why a control has an option nobody can choose, as text.
   *
   * The reason used to live only in a `title` on the disabled option, where it
   * reached no one: browsers fire no hover events on a disabled form control,
   * so that tooltip never appears. §7.6's point is that the user should be able
   * to see what the setting is costing them, and a note that is only there
   * while something is blocked is how they see it. A dropdown carries it inside
   * itself, under the items (`ui/Menu.svelte`'s `note`); an icon group and a
   * panel row have no inside, so they carry it beside themselves.
   *
   * @param {import('./tools.js').ChoiceParam} param
   * @returns {string|null}
   */
  function gateNote(param) {
    if (param.options.some((option) => option.value === values[param.key] && option.engine &&
      capabilities.engines[option.engine] === false)) return t('tools.option.engineMissing')
    const gated = param.options.some((option) => option.cloud) && cloudNote()
    if (gated) return gated
    // Rung 3a's half of the same duty. `sidecarReasonKey` is null for the
    // ordinary machine that installed nothing - absence is silent - and carries
    // a `decline.reason.sidecar*` sentence only where something *is* installed
    // and this machine still will not carry it. That sentence had no surface at
    // all until the tool window rendered it.
    const withheld =
      param.options.some((option) => option.sidecar) &&
      !capabilities.sidecar &&
      capabilities.sidecarReasonKey
    return withheld ? t(capabilities.sidecarReasonKey) : null
  }

  // When an engine option is filtered out (missing sidecar or weights), sync
  // the effective value back into editor.toolParams so the store and the
  // control agree rather than sending a stale rejected rung across the seam.
  // Reading values and calling set are untracked so the effect does not read
  // and write the same state or re-trigger on unrelated parameter changes.
  $effect(() => {
    const currentValues = untrack(() => values)
    for (const group of paramGroups(spec, currentValues)) {
      for (const param of group.params) {
        if (param.kind === 'choice') {
          const opts = optionsFor(param)
          if (opts.length === 0) continue
          const raw = currentValues[param.key]
          const effective = effectiveChoice(param, opts, raw)
          if (raw !== undefined ? raw !== effective : param.options[0]?.value !== effective) {
            untrack(() => set(param.key, effective))
          }
        }
      }
    }
  })

  /* ---------------------------------------------------------------- */
  /* Text cleanup: the run's choices                                   */
  /* ---------------------------------------------------------------- */

  /** The panel's parameters by key, live ones only. */
  const param = $derived(
    /** @type {Record<string, any>} */ (Object.fromEntries(activeParams(spec, values).map((entry) => [entry.key, entry]))),
  )

  const running = $derived(editor.run.active)
  const page = $derived(currentPage())

  /** The step the run takes, which the mode row holds and the button names. */
  const step = $derived(runStep(values))

  /** Which halves of this step go to the cloud GPU. */
  const parts = $derived(spec.runnable ? cloudParts(step) : { detect: false, clean: false })

  /**
   * Whether a project-wide run can start with these choices. The native run
   * covers every chapter of the project on this computer, and refuses one with
   * a cloud half: a single consent cannot cover chapters nobody looked at
   * (`cloudrun.js#cloudScopeRefusal`, `run.rs#cloud_refusal`). So Project is
   * offered only while nothing goes to the cloud.
   */
  const projectRuns = $derived(!parts.detect && !parts.clean)

  /**
   * The scope the run covers. A stored Project that cannot run now reads as
   * Chapter, and the effect below writes Chapter back as soon as the panel
   * shows it: a Project kept aside would come back unasked the moment the
   * cloud half went away, and the next run would quietly cover every chapter
   * of the project. Project is an explicit pick each time it can run.
   *
   * @type {'page'|'chapter'|'project'}
   */
  const scope = $derived.by(() => {
    const stored = values.scope
    if (stored === 'project') return projectRuns ? 'project' : 'chapter'
    return stored === 'chapter' ? 'chapter' : 'page'
  })

  $effect(() => {
    if (isPanel && values.scope === 'project' && !projectRuns) untrack(() => set('scope', 'chapter'))
  })

  const scopeOptions = $derived(
    (param.scope?.options ?? [])
      .filter((/** @type {any} */ option) => option.value !== 'project' || projectRuns)
      .map((/** @type {any} */ option) => ({ value: option.value, label: t(option.labelKey) })),
  )

  /**
   * The mode row reads the two halves, then both. The spec lists Detect &
   * clean first because a choice's first option is its default, and the
   * default run is still both halves.
   */
  const STEP_ORDER = ['detect', 'clean', 'auto']
  const stepOptions = $derived(
    [...(param.step?.options ?? [])]
      .sort((/** @type {any} */ a, /** @type {any} */ b) => STEP_ORDER.indexOf(a.value) - STEP_ORDER.indexOf(b.value))
      .map((/** @type {any} */ option) => ({ value: option.value, label: t(option.labelKey) })),
  )

  /** Which text a run takes, `session.textPolicy`. */
  const allText = $derived(session.textPolicy === 'all_text')
  const policyOptions = $derived([
    { value: 'legacy_gate', label: t('tools.option.policyLegacy') },
    { value: 'all_text', label: t('tools.option.policyAll') },
  ])

  /**
   * What the button says: the step and the scope, "Detect chapter", "Clean
   * page". One literal key per pair rather than two words glued together, so
   * a language is free to order them its own way.
   */
  const RUN_LABELS = /** @type {const} */ ({
    auto: { page: 'editor.action.run.autoPage', chapter: 'editor.action.run.autoChapter', project: 'editor.action.run.autoProject' },
    detect: { page: 'editor.action.run.detectPage', chapter: 'editor.action.run.detectChapter', project: 'editor.action.run.detectProject' },
    clean: { page: 'editor.action.run.cleanPage', chapter: 'editor.action.run.cleanChapter', project: 'editor.action.run.cleanProject' },
  })

  const actionLabel = $derived(running ? t('editor.action.cancelRun') : t(RUN_LABELS[step][scope]))

  /**
   * The line beside Cancel while a run is going, in a live region.
   *
   * It names the page the **run** is on, not the page on screen. Reading
   * `currentPage()` here froze the line at "Cleaning page 8" for the whole of a
   * twenty-page run, because the user stays on page 8 while the queue moves.
   * `runningPage()` is null for the moment between starting and the first
   * `page-started`, and the viewed page is the honest answer then. A Detect
   * run changes no pixel, so it says it is detecting (`editor.run.mode`).
   */
  const status = $derived.by(() => {
    if (!running) return ''
    const onIt = runningPage() ?? page
    const key = editor.run.mode === 'detect' ? 'editor.status.detecting' : 'editor.status.cleaning'
    return t(key, { page: onIt ? onIt.number : editor.pageIndex + 1 })
  })

  /** The run's progress, for the bar beside Cancel. Null before the queue is known. */
  const progress = $derived.by(() => {
    const total = Number(editor.run.queued) || 0
    if (!running || total <= 0) return null
    const done = Math.min(total, Math.max(0, Number(editor.run.pagesDone) || 0))
    return { done, total, percent: Math.round((done / total) * 100) }
  })

  /**
   * Text cleanup's own precondition, which is not an engine choice.
   *
   * The detector, the balloon detector and the script gate are three files the
   * run opens before it looks at a page, and without them `runClean` cannot
   * start at all - `src-tauri/src/run.rs` answers a `notice.run.modelsMissing`
   * and queues nothing. So the button is **disabled with a note** rather than
   * hidden, which is the opposite of what an engine option gets and is right
   * for the same reason: an engine has four alternatives beside it and the run
   * has none, so a tool that quietly lost its only action would read as a
   * broken panel rather than as a machine missing a download.
   *
   * The note names the detection models that are missing, never "the
   * models": a clean opens no detector, so LaMa being here says nothing about
   * whether detection can run. Where a cloud run would have everything it
   * needs here (CTD and the text reader, `pipelines.js#runDetection`), it
   * says so, and the way out is one press on Detect on.
   */
  const missingHere = $derived(spec.runnable && step !== 'clean' ? currentWorkflowMissing() : [])
  const missingNames = $derived((missingHere ?? []).map((id) => pipelineModel(id)?.product ?? id).join(', '))
  const missingOnCloud = $derived(Boolean(missingHere?.length) && detectOn === 'local' && !detectReason &&
    currentWorkflowMissing({ analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } })?.length === 0)
  const blockedKey = $derived.by(() => {
    if (!spec.runnable) return null
    // Clean alone loads no detector, and on the cloud GPU no local engine at
    // all: what it needs is the regions already stored, which the run itself
    // says when there are none (`notice.run.nothingDetected`).
    const cloudOnly = step === 'clean' && session.cleanTarget === 'cloud'
    if (!cloudOnly && !capabilities.runtime) return 'editor.state.runtimeMissing'
    if (missingHere === null) return 'editor.state.modelsMissing'
    if (missingHere.length) return missingOnCloud ? 'editor.state.detectModelsCloud' : 'editor.state.detectModelsMissing'
    return cloudGateKey
  })
  const blockedText = $derived(blockedKey ? t(blockedKey, { models: missingNames }) : '')

  /**
   * A half of the run routed to the cloud GPU is refused where it cannot run,
   * with the reason, rather than started to fail: a long strip, a cloud GPU
   * that is off or not set up. Readiness is not judged before it has been
   * read once. Detection's reason leads, since it is the half that would have
   * run first. A project cannot be chosen with a cloud half at all, so it is
   * not among these.
   */
  const cloudGateKey = $derived.by(() => {
    const refusal = cloudScopeRefusal(scope, parts)
    if (refusal) return refusal
    if (!parts.detect && !parts.clean) return null
    if (!session.cloudAllowed || (cloud.checked && !cloudUsable())) return parts.detect ? 'cloud.analysis.run.gate' : 'cloud.clean.gate'
    return null
  })

  /**
   * Detect on and Clean on, as the dropdowns draw them. The current place
   * stays selectable even when it could not be chosen now - Cloud GPU after
   * the endpoint went away - so the control never shows a place it is not.
   */
  const detectReason = $derived(spec.runnable ? detectReasonKey() : null)
  const cleanReason = $derived(spec.runnable ? cleanReasonKey() : null)
  const detectOn = $derived(detectTarget())
  const cleanOn = $derived(session.cleanTarget === 'cloud' ? 'cloud' : 'local')

  /**
   * Cloud engines are off and nothing is routed to them: one reason covers
   * both places, so it is said once under them rather than once per row. This
   * is where a fresh install starts, and two notes and two buttons saying the
   * same thing were the tallest part of the panel. A place already on the
   * cloud that can no longer run there is worth its own row's note.
   */
  const cloudOff = $derived(!session.cloudAllowed && detectOn !== 'cloud' && cleanOn !== 'cloud')
  const cloudOffId = `${uid}-cloud-off`

  /**
   * Detection on the cloud GPU uses its own fixed combination, whatever this
   * computer's selection holds (`pipelines.js#runDetection`), so the Detect
   * on row says which models that is and where each runs.
   */
  const cloudCombo = $derived(spec.runnable && step !== 'clean' && detectOn === 'cloud')
  const cloudComboId = `${uid}-cloud-combo`

  /**
   * When the per-run picks decide anything: whenever the run cleans. A clean
   * starts every region from them, including regions detected earlier
   * (`run.rs`, the stored-detection clean), and a cloud clean saves them onto
   * its regions before it plans them, so a LaMa pick is cleaned here. Detect
   * cleans nothing, so the rows are drawn for Clean and for Detect & clean,
   * never for Detect. A pick that is hidden must not be one that decides the
   * outcome.
   */
  const picksShown = $derived(step !== 'detect')
  /**
   * The fill colour is read by a clean on this computer, and paints a saved
   * Solid pick that holds no balloon tone as well as a fresh one, so it is
   * drawn wherever such a clean happens, whatever the rows say now.
   */
  const colorRead = $derived(step !== 'detect' && cleanOn === 'local')
  /**
   * The explicit mixed choice for a cloud clean (`cloudrun.js#cleanLocalFirst`):
   * saved Fill and Solid picks are cleaned here first, the rest on the cloud
   * GPU. Off unless ticked, and offered only while the clean goes there.
   */
  const localFirst = $derived(cleanLocalFirst(values))
  const mixedHintId = `${uid}-mixed-hint`

  /**
   * Where the picks send each region. Here, every region starts from them.
   * With the cloud GPU cleaning, LaMa picks stay here and the rest are sent,
   * Fill and Solid colour tried here first under the mixed choice.
   */
  const picksNoteKey = $derived(
    cleanOn === 'local' ? 'tools.picks.local' : localFirst ? 'tools.picks.mixed' : 'tools.picks.cloud',
  )
  const picksNoteId = `${uid}-picks-note`

  /** Whether the engine rows are shown. Collapsed at first, kept across tools. */
  let advancedOpen = $state(false)

  /** The two engine picks, for the collapsed group's summary. */
  const enginePicks = $derived.by(() => {
    /** @type {string[]} */
    const picks = []
    for (const key of ['bubbleEngine', 'outsideEngine']) {
      const entry = param[key]
      if (!entry) continue
      const opts = optionsFor(entry)
      const value = effectiveChoice(entry, opts, values[key])
      picks.push(opts.find((option) => option.value === value)?.label ?? value)
    }
    return picks.join(' · ')
  })

  /**
   * @param {'detect'|'clean'} key
   * @param {'local'|'cloud'} current
   * @param {string|null} reasonKey
   */
  function placeOptions(key, current, reasonKey) {
    const cloudEntry = { value: 'cloud', label: t('tools.option.onCloud'), disabled: Boolean(reasonKey) && current !== 'cloud' }
    // Every cloud profile at once, Clean on by the model it cleans with:
    // picking another makes its profile the default (`placeOn`). The reason
    // a row gives is the default's; another profile may not share it.
    const profiles = cloudEntries()
    return [
      { value: 'local', label: t('tools.option.onLocal') },
      ...(profiles
        ? profiles.map(({ value, choice }) => ({
          value,
          label: !choice ? cloudEntry.label
            : key === 'clean' ? cloudProfileLabel(choice.modelId, choice.name)
              : `${cloudEntry.label} · ${choice.name}`,
          disabled: value === 'cloud' ? cloudEntry.disabled : key === 'clean' && !cloudCleanAvailable(choice),
        }))
        : [cloudEntry]),
    ]
  }

  /**
   * @param {'local'|'cloud'} current
   * @param {string|null} reasonKey
   */
  function placeNote(current, reasonKey) {
    if (!reasonKey) return null
    return current === 'cloud' ? `${t('tools.target.stranded')} ${t(reasonKey)}` : t(reasonKey)
  }

  /**
   * Where a row's pick runs: another cloud profile's entry switches the
   * default first, and the row then holds Cloud. A switch that fails leaves
   * the row where it was, with its notice.
   *
   * @param {string} target
   * @returns {Promise<string|null>}
   */
  async function placeOn(target) {
    if (!target.startsWith('cloud@')) return target
    return (await pickCloudChoice(target)) ? 'cloud' : null
  }

  /** @param {string} picked */
  async function setDetectOn(picked) {
    const target = await placeOn(picked)
    if (!target) return
    if (!(await chooseDetectTarget(target)) && targets.detectSaveFailed) {
      notify({ key: 'tools.target.detectSaveFailed', tone: 'warn' })
    }
  }

  /** @param {string} picked */
  async function setCleanOn(picked) {
    const target = await placeOn(picked)
    if (!target) return
    if (!(await chooseCleanTarget(target)) && targets.cleanSaveFailed) {
      notify({ key: 'tools.target.cleanSaveFailed', tone: 'warn' })
    }
  }

  /** @param {string} policy */
  function choosePolicy(policy) {
    if (policy === 'legacy_gate' || policy === 'all_text') setTextPolicy(policy)
  }

  // Detect on's Cloud GPU entry needs to know what the endpoint offers, which
  // is asked once per endpoint and shared with Settings.
  $effect(() => {
    if (spec.runnable) syncCloudOffer()
  })

  /**
   * Mask padding's Apply: the masks already detected, over the page or, for
   * Chapter and Project, the open chapter, re-padded to the slider's value.
   * Held off while a run or another apply is going, so two never race.
   */
  let applyingPadding = $state(false)
  const paddingHintId = `${uid}-padding-hint`
  async function applyPadding() {
    if (applyingPadding || running) return
    applyingPadding = true
    try {
      await applyDetectionPadding(scope === 'page' ? 'page' : 'chapter')
    } finally {
      applyingPadding = false
    }
  }

  /**
   * @param {string} key
   * @param {unknown} value
   */
  function set(key, value) {
    if (spec.id === 'autoClean' && key === 'localFirst') setCleanLocalFirst(value === true)
    // Another cloud profile's model: switch the default, then store `cloud`.
    // A switch that fails leaves the pick where it was, with its notice.
    if (typeof value === 'string' && value.startsWith('cloud@')) {
      const toolId = spec.id
      void pickCloudChoice(value).then((switched) => { if (switched) setToolParam(toolId, key, 'cloud') })
      return
    }
    setToolParam(spec.id, key, value)
  }

  /**
   * How long after a run ends a press on the action is still read as the
   * Cancel it was showing. `run-finished` can land between the last frame the
   * user saw and their click, and the same button then says Start: without
   * this, a press aimed at Cancel would begin a whole new run.
   */
  const RUN_END_GRACE_MS = 500
  let runEndedAt = -Infinity
  let wasRunning = untrack(() => running)
  $effect.pre(() => {
    const now = running
    if (wasRunning && !now) runEndedAt = globalThis.performance?.now() ?? Date.now()
    wasRunning = now
  })

  function run() {
    if (running) cancelRun()
    else if ((globalThis.performance?.now() ?? Date.now()) - runEndedAt >= RUN_END_GRACE_MS) startCleanRun(scope)
  }

  /* ---------------------------------------------------------------- */
  /* The shell: position, size and the switch between shapes           */
  /* ---------------------------------------------------------------- */

  /** @type {HTMLElement|undefined} */
  let root = $state()

  const { onGestureStart, onGestureMove, onGestureEnd, onGestureKey, destroy: endGesture } = windowGesture({
    id: () => 'tool',
    measuredHeight: () => root?.offsetHeight ?? 44,
  })

  /**
   * Where the user put the shell, which a size change grows from.
   *
   * `setWindowBox` holds the shell on screen, and a panel near the bottom-right
   * corner is pushed up and left to fit. That push is not a decision of the
   * user's, so it is not kept: the next size is placed from here again, and a
   * pill that comes back goes back to where it was. Anything else that moved
   * the shell - a drag, the arrow keys, Escape, a smaller window - is a new
   * anchor, told apart from our own write by comparing against `placed`.
   */
  let anchor = { x: 0, y: 0 }
  /** @type {{x: number, y: number}|null} */
  let placed = null
  let lastViewport = { w: globalThis.innerWidth, h: globalThis.innerHeight }

  /** @param {{w: number, h: number|null}} size */
  function place(size) {
    const current = session.windows.tool
    if (!current) return
    const viewport = { w: globalThis.innerWidth, h: globalThis.innerHeight }
    const resized = viewport.w !== lastViewport.w || viewport.h !== lastViewport.h
    if (!placed || (!resized && (current.x !== placed.x || current.y !== placed.y))) {
      anchor = { x: current.x, y: current.y }
    }
    lastViewport = viewport
    setWindowBox('tool', { x: anchor.x, y: anchor.y, w: size.w, h: size.h })
    placed = { x: current.x, y: current.y }
  }

  /**
   * The whole pill is the drag surface, and its controls sit on it. A
   * pointerdown that reaches a control is that control's - a gesture would
   * capture the pointer and `preventDefault`, which between them can stop a
   * button's `onclick` firing at all, because Pointer Events L3 retargets the
   * click to the capture target. The grip is the exception: it starts the
   * gesture itself, before this ever sees the event.
   *
   * The panel is dragged by its header alone. Its body is rows of controls
   * with gaps between them, and a press that just missed a control would
   * otherwise carry the whole panel off.
   *
   * A control's `label` and its `output` are the control for this purpose:
   * pressing a slider's label is how a pointer focuses that slider, and a
   * gesture started on the word would swallow the transfer along with the
   * click.
   *
   * @param {PointerEvent & {currentTarget: HTMLElement}} event
   */
  function onBarPointerDown(event) {
    raiseWindow('tool')
    const target = /** @type {Element|null} */ (event.target)
    if (isPanel && !target?.closest?.('[data-shell-head]')) return
    if (
      target?.closest?.(
        'button, input, select, textarea, label, output, [role="radiogroup"], [role="dialog"], .menu',
      )
    ) {
      return
    }
    onGestureStart(event, 'move')
  }

  /**
   * The panel's rows are taller than the window has room for. They scroll
   * then, between a header and an action that stay put, so the run button is
   * never below the edge. Only then: a scrolling box clips what it holds, and
   * the colour picker opens out of it.
   *
   * Decided from the rows' own content height, which is the same whether they
   * are scrolling or not, so turning it on cannot be what turns it off.
   */
  let tight = $state(false)

  /** @param {HTMLElement} node */
  function checkTight(node) {
    const rows = /** @type {HTMLElement|null} */ (node.querySelector('.rows'))
    if (!rows) {
      tight = false
      return
    }
    const natural = node.offsetHeight - rows.clientHeight + rows.scrollHeight
    const room = (globalThis.innerHeight || 0) - MIN_TOP - TOOL_BOTTOM_RESERVE
    tight = room > 0 && natural > room
  }

  /** A shell that is changing shape: its own observer's readings are frames of that. */
  let morphing = false
  /** @type {Animation|null} */
  let morphAnimation = null
  /** @type {ReturnType<typeof setTimeout>|null} */
  let morphTimer = null
  /**
   * How long the change of shape takes, and how: `--dur-slow` and `--ease`
   * from `app.css`, written out because the Web Animations API reads no CSS
   * variables.
   */
  const MORPH_MS = 200
  const MORPH_EASE = 'cubic-bezier(.2,.7,.3,1)'

  /**
   * Measure the shell rather than compute it.
   *
   * Its size is whatever the selected tool put in it, in the user's own font,
   * at the user's own zoom - a number this component could only ever guess at
   * and the element already knows. `setWindowBox` is how it reaches
   * `clampPosition`, which is the one thing that needs it: a shell grown near
   * an edge must be held on screen, and it cannot be against a stale size.
   */
  $effect(() => {
    const node = root
    if (!node || typeof ResizeObserver !== 'function') return
    const observer = new ResizeObserver((entries) => {
      if (morphing) return
      // The **border box**, not the content box: `contentRect` stops inside the
      // shell's own padding, so a size taken from it is short of the thing
      // `clampPosition` is placing.
      // Older WebKit hands `borderBoxSize` over as one object rather than as a
      // one-element array, and the same engine is under the shipped app.
      const box = entries[0]?.borderBoxSize
      const size = Array.isArray(box) ? box[0] : box
      const width = size?.inlineSize ?? node.offsetWidth
      const height = size?.blockSize ?? node.offsetHeight
      // A shell being torn down measures 0, and that is not a size to place
      // anything by: keep the last honest answer.
      if (width > 0) place({ w: Math.round(width), h: height > 0 ? Math.round(height) : null })
      checkTight(node)
    })
    observer.observe(node)
    // A shorter window changes nothing the observer can see until it is too
    // late: the panel is already past the edge.
    const onresize = () => {
      place({ w: node.offsetWidth, h: node.offsetHeight || null })
      checkTight(node)
    }
    globalThis.addEventListener?.('resize', onresize)
    return () => {
      observer.disconnect()
      globalThis.removeEventListener?.('resize', onresize)
    }
  })

  $effect(() => () => {
    if (morphTimer !== null) clearTimeout(morphTimer)
    morphAnimation?.cancel()
  })

  /** Whether the user asked the system for less motion. */
  function reducedMotion() {
    try {
      return Boolean(globalThis.matchMedia?.('(prefers-reduced-motion: reduce)')?.matches)
    } catch {
      return false
    }
  }

  /**
   * The tool the shell last showed, and what the shell was like just before
   * it changed: its size and radius to animate from, and whether focus was
   * inside it. Read in `$effect.pre`, which runs before the DOM changes; a
   * shell caught in the middle of a change reads as the frame it is on, so a
   * second switch carries on from there rather than jumping.
   */
  let shownId = untrack(() => spec.id)
  /** @type {{w: number, h: number, x: number, y: number, radius: string, focused: boolean}|null} */
  let before = null

  $effect.pre(() => {
    const id = spec.id
    untrack(() => {
      const node = root
      if (id === shownId || !node) return
      // A drag started on the grip is captured by the grip, and the grip is
      // part of the content about to be replaced: its `pointerup` would reach
      // nothing, and the gesture would stay open and refuse every later drag.
      // It ends here, where the drag has put the shell.
      endGesture()
      // Where it is on screen, not where the store says: in the middle of a
      // change of shape the store already holds the last target, and the
      // animation is somewhere short of it.
      const style = getComputedStyle(node)
      before = {
        w: node.offsetWidth,
        h: node.offsetHeight,
        x: parseFloat(style.left),
        y: parseFloat(style.top),
        radius: style.borderTopLeftRadius,
        focused: node.contains(document.activeElement),
      }
    })
  })

  $effect(() => {
    const id = spec.id
    untrack(() => {
      if (id === shownId) return
      shownId = id
      const was = before
      before = null
      const node = root
      if (!node) return
      // The old content took the focused control with it. The new header is
      // the nearest thing that is the same place: the name of what is here now.
      if (was?.focused && !node.contains(document.activeElement)) {
        /** @type {HTMLElement|null} */ (node.querySelector('[data-shell-title]'))?.focus({ preventScroll: true })
      }
      morph(node, was)
    })
  })

  /**
   * Put the shell into its new shape.
   *
   * The new content is measured at its own size first, and the shell is placed
   * for that size straight away, so the clamp moves it once rather than once
   * a frame. Then, motion allowing, the shell runs from the size, radius and
   * place it had to the new ones, clipped so the larger content cannot spill
   * past an edge that is still growing; the content fades in on its own
   * (`.face`). The last frame is the shell's own new style, so there is
   * nothing to snap when it lands.
   *
   * The Web Animations API rather than a CSS transition because cancelling one
   * is synchronous: a switch in the middle of a switch, or a window that is
   * hidden while it runs and so never renders the frame a transition would
   * end on, leaves no pinned size behind. WebKit has had it since 13.1.
   *
   * @param {HTMLElement} node
   * @param {{w: number, h: number, x: number, y: number, radius: string}|null} was
   */
  function morph(node, was) {
    settle(node)
    const to = { w: node.offsetWidth, h: node.offsetHeight }
    // Nothing laid out (a hidden window, a test document): the observer
    // measures when there is something to measure.
    if (!(to.w > 0)) return
    const win = session.windows.tool
    const from = {
      x: was && Number.isFinite(was.x) ? was.x : (win?.x ?? 0),
      y: was && Number.isFinite(was.y) ? was.y : (win?.y ?? 0),
    }
    place({ w: to.w, h: to.h > 0 ? to.h : null })
    if (!was || !(was.w > 0 && was.h > 0) || reducedMotion() || typeof node.animate !== 'function') return
    const target = { x: win?.x ?? from.x, y: win?.y ?? from.y }
    if (was.w === to.w && was.h === to.h && from.x === target.x && from.y === target.y) return
    const radius = getComputedStyle(node).borderTopLeftRadius
    morphing = true
    node.style.overflow = 'hidden'
    const animation = node.animate(
      [
        { width: `${was.w}px`, height: `${was.h}px`, borderRadius: was.radius, left: `${from.x}px`, top: `${from.y}px` },
        { width: `${to.w}px`, height: `${to.h}px`, borderRadius: radius, left: `${target.x}px`, top: `${target.y}px` },
      ],
      { duration: MORPH_MS, easing: MORPH_EASE },
    )
    morphAnimation = animation
    animation.onfinish = () => land(node)
    // A window that renders no frames finishes nothing: land anyway.
    morphTimer = setTimeout(() => land(node), MORPH_MS + 120)
  }

  /** @param {HTMLElement} node */
  function land(node) {
    if (!morphing) return
    settle(node)
    const w = node.offsetWidth
    const h = node.offsetHeight
    if (w > 0) place({ w, h: h > 0 ? h : null })
    checkTight(node)
  }

  /**
   * Stop a change of shape where it is: the shell sizes itself again. Safe to
   * call at any time, including in the middle of a change that another switch
   * is about to replace.
   *
   * @param {HTMLElement} node
   */
  function settle(node) {
    if (morphTimer !== null) clearTimeout(morphTimer)
    morphTimer = null
    const animation = morphAnimation
    morphAnimation = null
    if (animation) {
      animation.onfinish = null
      animation.cancel()
    }
    if (!morphing) return
    morphing = false
    node.style.overflow = ''
  }
</script>

<!-- A choice drawn as a dropdown on the pill: what the parameter is, in the
     muted ink a label is drawn in, then the value it holds. -->
{#snippet dropdown(labelKey, shortKey, items, current, note, onselect)}
  <Menu label={t(labelKey)} {note} {items} {onselect}>
    {#snippet trigger({ toggle, triggerProps })}
      <button
        type="button"
        class="drop"
        title={note ?? undefined}
        aria-label={t('tools.label.choice', { labelKey, value: current })}
        onclick={toggle}
        {...triggerProps}
      >
        <span class="drop-key">{t(shortKey)}</span>
        <span class="drop-value">{current}</span>
        <Icon name="chevron-down" size={12} />
      </button>
    {/snippet}
  </Menu>
{/snippet}

{#snippet grip()}
  <button
    type="button"
    class="grip"
    title={t('editor.window.move', { windowName: t(spec.nameKey) })}
    aria-label={t('editor.window.move', { windowName: t(spec.nameKey) })}
    onpointerdown={(e) => onGestureStart(e, 'move')}
    onpointermove={onGestureMove}
    onpointerup={onGestureEnd}
    onpointercancel={onGestureEnd}
    onkeydown={(e) => onGestureKey(e, 'move')}
  >
    <Icon name="drag-handle" size={16} />
  </button>
{/snippet}

<!-- The tool's own icon and name, which is also the shell's accessible name.
     The hint is a tooltip on it: a reminder of the gesture is the one thing
     here that a tooltip is honestly enough for. The name takes focus from
     script (never from Tab) when a switch of tool took the focused control
     away with the old content. -->
{#snippet ident()}
  <span class="ident" title={t(spec.hintKey)}>
    <Icon name={TOOL_ICONS[spec.id]} size={16} />
    <h2 class="name" id={titleId} tabindex="-1" data-shell-title>{t(spec.nameKey)}</h2>
  </span>
{/snippet}

{#snippet closeButton()}
  <IconButton
    icon="close"
    label={t('editor.window.close', { windowName: t(spec.nameKey) })}
    size={26}
    iconSize={14}
    onclick={() => closeWindowFocusing('tool')}
  />
{/snippet}

<!-- Where one half of the run happens. A native select, as in Settings, with
     the reason Cloud GPU cannot be chosen under it and describing it, and on
     Detect on, while it is Cloud GPU, the models a cloud run uses. -->
{#snippet placeRow(key, labelKey, current, reasonKey, onchange)}
  {@const noteId = `${uid}-place-${key}-note`}
  {@const combo = key === 'detect' && cloudCombo}
  {@const describedBy = [cloudOff ? cloudOffId : reasonKey ? noteId : null, combo ? cloudComboId : null]
    .filter(Boolean).join(' ')}
  <label class="row-label" for="{uid}-place-{key}">{t(labelKey)}</label>
  <div class="row-control" data-run-place={key}>
    <Select
      id="{uid}-place-{key}"
      options={placeOptions(key, current, reasonKey)}
      value={current}
      describedBy={describedBy || undefined}
      {onchange}
    />
  </div>
  {#if combo}
    <div class="row-note">
      <p class="gate" id={cloudComboId} data-cloud-combo>{t('pipelines.cloudCombo')}</p>
    </div>
  {/if}
  {#if reasonKey && !cloudOff}
    <div class="row-note">
      <p class="gate" id={noteId}>{placeNote(current, reasonKey)}</p>
      <Button size="sm" variant="soft" onclick={openCloudSettings}>
        {t('tools.option.engineCloudSettings')}
      </Button>
    </div>
  {/if}
{/snippet}

<!-- A two-way choice on one row of the panel, as chips that fill the column. -->
{#snippet chipRow(key, labelKey, options, current, onchange)}
  <span class="row-label" id="{uid}-row-{key}">{t(labelKey)}</span>
  <div class="row-control" data-run-row={key}>
    <Segmented {options} value={current} block labelledBy="{uid}-row-{key}" {onchange} />
  </div>
{/snippet}

<!-- One per-run engine pick, as a native select: the panel's rows are its own
     column, and a native popup cannot be clipped by it. -->
{#snippet engineRow(entry)}
  {@const opts = optionsFor(entry)}
  {@const value = effectiveChoice(entry, opts, values[entry.key])}
  {@const note = gateNote(entry)}
  {@const noteId = `${uid}-engine-${entry.key}-note`}
  <label class="row-label" for="{uid}-engine-{entry.key}">{t(entry.labelKey)}</label>
  <div class="row-control">
    <Select
      id="{uid}-engine-{entry.key}"
      options={opts}
      {value}
      describedBy={note ? `${noteId} ${picksNoteId}` : picksNoteId}
      onchange={(next) => set(entry.key, next)}
    />
  </div>
  {#if note}
    <div class="row-note"><p class="gate" id={noteId}>{note}</p></div>
  {/if}
{/snippet}

<!-- The fill colour a clean on this computer paints a Solid pick with. -->
{#snippet colorRow()}
  <span class="row-label">{t('tools.param.bubbleColor')}</span>
  <div class="row-control" data-run-row="bubbleColor">
    <ColorPicker
      value={String(values.bubbleColor ?? '#ffffff')}
      label={t('tools.param.bubbleColor')}
      unclipped={tight}
      onchange={(hex) => set('bubbleColor', hex)}
    />
  </div>
{/snippet}

<!-- Which regions the picks above reach, and when. -->
{#snippet picksNote()}
  <div class="row-note"><p class="gate" id={picksNoteId} data-picks-note>{t(picksNoteKey)}</p></div>
{/snippet}

<section
  bind:this={root}
  class="bar"
  class:panel={isPanel}
  class:tight={isPanel && tight}
  aria-labelledby={titleId}
  style:left="{win.x}px"
  style:top="{win.y}px"
  style:z-index={20 + rank}
  style:--panel-width="{PANEL_WIDTH}px"
  data-shell={isPanel ? 'panel' : 'pill'}
  onpointerdown={onBarPointerDown}
  onpointermove={onGestureMove}
  onpointerup={onGestureEnd}
  onpointercancel={onGestureEnd}
  onfocusin={() => raiseWindow('tool')}
>
  {#key spec.id}
    {#if isPanel}
      <div class="face panel-face">
        <header class="head" data-shell-head>
          {@render grip()}
          {@render ident()}
          <span class="fill"></span>
          {@render closeButton()}
        </header>

        <div class="rows">
          <!-- What the run does and over what: the two choices every run
               makes, as full-width chips with their label above. -->
          <div class="block">
            <span class="block-label" id="{uid}-mode">{t(param.step.labelKey)}</span>
            <Segmented
              options={stepOptions}
              value={step}
              block
              labelledBy="{uid}-mode"
              onchange={(next) => set('step', next)}
            />
          </div>
          <div class="block">
            <span class="block-label" id="{uid}-scope">{t(param.scope.labelKey)}</span>
            <Segmented
              options={scopeOptions}
              value={scope}
              block
              labelledBy="{uid}-scope"
              onchange={(next) => set('scope', next)}
            />
          </div>

          <!-- Where each half runs and which text it takes. Only the rows the
               mode reads: Detect has nothing to clean, Clean nothing to find. -->
          <div class="grid">
            {#if step !== 'clean'}
              {@render placeRow('detect', 'tools.param.detectOn', detectOn, detectReason, setDetectOn)}
            {/if}
            {#if step !== 'detect'}
              {@render placeRow('clean', 'tools.param.cleanOn', cleanOn, cleanReason, setCleanOn)}
            {/if}
            <!-- Mixed execution: an explicit choice, never a default. -->
            {#if parts.clean}
              <div class="row-control mixed" data-run-row="localFirst">
                <label class="check-row">
                  <input
                    type="checkbox"
                    class="check"
                    checked={localFirst}
                    aria-describedby={mixedHintId}
                    onchange={(event) => set('localFirst', event.currentTarget.checked)}
                  />
                  <span>{t('cloud.clean.mixed.label')}</span>
                </label>
              </div>
              <div class="row-note"><p class="gate" id={mixedHintId}>{t('tools.target.mixedHint')}</p></div>
            {/if}
            {#if cloudOff}
              <div class="row-note">
                <p class="gate" id={cloudOffId}>{t('settings.detection.runOn.off')}</p>
                <Button size="sm" variant="soft" onclick={openCloudSettings}>
                  {t('tools.option.engineCloudSettings')}
                </Button>
              </div>
            {/if}
            <!-- Which text is cleaned: a choice about cleaning, so only Detect &
                 clean asks it. Detect alone finds all text for review, and Clean
                 cleans what the review kept. -->
            {#if step === 'auto'}
              {@render chipRow('textPolicy', 'tools.param.textPolicy', policyOptions, allText ? 'all_text' : 'legacy_gate', choosePolicy)}
              {#if !allText && param.outsideBubbles}
                {@const opts = optionsFor(param.outsideBubbles)}
                {@render chipRow(
                  'outsideBubbles',
                  param.outsideBubbles.labelKey,
                  opts,
                  effectiveChoice(param.outsideBubbles, opts, values.outsideBubbles),
                  (/** @type {string} */ next) => set('outsideBubbles', next),
                )}
              {/if}
            {/if}
          </div>

          <!-- Mask padding: what Detect grows each fitted mask by, and with
               Apply what the masks already detected are re-padded to. -->
          {#if param.maskPadding}
            <div class="block padding" data-run-row="maskPadding">
              <Slider
                label={t(param.maskPadding.labelKey)}
                value={Number(values.maskPadding ?? 0)}
                min={param.maskPadding.min}
                max={param.maskPadding.max}
                step={param.maskPadding.step}
                unit={param.maskPadding.unit ?? ''}
                onchange={(value) => set('maskPadding', value)}
              />
              <p class="gate wide" id={paddingHintId}>{t('tools.padding.hint')}</p>
              <Button
                size="sm"
                variant="soft"
                disabled={running || applyingPadding}
                aria-describedby={paddingHintId}
                onclick={applyPadding}
              >
                {t(scope === 'page' ? 'tools.action.applyPaddingPage' : 'tools.action.applyPaddingChapter')}
              </Button>
            </div>
          {/if}

          {#if picksShown}
            <div class="more">
              <button
                type="button"
                class="more-toggle"
                aria-expanded={advancedOpen}
                aria-controls={advancedOpen ? `${uid}-advanced` : undefined}
                onclick={() => (advancedOpen = !advancedOpen)}
              >
                <span class="chev" class:spun={advancedOpen}><Icon name="chevron-down" size={12} /></span>
                <span class="more-label">{t('tools.action.advanced')}</span>
                <span class="more-value">{enginePicks}</span>
              </button>
              {#if advancedOpen}
                <div class="grid" id="{uid}-advanced">
                  {#if param.bubbleEngine}{@render engineRow(param.bubbleEngine)}{/if}
                  {#if param.outsideEngine}{@render engineRow(param.outsideEngine)}{/if}
                  {#if colorRead}{@render colorRow()}{/if}
                  {@render picksNote()}
                </div>
              {/if}
            </div>
          {/if}
        </div>

        <!-- The one action, named for what it does and over what. While a run
             is going the same button is its Cancel, so focus stays on the
             control that was pressed, and the progress sits in front of it. -->
        <div class="action" class:running>
          {#if running}
            <div
              class="progress"
              role="progressbar"
              aria-label={t('tools.label.progress')}
              aria-valuemin="0"
              aria-valuemax={progress ? progress.total : undefined}
              aria-valuenow={progress ? progress.done : undefined}
              aria-valuetext={progress ? t('editor.run.progress', { done: progress.done, total: progress.total }) : undefined}
            >
              <span class="progress-fill" class:unknown={!progress} style:width={progress ? `${progress.percent}%` : undefined}></span>
            </div>
          {/if}
          <!-- The blocked note describes the button it blocks. A disabled
               control announced without it is an action the user is refused
               for no stated reason, which is the whole failure the note exists
               to prevent. -->
          <Button
            variant={running ? 'soft' : 'primary'}
            size="lg"
            block={!running}
            disabled={Boolean(blockedKey) && !running}
            aria-describedby={blockedKey && !running ? blockedId : undefined}
            data-run-action
            onclick={run}
          >
            <Icon name={running ? 'stop' : 'play'} size={13} />
            {actionLabel}
          </Button>
          <!-- Rendered whether or not there is anything in it: a live region
               has to be on the page *before* the text arrives, or nothing is
               announced. -->
          <span class="live" aria-live="polite">{status}{#if progress}<span class="count">{` · ${progress.done} / ${progress.total}`}</span>{/if}</span>
          {#if blockedKey && !running}<p class="gate wide" id={blockedId}>{blockedText}</p>{/if}
          {#if !running && blockedKey === 'editor.state.detectModelsCloud'}
            <Button size="sm" variant="soft" onclick={() => setDetectOn('cloud')}>
              {t('tools.target.useCloud')}
            </Button>
          {/if}
          {#if !running && (blockedKey === 'editor.state.modelsMissing' || blockedKey === 'editor.state.detectModelsMissing'
            || blockedKey === 'editor.state.detectModelsCloud')}
            <Button size="sm" variant="soft" onclick={() => openSettings('models', 'detection')}>
              {t('tools.target.chooseModels')}
            </Button>
          {/if}
          {#if !running && (blockedKey === 'cloud.analysis.run.gate' || blockedKey === 'cloud.clean.gate')}
            <Button size="sm" variant="soft" onclick={openCloudSettings}>
              {t('tools.option.engineCloudSettings')}
            </Button>
          {/if}
        </div>
      </div>
    {:else}
      <div class="face pill-face">
        {@render grip()}
        {@render ident()}

        {#each layout.groups as group, index (group.key ?? index)}
          <span class="rule" role="separator" aria-orientation="vertical"></span>
          <div class="group">
            {#each group.params as entry (entry.key)}
              {#if entry.kind === 'range'}
                <Slider
                  compact
                  label={t(entry.labelKey)}
                  value={Number(values[entry.key] ?? entry.min)}
                  min={entry.min}
                  max={entry.max}
                  step={entry.step}
                  unit={entry.unit ?? ''}
                  onchange={(value) => set(entry.key, value)}
                />
              {:else if entry.kind === 'color'}
                <ColorPicker
                  value={String(values[entry.key] ?? entry.default ?? '#ffffff')}
                  label={t(entry.labelKey)}
                  onchange={(hex) => set(entry.key, hex)}
                />
              {:else}
                {@const opts = optionsFor(entry)}
                {@const value = effectiveChoice(entry, opts, values[entry.key])}
                {@const note = gateNote(entry)}
                {#if iconChoice(entry)}
                  <!-- The note beside the group is the group's description, not
                       a loose sentence on the bar: without the association a
                       screen reader reaches the disabled cell and is told
                       nothing about why. -->
                  {@const gateId = note ? `${uid}-gate-${entry.key}` : undefined}
                  <Segmented
                    options={opts}
                    {value}
                    label={t(entry.labelKey)}
                    describedBy={gateId}
                    onchange={(next) => set(entry.key, next)}
                  />
                  {#if note}<p class="gate" id={gateId}>{note}</p>{/if}
                  {#if note && entry.options.some((/** @type {any} */ option) => option.cloud)}
                    <Button size="sm" variant="soft" onclick={openCloudSettings}>
                      {t('tools.option.engineCloudSettings')}
                    </Button>
                  {/if}
                {:else}
                  {@const current = opts.find((option) => option.value === value)?.label ?? value}
                  {@render dropdown(
                    entry.labelKey,
                    entry.shortKey ?? entry.labelKey,
                    opts.map((option) => ({
                      id: option.value,
                      label: option.label,
                      disabled: option.disabled,
                      selected: option.value === value,
                    })),
                    current,
                    note,
                    (/** @type {string} */ next) => set(entry.key, next),
                  )}
                {/if}
              {/if}
            {/each}
          </div>
        {/each}

        {#if adjustable}
          <span class="rule" role="separator" aria-orientation="vertical"></span>
          <Popover label={t('tools.action.adjustments')} align="end">
            {#snippet trigger({ toggle, triggerProps })}
              <IconButton
                icon="sliders"
                label={t('tools.action.adjustments')}
                size={28}
                iconSize={15}
                onclick={toggle}
                {...triggerProps}
              />
            {/snippet}

            <div class="adjust">
              {#each layout.behind as entry (entry.key)}
                <Slider
                  label={t(entry.labelKey)}
                  value={Number(values[entry.key] ?? entry.min)}
                  min={entry.min}
                  max={entry.max}
                  step={entry.step}
                  unit={entry.unit ?? ''}
                  onchange={(value) => set(entry.key, value)}
                />
              {/each}
            </div>
          </Popover>
        {/if}

        <span class="rule" role="separator" aria-orientation="vertical"></span>
        {@render closeButton()}
      </div>
    {/if}
  {/key}
</section>

<style>
  /* The shell. As a pill: one row, as long as its contents and no longer - no
     width is stored and none is set here, and the radius is the tool rail's,
     the other floating strip of controls on this screen. As a panel: a fixed
     width capped by the window, and the radius of the app's other panels.
     Deliberately **no `overflow`**: a dropdown, a colour picker and the
     Adjustments popover open outside the shell, and a shell that clipped its
     overflow would cut them off at its own rounded edge. The one moment it
     clips is while it changes shape, when the popovers of the old content are
     gone and the new content must not spill past the edge still growing. */
  .bar {
    position: absolute;
    width: max-content;
    border-radius: 19px;
    background: var(--panel);
    box-shadow: var(--edge);
    animation: mcIn 150ms ease-out;
    cursor: grab;
  }
  .bar:active { cursor: grabbing }
  .bar.panel {
    width: min(var(--panel-width), calc(100vw - 8px));
    border-radius: var(--r-xl);
    cursor: default;
  }
  /* The grip moves the whole shell, so its ring is drawn around the shell - the
     thing the user is holding - exactly as a window's is drawn around the title
     bar it moves (WCAG 2.4.7, 2.4.11). */
  .bar:has(.grip:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  /* The content of one shape, which fades in as it arrives. */
  .face { animation: mcFade var(--dur) var(--ease) }

  .pill-face {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    height: 44px;
    padding: 0 var(--s-2);
  }

  .panel-face {
    display: flex;
    flex-direction: column;
  }

  .grip {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: none;
    /* 24 rather than the glyph's 16: the smallest target WCAG 2.5.8 will call
       a target, and the one control on the shell a hand goes for without
       looking. */
    width: 24px;
    height: 28px;
    padding: 0;
    border: none;
    background: transparent;
    color: var(--t3);
    cursor: grab;
    /* On the grip alone, and deliberately not on the shell: the pill's
       background is a drag surface too, and `touch-action: none` across the
       whole of it would take the touch gestures away from the slider and the
       swatch sitting on it. */
    touch-action: none;
  }
  .grip:hover { color: var(--t2) }
  .grip:focus-visible { outline: none }

  .ident {
    display: flex;
    align-items: center;
    flex: none;
    gap: var(--s-2);
    padding-right: var(--s-1);
    color: var(--t2);
  }
  /* The window title's own type: the smallest thing on screen that is still a
     name rather than a label. */
  .name {
    margin: 0;
    font-size: 10px;
    letter-spacing: .1em;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--t2);
    white-space: nowrap;
  }

  /* What separates one group of controls from the next on the pill. The tool
     window drew a small uppercase heading over each group; a pill has no
     second line to put one on, so a hairline carries it - and the group is
     structural from there on, named by nothing. */
  .rule {
    flex: none;
    width: 1px;
    height: 22px;
    background: var(--line);
  }

  .group {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }

  /* A dropdown's trigger: what the parameter is, in the muted ink a label is
     drawn in, then the value it holds in the ink of a control. The full label
     is the button's accessible name; this is the short form, because a
     sentence-long label cannot sit on a bar in front of a model name. */
  .drop {
    display: inline-flex;
    align-items: center;
    flex: none;
    gap: var(--s-2);
    height: 28px;
    padding: 0 var(--s-2) 0 var(--s-3);
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    background: var(--accent-soft);
    color: var(--text);
    font-size: 11px;
    white-space: nowrap;
    cursor: pointer;
    transition:
      border-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .drop:hover { border-color: var(--tint) }
  .drop-key { color: var(--t3) }

  /* The popover's own body, which is the panel grid the tool window used to
     be: a fixed label column, the control filling what is left, and a readout
     slot at the right, so a column of sliders starts and ends its tracks at one
     x. `ui/Slider.svelte` reads both widths. */
  .adjust {
    display: grid;
    gap: var(--s-2);
    --field-label: 84px;
    --field-readout: 40px;
  }

  /* ---- the panel ------------------------------------------------------ */

  /* The header is the panel's drag surface, and the only one. */
  .head {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    height: 40px;
    padding: 0 var(--s-2);
    border-bottom: 1px solid var(--line);
    cursor: grab;
  }
  .head:active { cursor: grabbing }
  .fill { flex: 1 }

  .rows {
    display: grid;
    gap: var(--s-4);
    padding: var(--s-4) var(--s-5);
  }

  /* Mode and scope: the label over a row of chips that fills the panel. */
  .block { display: grid; gap: 5px }
  .block.padding { justify-items: start }
  .block.padding > :global(:first-child) { justify-self: stretch }
  .block-label,
  .row-label {
    font-size: 10.5px;
    line-height: 1.3;
    color: var(--t2);
  }

  /* Everything else: a label column and a control column, so the controls
     start at one x. A note under a control sits in the control's column. */
  .grid {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    align-items: center;
    gap: var(--s-3) var(--s-3);
  }
  .grid:empty { display: none }
  .row-control { min-width: 0 }
  /* A checkbox under the place it qualifies, in the control column. */
  .mixed { grid-column: 2 }
  .check-row {
    display: flex;
    align-items: flex-start;
    gap: var(--s-2);
    font-size: 11.5px;
    line-height: 1.35;
    color: var(--text);
    cursor: pointer;
  }
  .check {
    flex: none;
    width: 14px;
    height: 14px;
    margin: 1px 0 0;
    accent-color: var(--accent);
    cursor: pointer;
  }
  .row-note {
    grid-column: 2;
    display: grid;
    gap: var(--s-2);
    justify-items: start;
    margin-top: calc(-1 * var(--s-1));
  }

  /* The collapsed group: a quiet line that says what it holds and what is
     chosen in it, so the choices are visible without being opened. */
  .more { display: grid; gap: var(--s-3) }
  .more-toggle {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    min-height: 24px;
    padding: 0;
    border: none;
    background: transparent;
    color: var(--t2);
    font-size: 10.5px;
    text-align: start;
    cursor: pointer;
  }
  .more-toggle:hover { color: var(--text) }
  .more-label { flex: none }
  .more-value {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--t3);
  }
  .chev {
    display: flex;
    flex: none;
    color: var(--t3);
    transform: rotate(-90deg);
    transition: transform var(--dur) var(--ease);
  }
  .chev.spun { transform: none }

  .action {
    display: grid;
    gap: var(--s-2);
    padding: var(--s-4) var(--s-5) var(--s-5);
    border-top: 1px solid var(--line);
  }
  /* Running: the progress in front of Cancel, on one line. */
  .action.running {
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    column-gap: var(--s-3);
  }
  .action.running .live { grid-column: 1 / -1 }

  .progress {
    position: relative;
    height: 6px;
    overflow: hidden;
    border-radius: var(--r-pill);
    background: var(--accent-soft);
  }
  .progress-fill {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: inherit;
    background: var(--accent);
    transition: width var(--dur) var(--ease);
  }
  /* Before the queue is known: a still, partial bar rather than a moving one,
     so there is nothing for reduced motion to stop. */
  .progress-fill.unknown { width: 12%; opacity: .5 }

  /* The run's own line, and it takes no room at all while there is nothing to
     say - an empty live region that still reserved a gap would put a hole in
     the panel for the whole time nothing is running.

     Collapsed rather than `display: none`, and that is the whole point: a
     `display: none` element is out of the accessibility tree, so the region
     would not exist until the moment its first text arrived - which is exactly
     the announcement that would then be missed. This takes no space and stays
     in the tree. */
  .live {
    font-size: 10.5px;
    line-height: 1.3;
    color: var(--t2);
  }
  .live:empty {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .count { color: var(--t3) }

  /* A reason something is unavailable: beside the control it is about, in the
     muted ink the rest of the shell's small text uses. `--t2` rather than
     `--t3` - this is the only channel for a reason the user needs, and `--t3`
     is under 4.5:1 against `--panel` in dark. */
  .gate {
    margin: 0;
    max-width: 190px;
    font-size: 10px;
    line-height: 1.3;
    color: var(--t2);
  }
  .gate.wide,
  .row-note .gate { max-width: none }

  /* Taller than the window: the rows scroll, between a header and an action
     that stay. The 8px is `MIN_TOP` above and below. */
  .bar.tight .panel-face { max-height: calc(100vh - 66px) }
  .bar.tight .head,
  .bar.tight .action { flex: none }
  .bar.tight .rows {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
</style>
