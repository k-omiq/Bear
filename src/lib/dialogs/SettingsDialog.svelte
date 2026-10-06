<script module>
  import { MODELS as PIPELINE_MODELS } from '../model/pipelines.js'

  /**
   * Whether a `model-progress` event is about a row this dialog does not know
   * is downloading.
   *
   * The event channel is process-wide, so a second window hears the first
   * one's transfer; `catalogue` is a snapshot taken on mount and refreshed only
   * when something *ends*, so the row underneath that live progress can still
   * read "Not installed" with a Download button on it. `true` here means the
   * snapshot is behind the world and one `listModels` would put it right.
   *
   * Pure, and exported, so the decision is pinned without mounting a dialog:
   * everything around it is a timer and a call.
   *
   * @param {import('../api/backend.js').ModelsView|null} catalogue
   * @param {string} id
   * @param {string} runtimeId
   * @returns {boolean}
   */
  export function isStrangerDownload(catalogue, id, runtimeId = 'runtime') {
    // Nothing drawn yet: the mount's own `listModels` is already on its way and
    // a second one would answer the same question twice.
    if (!catalogue) return false
    if (id === runtimeId) return catalogue.runtime.downloading !== true
    const row = catalogue.models.find((model) => model.id === id)
    // An id with no row at all is a catalogue older than the backend - a
    // seventh weight added since this snapshot - which is the same remedy.
    return row?.downloading !== true
  }

  /**
   * The multi-file downloads Settings lists as one row each, with the name it
   * gives each. Derived from `model/pipelines.js#MODELS`, which is also what
   * `api/model-download-notices.js` names a failed download by.
   */
  export const MODEL_GROUPS = Object.freeze(
    PIPELINE_MODELS.filter((entry) => entry.group).map((entry) =>
      Object.freeze({ id: /** @type {string} */ (entry.group), nameKey: entry.nameKey, fileIds: entry.files }),
    ),
  )
</script>

<script>
  /**
   * Settings, as a full-window screen over the route underneath.
   *
   * It is still the `settings` modal kind, so every opener pushes the same
   * spec and the editor stays mounted behind it. `Screen` owns the layer
   * (focus in and back out, the Tab trap, Escape); this file owns the layout:
   * a sidebar with the way back and a vertical tab list, and one scrolling
   * column per section.
   *
   * **Seven sections, in the order a visit needs them.** General holds the
   * app's own preferences and the setup replay. Models is what a page is
   * detected and cleaned with: the detection choices, each model's files
   * beside the choice that made them necessary, the language filtering and
   * the FLUX helper collapsed until asked for, and the download token under
   * Download access. Cloud holds `InferenceSettings`. Denoise is page
   * denoise's target, preset and local model (`denoise/DenoiseSettings`).
   * Performance is what the
   * engines run on: the runtime, its build, where downloads go, the
   * accelerator, and per-model backends collapsed. Shortcuts and About close
   * the list; About is a GPL-3.0 obligation, this screen is its only route,
   * and it carries the community invite.
   *
   * Detection and Cleaning were two sections. As two groups of one, with the
   * advanced parts collapsed, Models measured 1,188 px tall at the minimum
   * window (1000 x 640, mock catalogue) against 1,711 px for Detection alone
   * and 1,056 px for Cleaning before: one stop instead of two, and shorter
   * than either half used to be.
   *
   * **Where a run goes is not here.** Detect on, Clean on, the run's scope and
   * its engine are the editor's Text cleanup panel's, beside the button that
   * starts the run. Models says only what each detection model can do: which
   * ones have a cloud version, and, while Text cleanup detects on the cloud
   * GPU, the fixed combination that run uses and where each part of it runs
   * (`pipelines.js#runDetection`, `#detectionPlacement`).
   *
   * **Older ids still open the right place** (`settingslinks.js`): Detection
   * and Cleaning land on their group in Models, `inference` on Cloud. A
   * caller can name an anchor too - a group or one model's row - and the
   * screen opens the collapsed group holding it, scrolls to it and moves
   * focus there.
   *
   * **Every panel is mounted, and only the selected one is shown.** `hidden`
   * rather than `{#if}`, so `aria-controls` names an element that exists, a
   * panel keeps its scroll position while another is looked at, and the
   * mount-time work is one `listModels` and one `about` however many panels
   * are visited.
   *
   * **The tab list is a real one.** `role="tablist"` with
   * `aria-orientation="vertical"`, one tab stop, Up and Down to move, Home and
   * End to the ends. Selection follows focus, which is safe because every
   * panel is already mounted. The outline is h1 (the screen), h2 (the
   * section), h3 below it.
   *
   * **A panel is focusable when, and only when, its tail is not** (WCAG
   * 2.1.1). Performance takes a tab stop while it ends in prose rather than in
   * its per-model backends; every other section ends in a control.
   *
   * **The backend reconciliation is not here.** `session.*` and
   * `backend.readSettings()` are reconciled once at boot, in `App.svelte`.
   * What this screen owns is the second half: every change pushes the
   * session's values down to the backend immediately.
   *
   * **The shortcut section is not a read-only list.** `ShortcutSheet` is where
   * a binding is *changed*, and it writes its own half of the settings, so it
   * behaves identically here and mounted on its own by `?`.
   */
  import { Button, ColorPicker, Disclosure, Field, Range, Screen, Segmented, Select, TextInput, ThemePicker } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import { onMount, tick, untrack } from 'svelte'
  import { closeModal } from '../state/app.svelte.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import { getBackend } from '../api/backend.js'
  import { chooseFolder, chooseOnnx } from '../api/folder.js'
  import { showSettingsSection } from '../api/model-download-notices.js'
  import { resolveSettingsLink, SETTINGS_SECTIONS } from './settingslinks.js'
  import { LOCALES, hasKey, t } from '../i18n/index.js'
  import { capabilities, loadCapabilities } from '../state/capabilities.svelte.js'
  import { resetCloudOffer, syncCloudOffer } from '../state/cloudtargets.svelte.js'
  import {
    THEMES,
    THEME_LABEL_KEYS,
    backendSettingsPatch,
    session,
    setAccelerator,
    setModelAccelerator,
    setCloseToTray,
    setDetection,
    setDetectorModels,
    setFluxBackend,
    setFluxModel,
    setOriginalView,
    setReadingDirection,
    setSidecarPath,
    setOcrRescue,
    setLanguage,
    setTheme,
    setMaskColor,
    setMaskOpacity,
    setOutsideMaskColor,
  } from '../state/session.svelte.js'
  const ACCEL_STATE_KEYS = {
    verified: 'settings.accel.state.verified',
    available: 'settings.accel.state.available',
    installed: 'settings.accel.state.installed',
    supported: 'settings.accel.state.supported',
    unsupported: 'settings.accel.state.unsupported',
  }
  import {
    ALL_TEXT_POLICY,
    CAPABILITIES,
    CLEANERS,
    LANGUAGES,
    detectionPlacement,
    groupOfModel,
    model as pipelineModel,
    modelOfFile,
    runDetection,
    runsOnCloud,
    runtimeState,
    toggleDetectorModel,
    usedNow,
    workflowNeeds,
    workflowForDetectorModels,
  } from '../model/pipelines.js'
  import WorkflowAnalysis from './WorkflowAnalysis.svelte'
  import ShortcutSheet from './ShortcutSheet.svelte'
  import AboutSection from './AboutSection.svelte'
  import InferenceSettings from './InferenceSettings.svelte'
  import DenoiseSettings from './denoise/DenoiseSettings.svelte'
  import { PAGE_DENOISE_FEATURE } from '../model/denoise.js'
  import { offerFirstLaunch } from './firstlaunch.svelte.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()

  let sidecarModels = $state(/** @type {Array<{id: string, label: string}>} */ ([]))

  async function loadModels() {
    if (capabilities.sidecar) {
      try {
        sidecarModels = await getBackend().listSidecarModels()
        if (!session.fluxModel && sidecarModels.length > 0) {
          const defaultModel = sidecarModels.some((m) => m.id === 'flux2-klein-4b')
            ? 'flux2-klein-4b'
            : sidecarModels[0].id
          setFluxModel(defaultModel)
          // Written straight away, like every other change here. Not `push()`:
          // that calls back into this function.
          await writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
        }
      } catch {
        sidecarModels = []
      }
    } else {
      sidecarModels = []
    }
  }

  $effect(() => {
    if (capabilities.sidecar) {
      loadModels()
    }
  })

  /** Push the session's values down to the backend after any change. */
  async function push() {
    await writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
    await loadCapabilities()
    await loadModels()
  }

  let backgroundError = $state(false)

  async function updateCloseToTray(input) {
    const enabled = input.checked
    backgroundError = false
    try {
      await writeSettingsSerialized(getBackend(), { closeToTray: enabled })
      setCloseToTray(enabled)
    } catch {
      backgroundError = true
      input.checked = session.closeToTray
    }
  }

  /* ---------- Models ---------- */

  /**
   * The catalogue, the runtime row, and where a download would go.
   *
   * `null` until the first answer: the section draws nothing rather than a
   * list of six rows all reading "Not installed", which is what a `[]` default
   * would show for the moment before the call returns and is the most alarming
   * thing this dialog could say untruthfully.
   *
   * @type {import('../api/backend.js').ModelsView|null}
   */
  let catalogue = $state(null)

  /**
   * What each download has reported, by id: `{downloaded, total}` while it
   * runs, gone the moment it ends.
   *
   * Held here rather than on `catalogue` because it arrives on the event
   * channel - `model-progress`, the seam's sixth event - and `listModels` is a
   * snapshot that knows nothing between two calls. The two are joined at the
   * row: the catalogue says what a thing *is*, this says what is happening to
   * it now.
   *
   * @type {Record<string, {downloaded: number, total: number|null}>}
   */
  let progress = $state({})
  let groupBusy = $state({})
  let groupFailures = $state({})
  let groupNotes = $state({})
  let groupVerification = $state({})

  // `MODEL_GROUPS` is declared in the module script above, exported.

  /** The last failure per id, until something else happens to that row. @type {Record<string, string>} */
  let failures = $state({})

  /**
   * What the last press on a row *answered*, as an i18n key, for the presses
   * that did nothing.
   *
   * Separate from `failures` because none of these is a failure: a download
   * that was already running and a delete that found nothing are both this
   * window looking at a row another window has moved on from.
   * The sentence goes under the row and is cleared by the next thing
   * that happens to it.
   *
   * @type {Record<string, string>}
   */
  let notes = $state({})

  /**
   * The sentence for a press that did nothing, by the answer it gave.
   *
   * Whole keys in a table rather than one assembled from the answer: the
   * catalogue's test asks for keys to be *chosen between* rather than built,
   * because a dead string cannot be found behind a template. An answer with no
   * entry - `started`, `deleted` - is the ordinary one and says nothing.
   */
  const DECLINED = {
    alreadyRunning: 'settings.models.declined.alreadyRunning',
    alreadyInstalled: 'settings.models.declined.alreadyInstalled',
    notFound: 'settings.models.declined.notFound',
    readOnlyElsewhere: 'settings.models.declined.readOnlyElsewhere',
    busy: 'settings.models.declined.busy',
  }

  /** @param {string} id @param {string|null} key */
  function note(id, key) {
    notes = key === null
      ? Object.fromEntries(Object.entries(notes).filter(([held]) => held !== id))
      : { ...notes, [id]: key }
  }

  /**
   * Ask for the catalogue again.
   *
   * `retryStore` is passed on the **open** and on nothing else. The token
   * migration is offered to the credential store once per process, which is
   * what keeps a locked keychain from prompting on every poll
   * and is also what leaves a keychain unlocked since launch unnoticed. An open
   * of this dialog is a user arriving at the one screen that says where their
   * token is, so it is worth one more attempt; the refreshes below - after a
   * press, after a download ends, on a stranger's progress event - are not.
   *
   * @param {{retryStore?: boolean}} [options]
   */
  async function refreshCatalogue(options = {}) {
    try {
      catalogue = await getBackend().listModels(options)
    } catch {
      catalogue = null
    }
    await refreshRuntimeLoad()
  }

  /**
   * Whether the installed runtime **loads**, which the catalogue cannot say.
   *
   * The runtime row's `installed` is a file found. A native run also loads
   * it and refuses to start when that fails - a CUDA build on a machine
   * without CUDA, a quarantined or damaged library - so a readiness row that
   * read `installed` alone went green over a run that would not start.
   * `diagnostics` makes the same load and keeps the failures apart by remedy
   * (`diagnostics.runtime.*`), so its answer is asked after every catalogue
   * refresh that finds the runtime here.
   *
   * Not asked again once it has loaded: the process keeps the library it
   * loaded for its lifetime, so the answer cannot change until the file goes,
   * and then the catalogue says `missing` first. A later answer supersedes an
   * earlier one still on its way (`runtimeLoadAsk`).
   *
   * @type {{state: 'checking'|'loaded'|'unchecked', reasonKey?: undefined}|{state: 'failed', reasonKey: string}}
   */
  let runtimeLoad = $state({ state: 'checking' })
  let runtimeLoadAsk = 0

  /** The keys a load failure may name; anything else reads as the generic one. */
  const LOAD_REASONS = new Set([
    'diagnostics.runtime.missing',
    'diagnostics.runtime.quarantined',
    'diagnostics.runtime.refused',
    'diagnostics.runtime.missingDependency',
    'diagnostics.runtime.unloadable',
  ])

  async function refreshRuntimeLoad() {
    const row = catalogue?.runtime
    const ask = ++runtimeLoadAsk
    if (!row?.installed) {
      runtimeLoad = { state: 'checking' }
      return
    }
    if (runtimeLoad.state === 'loaded') return
    try {
      const answer = await getBackend().diagnostics()
      if (ask !== runtimeLoadAsk) return
      const status = answer?.components?.find((component) => component.name === 'onnxruntime')
      if (!status) runtimeLoad = { state: 'unchecked' }
      else if (status.available) runtimeLoad = { state: 'loaded' }
      else {
        // The loader's own words are for the log, never the screen.
        if (status.detail) console.warn('ONNX Runtime did not load:', status.detail)
        runtimeLoad = {
          state: 'failed',
          reasonKey: LOAD_REASONS.has(status.reasonKey ?? '') ? /** @type {string} */ (status.reasonKey) : 'diagnostics.runtime.unloadable',
        }
      }
    } catch (error) {
      if (ask !== runtimeLoadAsk) return
      console.error('diagnostics was rejected', error)
      runtimeLoad = { state: 'unchecked' }
    }
  }

  let replayError = $state(false)
  let replaying = $state(false)

  /**
   * "Run setup again": the offer a first launch makes, over a fresh catalogue
   * so the download step shows what is here now. Settings closes first,
   * because the setup is drawn beside the modal stack and only while the
   * stack is empty (`App.svelte`) - but only once the offer has been made, so
   * a failure is said here, on a screen that is still open.
   */
  async function replayOnboarding() {
    if (replaying) return
    replaying = true
    replayError = false
    let offered = false
    try {
      const view = await getBackend().listModels()
      offered = offerFirstLaunch(view, { force: true })
      if (!offered) replayError = true
    } catch {
      replayError = true
    } finally {
      replaying = false
    }
    if (offered) closeModal(null)
  }

  /**
   * A download this window did not start.
   *
   * `model-progress` goes to every subscriber in the process, so a second
   * window hears the first one's transfer - but `catalogue` is a snapshot taken
   * on mount and refreshed only when something ends, so the row underneath the
   * live progress still reads "Not installed" with a Download button on it.
   * One `listModels` puts that right, and it is debounced
   * because the events arrive every 4 MiB and the catalogue is six `stat` calls
   * plus a settings read.
   *
   * @type {ReturnType<typeof setTimeout>|null}
   */
  let unknownRefresh = null

  /** @param {string} id */
  function refreshForStranger(id) {
    if (unknownRefresh !== null || !isStrangerDownload(catalogue, id, RUNTIME_ID)) return
    unknownRefresh = setTimeout(() => {
      unknownRefresh = null
      refreshCatalogue()
    }, 500)
  }

  onMount(() => {
    // The one call that asks for the credential store to be tried again.
    refreshCatalogue({ retryStore: true })
    // One handler for the whole dialog's lifetime. `subscribe` is the merge of
    // both implementations' streams (`api/tauri-events.js`), so this receives
    // the mock's simulated download in a browser and the command's real one
    // inside a Tauri window, with no branch here.
    const unsubscribe = getBackend().subscribe((event) => {
      if (event.type !== 'model-progress') return
      if (event.done) {
        const { [event.id]: _gone, ...rest } = progress
        progress = rest
        // Whatever the last press said about this row is about a press that
        // has now been overtaken by an ending.
        note(event.id, null)
        const group = MODEL_GROUPS.find((candidate) => candidate.id === event.id)
        if (group && event.total === null) {
          groupBusy = { ...groupBusy, [group.id]: false }
          groupFailures = event.error && event.error !== 'cancelled'
            ? { ...groupFailures, [group.id]: event.error }
            : Object.fromEntries(Object.entries(groupFailures).filter(([id]) => id !== group.id))
        }
        // A cancellation is a failure with a name the user chose, so it is
        // not shown as one: the row goes back to "Not installed", which is
        // the true thing about it, and that is the whole report.
        failures =
          event.error && event.error !== 'cancelled'
            ? { ...failures, [event.id]: event.error }
            : Object.fromEntries(Object.entries(failures).filter(([id]) => id !== event.id))
        refreshCatalogue()
        // A download changes which engines can run, which is the point of the
        // whole section.
        loadCapabilities()
        return
      }
      failures = Object.fromEntries(Object.entries(failures).filter(([id]) => id !== event.id))
      note(event.id, null)
      // A row that does not know it is downloading is a row drawn before
      // another window pressed. Ask again.
      refreshForStranger(event.id)
      progress = { ...progress, [event.id]: { downloaded: event.downloaded, total: event.total } }
    })
    return () => {
      if (unknownRefresh !== null) clearTimeout(unknownRefresh)
      unsubscribe()
    }
  })

  /**
   * One row's status, as a key and its parameters.
   *
   * Five states and they are ordered by what the reader needs to know first: a
   * download in flight beats everything, then a failure they have to act on,
   * then a digest that did not match, then presence.
   *
   * @param {string} id
   * @param {boolean} installed
   * @param {boolean|null} [sha256Ok]
   * @returns {{key: string, params?: Object}}
   */
  function statusOf(id, installed, sha256Ok) {
    const live = progress[id]
    if (live) {
      const percent = live.total ? Math.floor((live.downloaded / live.total) * 100) : null
      return percent === null
        ? { key: 'settings.models.status.downloading' }
        : { key: 'settings.models.status.downloadingPercent', params: { percent } }
    }
    if (failures[id]) return { key: 'settings.models.status.failed' }
    if (installed && sha256Ok === false) return { key: 'settings.models.status.mismatch' }
    if (installed) return { key: 'settings.models.status.installed' }
    return { key: 'settings.models.status.missing' }
  }

  /**
   * What a refused row actually says.
   *
   * Two shapes arrive here as one string. A refusal the backend has a sentence
   * for answers as a catalogue key with its figures beside it, in the grammar
   * `key name=value ...` with decimal values - `notice.runtime.noSpace
   * needed=253000000 free=1200000`, and `notice.runtime.inUse` with no figures
   * at all. Everything else answers with whatever the failure itself said,
   * which is what this row showed for every failure before the two Windows
   * refusals existed. The byte counts travel as data rather than inside the
   * sentence because a byte count is not translatable; `{needed:memory}` in the
   * catalogue is what turns them into something a person reads.
   *
   * Only `notice.runtime.*` is honoured, and deliberately: a rejection free to
   * name any key in the catalogue would be the backend choosing what the
   * interface says. It is the same restriction `reportRegionEditFailure`
   * applies to `decline.reason.*` for a region edit, for the same reason.
   *
   * @param {string} id
   * @returns {string}
   */
  function failureText(id) {
    const message = String(failures[id] ?? '')
    if (!message) return ''
    const [key, ...pairs] = message.split(' ')
    if (!key.startsWith('notice.runtime.') || !hasKey(key)) return message
    /** @type {Record<string, number>} */
    const params = {}
    for (const pair of pairs) {
      const at = pair.indexOf('=')
      // A malformed pair is dropped rather than shown: the sentence it belongs
      // to renders without that figure, which is a worse sentence and not a
      // wrong one.
      if (at <= 0) continue
      const value = Number(pair.slice(at + 1))
      if (Number.isFinite(value)) params[pair.slice(0, at)] = value
    }
    return t(key, params)
  }

  /**
   * Press Download, and say what the press answered.
   *
   * `started` is the ordinary answer and says nothing - the progress bar is the
   * report. The other two are refusals, and both mean the row was drawn before
   * something else happened to it, so the sentence goes up *and* the list is
   * refreshed.
   *
   * @param {string} id
   */
  async function download(id) {
    failures = Object.fromEntries(Object.entries(failures).filter(([key]) => key !== id))
    note(id, null)
    try {
      const outcome =
        id === RUNTIME_ID
          ? await getBackend().downloadRuntime()
          : await getBackend().downloadModel({ id })
      note(id, DECLINED[outcome] ?? null)
    } catch (error) {
      failures = { ...failures, [id]: String(error) }
    }
    await refreshCatalogue()
  }

  /** @param {string} id */
  async function cancel(id) {
    try {
      await getBackend().cancelDownload({ id })
    } finally {
      await refreshCatalogue()
    }
  }

  /**
   * Press Delete, and say what it found.
   *
   * Three answers rather than a boolean: the file went, there
   * was nothing there, or there is one and it is not this application's to
   * remove. The last two are only reachable from a stale row, which is exactly
   * when the user needs telling rather than left with a button that did
   * nothing.
   *
   * @param {string} id
   */
  async function remove(id) {
    note(id, null)
    try {
      const outcome =
        id === RUNTIME_ID
          ? await getBackend().deleteRuntime()
          : await getBackend().deleteModel({ id })
      note(id, DECLINED[outcome] ?? null)
    } catch (error) {
      failures = { ...failures, [id]: String(error) }
    }
    await refreshCatalogue()
    await loadCapabilities()
  }

  /**
   * Give back the bytes a stopped download left.
   *
   * The row reports them because keeping them is what makes the next Download
   * resume rather than start over, and 180 MB of a 207 MB
   * transfer nobody came back for is disk the user did not agree to spend.
   * `false` means there was nothing there - or that a download is
   * writing into it, which is the same news to this dialog and is why the list
   * is refreshed either way: the row then shows the transfer it lost to.
   *
   * @param {string} id
   */
  async function discard(id) {
    note(id, null)
    try {
      await getBackend().discardPartial({ id })
    } catch (error) {
      failures = { ...failures, [id]: String(error) }
    }
    await refreshCatalogue()
  }

  /** @param {string} id */
  async function verify(id) {
    note(id, null)
    try {
      await getBackend().verifyModel({ id })
    } catch (error) {
      failures = { ...failures, [id]: String(error) }
    }
    await refreshCatalogue()
  }

  /** @param {string} id - a native group id */
  function groupById(id) {
    return MODEL_GROUPS.find((group) => group.id === id) ?? null
  }

  /** @param {{id: string, fileIds: readonly string[]}} group */
  async function downloadGroup(group) {
    groupFailures = Object.fromEntries(Object.entries(groupFailures).filter(([id]) => id !== group.id))
    groupNotes = Object.fromEntries(Object.entries(groupNotes).filter(([id]) => id !== group.id))
    try {
      const outcome = await getBackend().downloadModelGroup({ id: group.id })
      groupBusy = { ...groupBusy, [group.id]: outcome === 'started' }
      if (outcome !== 'started') groupNotes = { ...groupNotes, [group.id]: DECLINED[outcome] ?? null }
    } catch (error) {
      groupFailures = { ...groupFailures, [group.id]: String(error) }
      groupBusy = { ...groupBusy, [group.id]: false }
    }
    await refreshCatalogue()
  }

  /** @param {{id: string, fileIds: readonly string[]}} group */
  async function cancelGroup(group) {
    groupBusy = { ...groupBusy, [group.id]: false }
    await Promise.all(group.fileIds.map((id) => {
      const model = catalogue?.models.find((entry) => entry.id === id)
      return (progress[id] || model?.downloading)
        ? getBackend().cancelDownload({ id }).catch((error) => { groupFailures = { ...groupFailures, [group.id]: String(error) } })
        : Promise.resolve()
    }))
    await refreshCatalogue()
  }

  /** @param {{id: string, fileIds: readonly string[]}} group */
  async function verifyGroup(group) {
    groupFailures = Object.fromEntries(Object.entries(groupFailures).filter(([id]) => id !== group.id))
    try {
      const verified = await getBackend().verifyModelGroup({ id: group.id })
      groupVerification = { ...groupVerification, [group.id]: verified }
      if (!verified) groupFailures = { ...groupFailures, [group.id]: t('settings.models.groupMismatch') }
    } catch (error) {
      groupFailures = { ...groupFailures, [group.id]: String(error) }
    }
    await refreshCatalogue()
  }

  /** @param {{id: string, fileIds: readonly string[]}} group */
  async function removeGroup(group) {
    groupFailures = Object.fromEntries(Object.entries(groupFailures).filter(([id]) => id !== group.id))
    groupNotes = Object.fromEntries(Object.entries(groupNotes).filter(([id]) => id !== group.id))
    try {
      const outcome = await getBackend().deleteModelGroup({ id: group.id })
      if (outcome !== 'deleted') groupNotes = { ...groupNotes, [group.id]: DECLINED[outcome] ?? null }
      groupVerification = Object.fromEntries(Object.entries(groupVerification).filter(([id]) => id !== group.id))
    } catch (error) {
      groupFailures = { ...groupFailures, [group.id]: String(error) }
    }
    await refreshCatalogue()
    await loadCapabilities()
  }

  /* ---------- the capability graph ---------- */

  /**
   * The SAM-TS-L and Ogkalu Full readiness, from the same
   * `listWorkflowCapabilities` the review panel reads. `null` until it
   * answers; `workflowFailed` separates "not asked yet" from "refused", for
   * the reason `accelFailure` gives.
   *
   * @type {any}
   */
  let workflowCaps = $state(null)
  let workflowFailed = $state(false)
  /** Whether an explicit Check of the SAM graphs passed; null until one runs. @type {boolean|null} */
  let samVerified = $state(null)
  /** @type {Record<string, boolean>} */
  let importBusy = $state({})
  /** @type {Record<string, string>} */
  let importFailures = $state({})

  async function refreshWorkflowCaps() {
    const backend = getBackend()
    if (typeof backend.listWorkflowCapabilities !== 'function') {
      workflowFailed = true
      return
    }
    try {
      workflowCaps = await backend.listWorkflowCapabilities()
      workflowFailed = false
    } catch {
      workflowCaps = null
      workflowFailed = true
    }
  }

  /**
   * Asked when Models is first shown, not on mount, for the reason the
   * accelerator list waits for Performance: the answer digests the Ogkalu
   * detector graphs and probes the runtime, which is work only this panel
   * needs.
   */
  let workflowAsked = false
  $effect(() => {
    if (active !== 'models') return
    untrack(() => {
      if (workflowAsked) return
      workflowAsked = true
      refreshWorkflowCaps()
    })
  })

  /** @param {Record<string, unknown>} record @param {string} key */
  function without(record, key) {
    return Object.fromEntries(Object.entries(record).filter(([held]) => held !== key))
  }

  /**
   * The catalogue rows a logical model is made of, in its own order. A file
   * the catalogue does not list is left out rather than invented.
   *
   * @param {import('../model/pipelines.js').LogicalModel} entry
   */
  function filesOf(entry) {
    return entry.files.map((id) => catalogue?.models.find((row) => row.id === id)).filter(Boolean)
  }

  /**
   * What one logical row says: total size, one state, and what it is still
   * missing. Downloads read the catalogue and the live progress; imports
   * read the workflow readiness answer.
   *
   * @param {import('../model/pipelines.js').LogicalModel} entry
   */
  function viewOf(entry) {
    if (entry.source === 'import') return importView(entry)
    const files = /** @type {any[]} */ (filesOf(entry))
    const total = files.length
    const installedCount = files.filter((row) => row.installed).length
    const installed = total > 0 && installedCount === total
    const bytes = files.reduce((sum, row) => sum + row.bytes, 0)
    const missingBytes = files.reduce((sum, row) => (row.installed ? sum : sum + row.bytes), 0)
    const downloading = Boolean(entry.group && groupBusy[entry.group]) || files.some((row) => progress[row.id] || row.downloading)
    const readOnly = files.some((row) => row.installed && row.readOnly)
    const failed = entry.group ? Boolean(groupFailures[entry.group]) : files.some((row) => failures[row.id])
    const mismatch = files.some((row) => row.installed && row.sha256Ok === false) ||
      (entry.group ? groupVerification[entry.group] === false : false)
    const verified = installed && (entry.group ? groupVerification[entry.group] === true : files.every((row) => row.sha256Ok === true))
    /** @type {{key: string, params?: Object}} */
    let state
    if (downloading) {
      const got = files.reduce((sum, row) => sum + (row.installed ? row.bytes : (progress[row.id]?.downloaded ?? 0)), 0)
      const percent = bytes > 0 && files.some((row) => progress[row.id]) ? Math.min(99, Math.floor((got / bytes) * 100)) : null
      state = percent === null
        ? { key: 'settings.models.status.downloading' }
        : { key: 'settings.models.status.downloadingPercent', params: { percent } }
    } else if (failed) state = { key: 'settings.models.status.failed' }
    else if (mismatch) state = { key: 'settings.models.status.mismatch' }
    else if (installed) state = { key: 'settings.models.status.installed' }
    else if (installedCount > 0) state = { key: 'settings.models.status.someInstalled', params: { installed: installedCount, total } }
    else state = { key: 'settings.models.status.missing' }
    return { kind: 'download', known: total > 0, files, total, installedCount, installed, bytes, missingBytes, downloading, readOnly, verified, state }
  }

  /** @param {import('../model/pipelines.js').LogicalModel} entry */
  function importView(entry) {
    const caps = workflowCaps
    if (!caps) {
      return {
        kind: 'import', known: false, installed: false, managed: false, bytes: null, files: [], revision: null, verified: false,
        state: { key: workflowFailed ? 'settings.models.status.readinessUnavailable' : 'settings.models.status.checking' },
      }
    }
    const sam = entry.importId === 'samTs'
    /** @type {Array<{name: string, bytes: number, sha256: string}>} */
    const files = sam ? (caps.samFiles ?? []) : (caps.fullRtFile ? [caps.fullRtFile] : [])
    const installed = (sam ? caps.samInstalled : caps.fullRtInstalled) === true
    const managed = (sam ? caps.samManaged : caps.fullRtManaged) === true
    const bytes = files.reduce((sum, file) => sum + (Number(file.bytes) || 0), 0) || null
    // The full graph is digested on every readiness answer, so present means
    // verified. The SAM pair is only checked when someone presses Check.
    const verified = installed && (sam ? samVerified === true : true)
    /** @type {{key: string, params?: Object}} */
    let state
    if (importBusy[entry.id]) state = { key: 'settings.models.status.checking' }
    else if (!installed) state = { key: 'settings.models.status.importToEnable' }
    else if (sam && samVerified === false) state = { key: 'settings.models.status.mismatch' }
    else state = { key: 'settings.models.status.imported' }
    return { kind: 'import', known: true, installed, managed, bytes, files, revision: sam ? caps.samRevision : caps.fullRtRevision, verified, state }
  }

  /** The choices the workflow is computed from. */
  const choices = $derived({ textPolicy: session.textPolicy, detection: session.detection, ocrRescue: session.ocrRescue, detectorModels: session.detectorModels, analysisTargets: session.analysisTargets })
  const allText = $derived(session.textPolicy === ALL_TEXT_POLICY)
  /** The logical models the selected workflow needs. */
  const needs = $derived(workflowNeeds(choices))

  /**
   * Whether a missing model is one the selected workflow needs now. The
   * review's small profile is not needed while the full graph is imported.
   *
   * @param {string} id
   */
  function neededNow(id) {
    if (!needs.includes(id)) return false
    if (allText && id === 'rtSmall' && workflowCaps?.fullRtInstalled) return false
    return true
  }

  /**
   * Whether a single-file model's own Delete in File details is held, and
   * why. A model more than one workflow reads (the Small detector: legacy
   * cleaning and the review's small profile) is not removed from the details
   * while the selected workflow uses it. The row's own Delete stays: that is
   * where the removal is asked, with everything it stops named first. The
   * group rows never reach this, because no group holds a shared file.
   *
   * @param {import('../model/pipelines.js').LogicalModel} entry
   * @returns {string|null} the reason's key
   */
  function fileDeleteHeld(entry) {
    return !entry.group && entry.disables.length > 1 && neededNow(entry.id) ? 'settings.models.fileShared' : null
  }

  /**
   * What the readiness row adds when the runtime is not there: whole keys,
   * chosen rather than built, so the catalogue test can find each one.
   */
  const RUNTIME_LINES = {
    missing: 'settings.detection.ready.runtime',
    downloading: 'settings.detection.ready.runtimeDownloading',
    unavailable: 'settings.detection.ready.runtimeUnavailable',
    checking: 'settings.detection.ready.runtimeChecking',
    unchecked: 'settings.detection.ready.runtimeUnchecked',
  }

  /**
   * The one line under the policy: can the selected workflow run, and what
   * would make it. Built from the same `workflowNeeds` the downloads use, so
   * the sentence and the button can never disagree with setup.
   *
   * **The runtime counts.** A native run refuses to start without ONNX
   * Runtime, so a workflow whose files are all here is still not complete
   * while it is missing, and the row names it rather than going green. The
   * same holds for a runtime that is here and will not load: the row says
   * why, in the words `diagnostics` chose for that failure's remedy.
   */
  const readiness = $derived.by(() => {
    if (!catalogue) return null
    /** @type {string[]} */
    const lines = []
    /** @type {import('../model/pipelines.js').LogicalModel[]} */
    const toDownload = []
    let bytes = 0
    let fetching = false
    let samHeld = false
    let importMissing = false
    for (const id of needs) {
      // On this computer the text reader is an extra: cleaning runs without
      // it, and its switch says what is missing and offers the download on its
      // own line. Under Cloud GPU it is one of the four the run uses, and Text
      // cleanup will not start without it, so it counts like the rest.
      if (!neededNow(id) || (id === 'hayaiOcr' && runDetection(session).target !== 'cloud')) continue
      const entry = pipelineModel(id)
      if (!entry) continue
      const view = viewOf(entry)
      if (entry.source === 'download' && view.known && !view.installed) {
        if (view.downloading) fetching = true
        else toDownload.push(entry)
        bytes += view.missingBytes ?? 0
      }
      if (entry.source === 'import' && view.known && !view.installed) importMissing = true
    }
    if (!allText && needs.length === 0) lines.push(t('settings.detection.ready.nothing'))
    else if (!allText) {
      lines.push(bytes > 0
        ? t('settings.detection.ready.legacyMissing', { bytes })
        : t('settings.detection.ready.legacy'))
      if (importMissing) lines.push(t('settings.detection.ready.allTextImport'))
    } else {
      // The review refuses SAM graphs that failed their checksum, and SAM on
      // a computer without the free memory for it (`readinessKeyOf`), so the
      // row does too. Graphs not checked yet do not hold it back: the review
      // checks them itself when it opens.
      const samHere = workflowCaps?.samInstalled === true
      samHeld = samHere && (samVerified === false || workflowCaps.samMemoryReady === false)
      if (importMissing) lines.push(t('settings.detection.ready.allTextImport'))
      if (bytes > 0) lines.push(t('settings.detection.ready.allTextMissing', { bytes }))
      if (samHere && samVerified === false) lines.push(t('settings.detection.ready.allTextSamMismatch'))
      if (samHere && workflowCaps.samMemoryReady === false) lines.push(t('settings.detection.ready.allTextMemory'))
      if (!importMissing && bytes === 0 && workflowCaps && !samHeld) lines.push(t('settings.detection.ready.allText'))
    }
    // A live progress event is a download the catalogue snapshot may not know
    // about yet; it is still not a runtime that is here.
    const row = catalogue.runtime
    const runtime = runtimeState(
      row ? { ...row, downloading: row.downloading === true || Boolean(progress[RUNTIME_ID]) } : null,
      needs,
      runtimeLoad.state,
    )
    const runtimeReady = runtime === 'notNeeded' || runtime === 'installed'
    if (runtime === 'unloadable') {
      lines.push(t('settings.detection.ready.runtimeUnloadable', { reasonKey: runtimeLoad.reasonKey ?? 'diagnostics.runtime.unloadable' }))
    } else if (!runtimeReady) lines.push(t(RUNTIME_LINES[runtime]))
    let backendBlocked = false
    for (const id of needs) {
      const placement = accelerators?.models.find((model) => model.id === id)
      if (!placement || placement.preference === 'auto') continue
      const status = placement.backendStatus?.find((entry) => entry.id === placement.preference)
      if (status?.available) continue
      backendBlocked = true
      lines.push(t('settings.accel.unavailableForModel', {
        model: placement.modelName ?? pipelineModel(id)?.product ?? id,
        backend: t(`accel.${placement.preference}`),
        reason: t(status?.reasonKey ?? 'settings.accel.state.unsupported'),
      }))
    }
    const missing = toDownload.reduce((sum, entry) => sum + (viewOf(entry).missingBytes ?? 0), 0)
    const complete = bytes === 0 && !importMissing && !samHeld && !backendBlocked && (!allText || Boolean(workflowCaps)) && runtimeReady
    return { lines, toDownload, missing, fetching, complete, runtime }
  })

  /** Download everything the selected workflow is missing, each as its own unit. */
  async function downloadNeeded() {
    for (const entry of readiness?.toDownload ?? []) {
      if (entry.group) {
        const group = groupById(entry.group)
        if (group) await downloadGroup(group)
        continue
      }
      for (const row of /** @type {any[]} */ (filesOf(entry))) if (!row.installed) await download(row.id)
    }
    if (needs.includes('samTs') && workflowCaps?.samInstalled !== true) await installSamTs()
  }

  /**
   * The text reader switch, honestly: whether it can run with what is here.
   * `null` says nothing needs saying.
   */
  const rescueStatus = $derived.by(() => {
    if (!runSelection.ocrRescue) return null
    const entry = pipelineModel('hayaiOcr')
    const view = entry && catalogue ? viewOf(entry) : null
    if (!view || !view.known || view.installed) return null
    return { key: 'settings.detection.rescue.missing', download: true, view }
  })

  /* ---------- imports ---------- */

  /** @param {import('../model/pipelines.js').LogicalModel} entry */
  async function importModel(entry) {
    const sam = entry.importId === 'samTs'
    /** @type {string|null} */
    let path = null
    try {
      path = sam
        ? await chooseFolder({ title: t('settings.detection.sam.chooserTitle') })
        : await chooseOnnx({ title: t('settings.detection.rtFull.chooserTitle') })
    } catch {
      // A chooser that could not open chose nothing, which is also what
      // closing it does.
      path = null
    }
    if (!path) return
    importBusy = { ...importBusy, [entry.id]: true }
    importFailures = without(importFailures, entry.id)
    try {
      if (sam) {
        await getBackend().importSamTs({ sourceDir: path })
        // The import verifies both graphs against the pinned manifest.
        samVerified = true
      } else {
        await getBackend().importFullRt({ sourcePath: path })
      }
    } catch (error) {
      importFailures = { ...importFailures, [entry.id]: String(error) }
    } finally {
      importBusy = { ...importBusy, [entry.id]: false }
      await refreshWorkflowCaps()
    }
  }

  async function installSamTs() {
    importBusy = { ...importBusy, samTs: true }
    importFailures = without(importFailures, 'samTs')
    try {
      await getBackend().installSamTs()
      samVerified = true
    } catch (error) {
      importFailures = { ...importFailures, samTs: String(error) }
    } finally {
      importBusy = { ...importBusy, samTs: false }
      await refreshWorkflowCaps()
    }
  }

  /** SAM only: the full graph is digested on every readiness answer. @param {import('../model/pipelines.js').LogicalModel} entry */
  async function checkImport(entry) {
    importFailures = without(importFailures, entry.id)
    importBusy = { ...importBusy, [entry.id]: true }
    try {
      samVerified = (await getBackend().verifySamTs()) === true
    } catch (error) {
      samVerified = false
      importFailures = { ...importFailures, [entry.id]: String(error) }
    } finally {
      importBusy = { ...importBusy, [entry.id]: false }
      await refreshWorkflowCaps()
    }
  }

  /** @param {import('../model/pipelines.js').LogicalModel} entry */
  async function removeImport(entry) {
    importFailures = without(importFailures, entry.id)
    importBusy = { ...importBusy, [entry.id]: true }
    try {
      if (entry.importId === 'samTs') {
        await getBackend().removeSamTs()
        samVerified = null
      } else {
        await getBackend().removeFullRt()
      }
    } catch (error) {
      importFailures = { ...importFailures, [entry.id]: String(error) }
    } finally {
      importBusy = { ...importBusy, [entry.id]: false }
      await refreshWorkflowCaps()
    }
  }

  /* ---------- removal, confirmed ---------- */

  /**
   * The removal waiting for a yes: which model (or, for a file no model
   * claims, which file), and where focus goes if the answer is Keep.
   *
   * Inline rather than a modal: the question belongs to one row, and the row
   * is where the reader is looking. Settings is itself a layer, and a second
   * one over it for a two-button question is interruption without purpose.
   *
   * @type {{modelId: string|null, fileId: string|null, returnId: string}|null}
   */
  let confirming = $state(null)

  /** @param {string} id */
  const rowId = (id) => `${uid}-model-${id}`

  /**
   * @param {string|null} modelId
   * @param {string|null} fileId
   * @param {string} returnId - the id of the Delete that asked
   */
  async function askRemove(modelId, fileId, returnId) {
    confirming = { modelId, fileId, returnId }
    await tick()
    document.getElementById(`${uid}-keep`)?.focus()
  }

  async function keep() {
    const back = confirming?.returnId
    confirming = null
    await tick()
    if (back) document.getElementById(back)?.focus()
  }

  async function confirmRemove() {
    const pending = confirming
    confirming = null
    if (!pending) return
    const entry = pending.modelId ? pipelineModel(pending.modelId) : null
    if (entry?.source === 'import') await removeImport(entry)
    else if (entry?.group) {
      const group = groupById(entry.group)
      if (group) await removeGroup(group)
    } else {
      const id = pending.fileId ?? entry?.files[0]
      if (id) await remove(id)
    }
    await tick()
    // The row stays; its Delete may not. Its first action is where the
    // reader carries on from.
    const row = document.getElementById(rowId(pending.modelId ?? pending.fileId ?? ''))
    const next = /** @type {HTMLElement|null} */ (row?.querySelector('.row-actions button:not(:disabled)') ?? null)
    next?.focus()
  }

  /** Escape answers Keep, and does not also close Settings. @param {KeyboardEvent} event */
  function onconfirmkeydown(event) {
    if (event.key !== 'Escape') return
    event.preventDefault()
    event.stopPropagation()
    keep()
  }

  /** The removal question, as the sentences it is drawn from. */
  const confirmLines = $derived.by(() => {
    if (!confirming) return []
    const entry = confirming.modelId ? pipelineModel(confirming.modelId) : null
    if (!entry || !entry.removeKey) {
      const row = catalogue?.models.find((model) => model.id === confirming?.fileId)
      return [t('settings.models.remove.file', { name: row ? t(row.kindKey) : String(confirming.fileId) })]
    }
    const lines = [t(entry.removeKey)]
    if (usedNow(entry.id, choices) && neededNow(entry.id)) lines.push(t('settings.models.remove.inUse'))
    return lines
  })

  /** Whether the review panel is open. Open by default only under all-text. */
  let reviewOpen = $state(untrack(() => session.textPolicy === ALL_TEXT_POLICY))

  /** @param {boolean} open */
  function toggleReview(open) {
    reviewOpen = open
    // The panel imports and removes graphs on its own; the rows above read
    // the same answer again once it is put away.
    if (!open) refreshWorkflowCaps()
  }

  /**
   * Tick or untick one detection model. Full and Small are one model's two
   * profiles, so choosing one clears the other; the last model selected
   * cannot be cleared (`toggleDetectorModel`).
   *
   * @param {string} id
   * @param {boolean} checked
   * @param {HTMLInputElement} input
   */
  function chooseDetectorModel(id, checked, input) {
    // On the cloud GPU the combination is fixed; the boxes are drawn disabled.
    if (cloudFixed) {
      input.checked = runSelection.detectorModels.includes(id)
      return
    }
    const next = toggleDetectorModel(session.detectorModels, id, checked)
    // A refused change leaves the box as the selection says.
    input.checked = next.includes(id)
    if (next.join() === session.detectorModels.join()) return
    setDetectorModels(next)
    void loadCapabilities()
  }

  /**
   * The selection a run uses (`pipelines.js#runDetection`) and where each of
   * its models runs, as the editor's Detect on routes it. Only said here,
   * never chosen. On the cloud GPU the combination is fixed to the best one,
   * so the boxes show it checked and cannot be changed; the user's own
   * combination and reader switch wait in the session for This computer.
   */
  const runSelection = $derived(runDetection(session))
  const placement = $derived(detectionPlacement(session))
  const cloudFixed = $derived(runSelection.target === 'cloud')

  // The editor's Detect on asks the endpoint what it offers and keeps the
  // answer (`state/cloudtargets.svelte.js`). Opening Settings asks again, so a
  // worker updated from the Cloud section is read afresh when the editor
  // shows its choice next.
  resetCloudOffer()
  $effect(() => syncCloudOffer())

  /** Which model rows have their file details open. @type {Record<string, boolean>} */
  let detailsOpen = $state({})

  /**
   * A row's meta line: file count for a multi-file model, total size, one
   * state, and the facts that qualify it.
   *
   * @param {any} view - what `viewOf` answered
   */
  function metaOf(view) {
    const parts = []
    if (view.kind === 'download' && view.total > 1) parts.push(t('settings.models.fileCount', { count: view.total }))
    if (view.bytes) parts.push(t('models.value.size', { bytes: view.bytes }))
    parts.push(t(view.state.key, view.state.params))
    if (view.kind === 'download' && view.readOnly) parts.push(t('settings.models.status.readOnly'))
    if (view.kind === 'import' && view.installed && !view.managed) parts.push(t('settings.models.status.readOnly'))
    if (view.verified && !view.downloading) parts.push(t('settings.models.status.verified'))
    return parts.join(' · ')
  }

  /** Whether this backend can import the graph at all. @param {import('../model/pipelines.js').LogicalModel} entry */
  function canImport(entry) {
    const backend = getBackend()
    return typeof (entry.importId === 'samTs' ? backend.importSamTs : backend.importFullRt) === 'function'
  }

  /** The reader switch's own Download: the Hayai group, as one unit. */
  async function downloadRescue() {
    const group = groupById('hayaiOcr')
    if (group) await downloadGroup(group)
  }

  /** The logical models a Models group lists, in its order. @param {string} group */
  function groupEntries(group) {
    return (CAPABILITIES.find((entry) => entry.group === group)?.models ?? [])
      .map((id) => pipelineModel(id))
      .filter((entry) => entry !== null)
  }
  const detectEntries = groupEntries('detection')
  const filteringEntries = groupEntries('filtering')
  const cleaningEntries = groupEntries('cleaning')

  /** Which collapsed groups are open. Deep links open the one they name. */
  let filteringOpen = $state(false)
  let fluxOpen = $state(false)
  let backendsOpen = $state(false)

  /**
   * What the collapsed language filtering says while shut: whether it runs
   * (the Text cleanup panel's text choice decides), and a failed download or
   * check on either of its rows, so a failure inside it is not hidden behind
   * the summary.
   */
  const filteringNote = $derived.by(() => {
    const policy = t(allText ? 'settings.detection.filtering.off' : 'settings.detection.filtering.on')
    const failed = filteringEntries.some((entry) => {
      const view = catalogue ? viewOf(entry) : null
      return view && (view.state.key === 'settings.models.status.failed' || view.state.key === 'settings.models.status.mismatch')
    })
    return failed ? `${policy} · ${t('settings.models.status.failed')}` : policy
  })

  /** The id the ONNX Runtime's own download reports under (`src-tauri/src/weights.rs`). */
  const RUNTIME_ID = 'runtime'

  /**
   * Which ONNX Runtime build this machine should fetch.
   *
   * Only Windows publishes more than one - DirectML by default, CUDA 12 and
   * CUDA 13 offered - so the picker is drawn only where there is something to
   * pick, which is what `flavours.length > 1` says without this file knowing
   * which platform it is on. The id is an ordinary setting; the
   * backend resolves anything it does not publish back to the default, so a
   * stale value cannot leave a machine unable to download a runtime at all.
   *
   * It does not re-download by itself. Switching build is 200-455 MB and the
   * press for that is the row's own Download button, which is offered beside
   * Delete wherever there is a choice.
   *
   * @param {string} id
   */
  async function chooseFlavour(id) {
    await writeSettingsSerialized(getBackend(), { runtimeFlavour: id })
    await refreshCatalogue()
  }

  /**
   * One line under the picker for a build that needs something installed first.
   *
   * `userInstalled` is version strings - `CUDA 12.x`, `cuDNN 9.x` - which are
   * data and are interpolated rather than translated, the same way a licence
   * identifier is. Empty for every build but CUDA, and that emptiness
   * is the argument for the default.
   */
  /**
   * The line that says the machine is running a build other than the chosen
   * one, and says nothing at all the rest of the time.
   *
   * Two facts sat side by side and never met: the row's
   * `flavour` is what a Download press *would* fetch, and until that press
   * happens the computer is still running whatever it unpacked last. Both are
   * true; on screen together, with only the first named, they read as a claim
   * about the library on disk. `installedFlavour` is null for a runtime this
   * application did not unpack - unknown rather than none - and an unknown
   * build is nothing to report a difference about.
   */
  const installedNote = $derived.by(() => {
    const runtime = catalogue?.runtime
    if (!runtime?.installed || !runtime.installedFlavour) return null
    const same =
      runtime.installedFlavour === runtime.flavour &&
      runtime.installedVersion === runtime.version
    if (same) return null
    return t('settings.models.runtime.installedDiffers', {
      installed: runtime.installedFlavour,
      installedVersion: runtime.installedVersion,
      chosen: runtime.flavour,
      chosenVersion: runtime.version,
    })
  })

  const flavourNeeds = $derived.by(() => {
    const chosen = catalogue?.runtime.flavours.find((row) => row.id === catalogue?.runtime.flavour)
    if (!chosen || chosen.userInstalled.length === 0) return null
    return t('settings.models.runtime.flavourNeeds', { items: chosen.userInstalled.join(', ') })
  })

  /* ---------- the Hugging Face token ---------- */

  /**
   * The field's contents, which are **write-only**.
   *
   * The stored token is never read back across the seam - `listModels` answers
   * `hasToken` and `tokenStore` and nothing more - so this box starts empty on
   * every open, and "a token is saved" is said in words beside it rather than
   * by filling a field with dots. A password input whose value came from the
   * backend would be a secret sitting in the DOM for the length of a dialog,
   * for no gain: the user cannot read it through the dots anyway.
   */
  let tokenDraft = $state('')

  /**
   * Which sentence goes under the field.
   *
   * The token lives in the operating system's credential store; a computer
   * without one keeps it in `settings.json` instead. That is a
   * difference the user is entitled to know about *before* they paste a token,
   * so the fallback is said whether or not one is saved, and the saved note
   * says which of the two places it went.
   *
   * Three states, not two, because "this build has no credential store" and
   * "there is one and it would not answer" are opposite pieces of news: the
   * first is permanent and the second is usually a locked keychain the user can
   * unlock.
   */
  const tokenNote = $derived.by(() => {
    if (!catalogue) return null
    if (catalogue.tokenStore === 'keychain') {
      return catalogue.hasToken ? t('settings.models.token.saved') : null
    }
    // The two fallbacks read almost the same and mean opposite things, so each
    // has its own sentence rather than sharing one about "no credential store".
    const unreachable = catalogue.tokenStore === 'fileStoreUnavailable'
    if (catalogue.hasToken) {
      return t(unreachable ? 'settings.models.token.savedStoreUnreachable' : 'settings.models.token.savedInFile')
    }
    return t(unreachable ? 'settings.models.token.storeUnreachable' : 'settings.models.token.fileOnly')
  })

  /**
   * The sentence for *why* the store would not answer.
   *
   * Whole keys in a table rather than one assembled from the id, for the reason
   * `DECLINED` above gives: a key built from a value cannot be found by the
   * catalogue's scan, and a dead string survives behind a template. An id with
   * no entry - or none at all, which is every location but the unreachable one
   * - says nothing, and the sentence above it is still true on its own.
   */
  const STORE_REASON = {
    locked: 'settings.models.token.reason.locked',
    unreachable: 'settings.models.token.reason.unreachable',
    ambiguous: 'settings.models.token.reason.ambiguous',
    unknown: 'settings.models.token.reason.unknown',
  }

  /**
   * One line under the note, naming what the store did.
   *
   * Separate from `tokenNote` rather than four more variants of it: the two
   * sentences answer different questions - where the token is, and what is
   * wrong with the place it should be - and a locked keychain reads the same
   * whether or not a token has been saved yet.
   */
  const tokenReason = $derived.by(() => {
    const reason = catalogue?.tokenStoreReason
    if (catalogue?.tokenStore !== 'fileStoreUnavailable' || !reason) return null
    const key = STORE_REASON[reason]
    return key ? t(key) : null
  })

  async function saveToken() {
    const value = tokenDraft.trim()
    if (!value) return
    tokenFailure = false
    await writeSettingsSerialized(getBackend(), { hfToken: value })
    tokenDraft = ''
    await refreshCatalogue()
  }

  /**
   * The sentence `settings::write` rejects with when the credential store kept
   * the secret - `format!("the credential store kept the token: {err}")`, from
   * `TokenWrite::NotCleared`.
   *
   * **Matched as a string, deliberately and unhappily.** The seam carries one
   * `Err(String)` per call and there is no id on it, so the
   * only thing that separates "your token is still in the keychain" from "the
   * settings file could not be written" is the words the backend chose. The
   * two are opposite instructions - one sends the user to their keychain, the
   * other says nothing was changed and to try again - so telling them apart on
   * the prefix is better than saying the alarming one for every failure. A
   * `reasonKey` on the rejection would end this, and is the row's remedy.
   */
  const TOKEN_KEPT = 'the credential store kept the token'

  /**
   * Clear the stored token.
   *
   * The press can genuinely fail, in two ways that read nothing alike. A
   * credential store that refuses to delete leaves the secret in it, and the
   * backend rejects rather than removing the settings key and reporting a
   * deletion that did not happen - the user has to hear that,
   * because the remedy, their keychain, is somewhere this dialog cannot reach.
   * Any other rejection - an unwritable settings file, an adapter that is not
   * answering - is a write that did not happen at all, and telling that user
   * their token is still in a credential store would send them looking for a
   * secret that is not there.
   */
  async function clearToken() {
    tokenFailure = false
    try {
      await writeSettingsSerialized(getBackend(), { hfToken: '' })
    } catch (error) {
      tokenFailure = String(error).includes(TOKEN_KEPT)
        ? 'settings.models.token.clearFailed'
        : 'settings.models.token.clearFailedOther'
      return
    }
    tokenDraft = ''
    await refreshCatalogue()
  }

  /**
   * Which sentence the last Clear earned, or `false` for a press that did not
   * fail. A key rather than a boolean because there are two failures and they
   * are opposite news; `false` because `saveToken` clears it that way and a
   * falsy value is what the template tests.
   *
   * @type {string|false}
   */
  let tokenFailure = $state(/** @type {string|false} */ (false))

  /* ---------- Acceleration ---------- */

  /** @type {import('../api/backend.js').Accelerators|null} */
  let accelerators = $state(null)

  /**
   * Whether the last `listAccelerators` was **rejected**, as against not having
   * been asked for yet. `accelerators === null` cannot tell those two apart,
   * and the difference is the whole of what this panel has to say: a list not
   * yet asked for draws nothing, a list that was refused has to explain why
   * Automatic is the only thing on offer.
   */
  let accelFailure = $state(false)

  async function refreshAccelerators() {
    try {
      accelerators = await getBackend().listAccelerators()
      accelFailure = false
    } catch (error) {
      // Caught and dropped until now, which on the machine this matters most on
      // said nothing at all: a Windows install whose ONNX Runtime will not load
      // - the missing Microsoft redistributable - answers nothing here, and the
      // picker collapsed to a bare Automatic with no explanation beside it. The
      // *reason* is the runtime's own and is reported on the runtime's row; the
      // panel's share is that the list is missing and where the reason is.
      console.error('listAccelerators was rejected', error)
      accelerators = null
      accelFailure = true
    }
  }

  /**
   * Asked for when the Performance panel is shown, not when the screen
   * mounts.
   *
   * The list is the loaded runtime answering, so asking for it maps the
   * runtime's library into this process - and Windows will not replace a file
   * that is mapped, which is exactly what the runtime row's Download has to
   * do. Opening Settings to fetch a runtime must not be the thing that makes
   * the fetch impossible, so the question waits until the panel that shows the
   * answer is actually looked at.
   *
   * Asked once, unless it was refused: a refusal is asked again the next time
   * the panel is shown, which is after a runtime download has had its chance
   * to put the cause right. Only `active` is tracked, so a refusal does not
   * retry itself in a loop.
   */
  let accelAsked = false
  $effect(() => {
    if (active !== 'performance' && !(active === 'models' && runtimeLoad.state === 'loaded')) return
    untrack(() => {
      if (accelAsked && !accelFailure) return
      accelAsked = true
      refreshAccelerators()
    })
  })

  /**
   * Whether `Automatic` is itself a guess.
   *
   * It is, when the setting in force puts models on providers and not one of
   * those has ever been timed on this kind of machine - which is every Windows
   * GPU today. `active` is the backend saying which providers the current
   * setting actually uses, and this is what it is for.
   *
   * Asked **only while Automatic is the setting in force**, which is what the
   * absence of a `selected` provider means. `active` describes the current
   * placement, so under a forced provider it says nothing about what Automatic
   * would have done, and reading it there would put a caveat on a choice
   * nobody had made. An empty list says nothing either: no rows is not
   * evidence of anything.
   */
  const autoUnmeasured = $derived.by(() => {
    const providers = accelerators?.providers ?? []
    if (providers.some((provider) => provider.selected)) return false
    const inUse = providers.filter((provider) => provider.active)
    return inUse.length > 0 && inUse.every((provider) => !provider.measured)
  })

  /**
   * Which option the picker shows as the current one.
   *
   * `selected` is the backend's own reading of the stored preference, so once
   * the list has been read it is the better answer than the session copy - the
   * two differ whenever a stored value was not one the backend kept. Before the
   * list has been read there is nothing to compare against, and the session's
   * value is all there is.
   */
  const acceleratorValue = $derived(
    accelerators
      ? (accelerators.providers.find((provider) => provider.selected)?.id ?? 'auto')
      : session.accelerator,
  )

  /**
   * `Automatic`, then every provider the runtime reports - the unusable ones
   * included, disabled, with the reason on them.
   *
   * Shown rather than filtered out for the same reason the cloud rung is shown
   * disabled: a user looking for CUDA and finding no entry at all concludes the
   * application does not support it, where a disabled entry saying "it needs
   * CUDA and cuDNN installed on this machine" is an instruction.
   *
   * `note` is one field for two different things, because the option has one
   * line to say either in: why a provider cannot be picked, or - for one that
   * can - that picking it rests on how the provider works rather than on a
   * timing taken here. The second half is `measured`, which crossed the wire
   * from the start and was read by nothing, so every Windows choice was offered
   * as though it had been measured.
   */
  const acceleratorOptions = $derived([
    {
      id: 'auto',
      label: t('settings.accel.auto'),
      disabled: false,
      note: autoUnmeasured ? t('accel.chosen.unmeasured') : undefined,
    },
    ...(accelerators?.providers ?? []).map((provider) => ({
      id: provider.id,
      label: t(provider.labelKey),
      disabled: !provider.available,
      note: !provider.available
        ? provider.reasonKey
          ? t(provider.reasonKey)
          : undefined
        : provider.measured
          ? undefined
          : t('accel.chosen.unmeasured'),
    })),
  ])

  /**
   * The options the picker draws. A stored id the list does not offer - a
   * provider this runtime no longer reports, or any id before the list has
   * been read - is drawn as itself rather than left out: a native select with
   * no matching option shows its first one, and the picker would read
   * Automatic while the setting in force was something else.
   */
  const acceleratorChoices = $derived.by(() => {
    const rows = acceleratorOptions.map((option) => ({
      value: option.id,
      label: option.note ? `${option.label}: ${option.note}` : option.label,
      disabled: option.disabled,
      title: option.note,
    }))
    if (rows.some((row) => row.value === acceleratorValue)) return rows
    return [...rows, { value: acceleratorValue, label: t('settings.accel.saved', { id: acceleratorValue }) }]
  })

  /**
   * One line per model: where it will run, and the caveat if there is one.
   *
   * Three things can be true of a row and all three are said: the provider it
   * landed on, whether that was a measurement or a guess (`accel.chosen.*`),
   * and - where a forced provider was refused - which one and why, with the two
   * byte figures beside the sentence rather than inside it, because a byte
   * count is not translatable.
   *
   * @param {{modelKey: string, labelKey: string, noteKey: string|null, declinedKey: string|null, declinedId: string|null, neededBytes: number|null, roomBytes: number|null}} row
   */
  function placementOf(row) {
    if (row.declinedKey && row.preference !== 'auto') {
      const wanted = row.declinedId ? t(`accel.${row.declinedId}`) : t(`accel.${row.preference}`)
      return t('settings.accel.refused', { backend: wanted, reason: t(row.declinedKey) })
    }
    const parts = [t(row.labelKey)]
    if (row.noteKey) parts.push(t(row.noteKey))
    if (row.declinedKey) {
      const which = row.declinedId ? t(`accel.${row.declinedId}`) : ''
      const reason = t(row.declinedKey)
      const sizes =
        row.neededBytes !== null && row.roomBytes !== null
          ? ` (${t('models.value.size', { bytes: row.neededBytes })} / ${t('models.value.size', { bytes: row.roomBytes })})`
          : ''
      parts.push(`${which}: ${reason}${sizes}`.trim())
    }
    return parts.join(' · ')
  }

  async function chooseAccelerator(id) {
    setAccelerator(id)
    await push()
    await refreshAccelerators()
  }

  let modelAccelFailure = $state(false)

  /** The native capability matrix is the source of selectable backends. */
  function modelBackendChoices(row) {
    const inherited = session.accelerator === 'auto'
      ? t('settings.accel.auto')
      : t(`accel.${session.accelerator}`)
    const choices = [
      { value: 'inherit', label: t('settings.accel.inherit', { backend: inherited }) },
      { value: 'auto', label: t('settings.accel.auto') },
    ]
    for (const status of row.backendStatus ?? []) {
      const provider = accelerators?.providers.find((entry) => entry.id === status.id)
      const name = provider ? t(provider.labelKey) : status.id
      const level = status.verified ? 'verified'
        : status.available ? 'available'
          : status.installed ? 'installed'
            : status.supported ? 'supported' : 'unsupported'
      const note = status.reasonKey ? t(status.reasonKey) : t(`settings.accel.state.${level}`)
      choices.push({
        value: status.id,
        label: `${name} · ${note}`,
        disabled: !status.supported || !status.available,
        title: note,
      })
    }
    const saved = session.modelAccelerators[row.id]
    if (saved && !choices.some((choice) => choice.value === saved)) {
      choices.push({ value: saved, label: t('settings.accel.saved', { id: saved }) })
    }
    return choices
  }

  async function chooseModelAccelerator(modelId, id) {
    const before = session.modelAccelerators[modelId] ?? 'inherit'
    modelAccelFailure = false
    setModelAccelerator(modelId, id)
    try {
      await push()
      await refreshAccelerators()
    } catch {
      setModelAccelerator(modelId, before)
      modelAccelFailure = true
    }
  }

  function modelDisplayName(row) {
    return row.modelName ?? pipelineModel(row.id)?.product ?? (row.id === 'inpainter' ? CLEANERS[0].name : t(row.modelKey))
  }

  let choosing = $state(false)

  /**
   * What the folder field holds while it is being typed into, or `null` when
   * it is showing the stored path. The path is committed on blur and on
   * Enter, not per keystroke: each commit is a settings write, a capability
   * probe and a model listing, and a half-typed path is none of those.
   *
   * @type {string|null}
   */
  let sidecarDraft = $state(null)
  let installingFlux = $state(false)
  let fluxInstallStage = $state('')
  let fluxInstallError = $state('')
  let fluxAccelerator = $state('auto')
  const fluxStageKey = $derived({
    environment: 'settings.sidecar.stage.environment',
    dependencies: 'settings.sidecar.stage.dependencies',
    weights: 'settings.sidecar.stage.weights',
    ready: 'settings.sidecar.stage.ready',
  }[fluxInstallStage] ?? 'settings.sidecar.stage.environment')

  async function installFlux() {
    if (installingFlux) return
    installingFlux = true
    fluxInstallStage = 'environment'
    fluxInstallError = ''
    let unlisten = /** @type {null|(() => void)} */ (null)
    try {
      if (window.__TAURI_INTERNALS__) {
        const { listen } = await import('@tauri-apps/api/event')
        unlisten = await listen('flux-install://progress', (event) => {
          const step = event.payload?.step
          if (['environment', 'dependencies', 'weights', 'ready'].includes(step)) fluxInstallStage = step
        })
      }
      await getBackend().installFluxHelper({ backend: session.fluxBackend, accelerator: fluxUsesMlx ? 'auto' : fluxAccelerator })
      setFluxModel('flux2-klein-4b')
      await writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
      await loadCapabilities()
      await loadModels()
    } catch (error) {
      fluxInstallError = error instanceof Error ? error.message : String(error)
    } finally {
      unlisten?.()
      installingFlux = false
    }
  }

  function commitSidecar() {
    if (sidecarDraft === null) return
    const next = sidecarDraft
    sidecarDraft = null
    if (next === session.sidecarPath) return
    setSidecarPath(next)
    push()
  }

  async function browseSidecar() {
    if (choosing) return
    choosing = true
    try {
      const chosen = await chooseFolder({
        title: t('settings.sidecar.chooserTitle'),
        defaultPath: session.sidecarPath || undefined,
      })
      if (chosen !== null) {
        sidecarDraft = null
        setSidecarPath(chosen)
        await push()
      }
    } finally {
      choosing = false
    }
  }

  /**
   * The helper's models as options. A stored model the helper does not list
   * is drawn as itself, for the reason `acceleratorChoices` gives; an empty
   * list with nothing stored is one disabled line saying so.
   */
  const sidecarChoices = $derived.by(() => {
    const rows = sidecarModels.map((model) => ({ value: model.id, label: model.label }))
    if (session.fluxModel && !rows.some((row) => row.value === session.fluxModel)) {
      rows.push({ value: session.fluxModel, label: t('settings.sidecarModel.missing', { id: session.fluxModel }) })
    }
    return rows.length > 0 ? rows : [{ value: '', label: t('settings.sidecarModel.noneFound') }]
  })

  const themes = THEMES.map((value) => ({ value, label: t(THEME_LABEL_KEYS[value]) }))
  const directions = [
    { value: 'rtl', label: t('settings.direction.rtl') },
    { value: 'ltr', label: t('settings.direction.ltr') },
  ]
  const originalViews = [
    { value: 'hold', label: t('settings.originalView.hold') },
    { value: 'pinned', label: t('settings.originalView.pinned') },
  ]
  /**
   * Which sidecar backend the AI redraw rung asks for.
   *
   * Three options and not two, because `Auto` is the honest default: which
   * runtime is better on a given machine is a question about that machine, the
   * core answers it per platform (`mflux` on Apple Silicon, `sdnq` elsewhere),
   * and hiding that behind a two-way switch would make every Mac user choose
   * between two things they have no way to compare. MLX stays visible but
   * disabled outside Apple Silicon, with its platform requirement on screen.
   */
  const fluxPlatform = $derived(catalogue?.runtime?.platform ?? null)
  const fluxUsesMlx = $derived(session.fluxBackend === 'mflux' || (session.fluxBackend === 'auto' && fluxPlatform === 'macos-arm64'))
  const fluxBackends = $derived([
    { value: 'auto', label: t('settings.fluxBackend.auto') },
    {
      value: 'mflux',
      label: fluxPlatform === 'macos-arm64' ? t('settings.fluxBackend.mflux') : t('settings.fluxBackend.mfluxUnsupported'),
      disabled: fluxPlatform !== 'macos-arm64',
      title: t('settings.fluxBackend.mfluxReason'),
    },
    { value: 'sdnq', label: t('settings.fluxBackend.sdnq') },
  ])
  const fluxAcceleratorChoices = $derived([
    { value: 'auto', label: t('settings.sidecar.acceleratorAuto') },
    { value: 'cuda', label: 'NVIDIA CUDA', disabled: fluxPlatform?.startsWith('macos-') === true },
    { value: 'xpu', label: 'Intel XPU', disabled: fluxPlatform?.startsWith('macos-') === true },
    { value: 'mps', label: 'Apple Metal', disabled: fluxPlatform?.startsWith('macos-') !== true },
  ])
  const fluxAcceleratorAllowed = $derived(fluxUsesMlx || fluxAcceleratorChoices.some((entry) => entry.value === fluxAccelerator && !entry.disabled))

  /** Each language by its own name, so it can be found from any other. */
  const languages = LOCALES.map(({ tag, name }) => ({ value: tag, label: name }))


  /* ---------- the two pipelines ---------- */

  /** What a language row stores for "skip this language". */
  const SKIP = ''

  /** @param {string} language */
  function detectorOptions(language) {
    return [
      { value: 'ctd-rtdetr', label: t('pipelines.clean') },
      { value: SKIP, label: t('pipelines.skip') },
    ]
  }

  /**
   * Catalogue rows no logical model claims. Normally empty; a weight the
   * backend added before the capability graph knew it still has a place in
   * Models to be checked and deleted from.
   */
  // The page denoise packages are Denoise's own rows, not stray files.
  const unclaimed = $derived(catalogue?.models.filter((model) => !modelOfFile(model.id) && !model.requiredBy?.includes(PAGE_DENOISE_FEATURE)) ?? [])

  // The backend discovers managed, bundled and environment-provided installs
  // too. The folder setting is an optional override, not a setup requirement.
  const fluxReady = $derived(capabilities.sidecar)

  /* ---------- the tab list ---------- */

  /**
   * The sections, in the order they are offered (`settingslinks.js`). Older
   * ids - `detection`, `cleaning`, `inference` - resolve there too.
   */
  const TAB_META = /** @type {Record<string, {labelKey: string, icon: string}>} */ ({
    general: { labelKey: 'settings.section.general', icon: 'sliders' },
    models: { labelKey: 'settings.section.models', icon: 'search' },
    cloud: { labelKey: 'settings.section.cloud', icon: 'cloud' },
    denoise: { labelKey: 'settings.section.denoise', icon: 'wand' },
    performance: { labelKey: 'settings.section.performance', icon: 'cpu' },
    shortcuts: { labelKey: 'settings.section.shortcuts', icon: 'keyboard' },
    about: { labelKey: 'settings.section.about', icon: 'info' },
  })
  const TABS = SETTINGS_SECTIONS.map((id) => ({ id, ...TAB_META[id] }))

  // A caller may open Settings on a section and at a place inside it
  // (`openCloudSettings` asks for Cloud, a missing-model link for one row).
  // Read once: after that the list owns it.
  const opening = untrack(() => resolveSettingsLink(spec?.props?.tab, spec?.props?.anchor))
  let active = $state(opening.section)

  /**
   * Go to one place inside the section on screen: open the collapsed group
   * that holds it, scroll it into view, and move focus to it, so the reader
   * lands on the row that needs them rather than on the back button.
   *
   * What takes focus: the anchor itself when it is a focus target
   * (`tabindex="-1"`, a model row or a group heading), else its marked
   * `[data-anchor-focus]`, else a collapsed group's own summary button.
   *
   * @param {string} anchor - a group id, a logical model id, or `runtime`
   * @returns {Promise<boolean>} whether the anchor was on screen to go to
   */
  async function reveal(anchor) {
    const group = groupOfModel(anchor) ?? anchor
    if (group === 'filtering') filteringOpen = true
    if (anchor === 'flux') fluxOpen = true
    await tick()
    const panel = document.getElementById(panelId(active))
    const target = /** @type {HTMLElement|null} */ (
      panel?.querySelector(`[data-settings-anchor="${anchor}"]`) ?? null
    )
    if (!target) return false
    target.scrollIntoView?.({ block: 'start' })
    const focusable = /** @type {HTMLElement|null} */ (
      target.matches('[tabindex="-1"]')
        ? target
        : target.querySelector('[data-anchor-focus]') ?? target.querySelector('button[aria-expanded]')
    )
    ;(focusable ?? target).focus?.({ preventScroll: true })
    return true
  }

  /**
   * An anchor whose row is not drawn yet, and where focus was put meanwhile.
   * Most rows (the redraw weight, the filtering models, other files, the
   * runtime) are drawn from the catalogue, which arrives after mount; the
   * link lands on the group heading first and on the row once it is there,
   * unless the reader has moved focus in between.
   *
   * @type {{anchor: string, landed: Element|null}|null}
   */
  let pending = $state.raw(null)

  /** The group heading a model's link lands on while its row is not drawn. @param {string} anchor */
  function holdingGroup(anchor) {
    const group = groupOfModel(anchor)
    return group && group !== anchor ? group : null
  }

  onMount(() => {
    const anchor = opening.anchor
    if (!anchor) return
    void reveal(anchor).then(async (found) => {
      if (found) return
      const group = holdingGroup(anchor)
      if (group) await reveal(group)
      pending = { anchor, landed: document.activeElement }
    })
  })

  $effect(() => {
    if (!pending || !catalogue) return
    const { anchor, landed } = pending
    pending = null
    untrack(() => {
      if (active !== opening.section || document.activeElement !== landed) return
      void reveal(anchor)
    })
  })

  // Which section is on screen, for the notice a failed background download
  // raises: a row's inline error only counts as said while its section shows.
  $effect(() => {
    showSettingsSection(active)
    return () => showSettingsSection(null)
  })

  const uid = $props.id()
  /** @param {string} id */
  const tabId = (id) => `${uid}-tab-${id}`
  /** @param {string} id */
  const panelId = (id) => `${uid}-panel-${id}`

  /** @type {HTMLButtonElement[]} */
  let tabButtons = $state([])

  /**
   * @param {string} id
   * @param {{focus?: boolean}} [options]
   */
  function select(id, { focus = false } = {}) {
    active = id
    // The element identity does not change - the list is keyed over a static
    // table - so the press can move focus without waiting for a flush.
    if (focus) tabButtons[TABS.findIndex((tab) => tab.id === id)]?.focus()
  }

  /**
   * Whether Performance ends in its per-model backends, which are a
   * control. Otherwise it ends in the runtime's prose and the scroller takes
   * the tab stop (WCAG 2.1.1).
   */
  const performanceTailFocusable = $derived(Boolean(accelerators && accelerators.models.length > 0))

  /**
   * Up and Down move and select, Home and End go to the ends. Stopped as well
   * as prevented: the editor underneath pages the chapter on the arrows.
   *
   * @param {KeyboardEvent} event
   */
  function onlistkeydown(event) {
    const index = TABS.findIndex((tab) => tab.id === active)
    let next
    switch (event.key) {
      case 'ArrowDown':
        next = (index + 1) % TABS.length
        break
      case 'ArrowUp':
        next = (index - 1 + TABS.length) % TABS.length
        break
      case 'Home':
        next = 0
        break
      case 'End':
        next = TABS.length - 1
        break
      default:
        return
    }
    event.preventDefault()
    event.stopPropagation()
    select(TABS[next].id, { focus: true })
  }
</script>

<!-- The removal question, inside the row that asked it. Escape answers Keep
     on either button, so it never also closes Settings. -->
{#snippet confirmStrip()}
  <div class="confirm" role="group" aria-labelledby="{uid}-confirm-text">
    <p class="confirm-text" id="{uid}-confirm-text">
      {#each confirmLines as line (line)}<span>{line}</span>{/each}
    </p>
    <div class="confirm-actions">
      <Button size="sm" id="{uid}-keep" onclick={keep} onkeydown={onconfirmkeydown}>
        {t('settings.models.remove.keep')}
      </Button>
      <Button size="sm" variant="primary" onclick={confirmRemove} onkeydown={onconfirmkeydown}>
        {t('settings.models.action.delete')}
      </Button>
    </div>
  </div>
{/snippet}

<!-- A catalogue row no capability claims: a weight's name, size and state,
     with the presses that apply to it. Normally there are none. -->
{#snippet fileRow(/** @type {any} */ model)}
  {@const status = statusOf(model.id, model.installed, model.sha256Ok)}
  <li class="row" id={rowId(model.id)}>
    <div class="row-text">
      <span class="row-name">{t(model.kindKey)}</span>
      <span class="row-meta">
        {t('models.value.size', { bytes: model.bytes })} ·
        {t(status.key, status.params)}{#if model.installed && model.readOnly}
          · {t('settings.models.status.readOnly')}{/if}
      </span>
      {#if failures[model.id]}
        <span class="row-error">{failureText(model.id)}</span>
      {/if}
      {#if notes[model.id]}
        <span class="row-error">{t(notes[model.id])}</span>
      {/if}
      <!-- The bytes a stopped download left. Not shown while one runs: the
           progress in the meta line already says it. -->
      {#if model.partialBytes && !progress[model.id]}
        <span class="row-partial">
          {t('settings.models.status.partial', { bytes: model.partialBytes })}
        </span>
      {/if}
    </div>
    <div class="row-actions">
      {#if progress[model.id]}
        <Button size="sm" onclick={() => cancel(model.id)}>
          {t('settings.models.action.cancel')}
        </Button>
      {:else}
        {#if model.installed}
          <Button size="sm" onclick={() => verify(model.id)}>
            {t('settings.models.action.verify')}
          </Button>
          <Button
            size="sm"
            id="{rowId(model.id)}-delete"
            disabled={model.readOnly}
            onclick={() => askRemove(null, model.id, `${rowId(model.id)}-delete`)}
          >
            {t('settings.models.action.delete')}
          </Button>
        {:else}
          <Button size="sm" onclick={() => download(model.id)}>
            {t('settings.models.action.download')}
          </Button>
        {/if}
        <!-- Beside either pair: a `.part` can outlive a weight installed by
             hand, and it is still disk nobody asked to spend. -->
        {#if model.partialBytes}
          <Button size="sm" onclick={() => discard(model.id)}>
            {t('settings.models.action.discard')}
          </Button>
        {/if}
      {/if}
    </div>
    {#if confirming && confirming.modelId === null && confirming.fileId === model.id}
      {@render confirmStrip()}
    {/if}
  </li>
{/snippet}

{#snippet fileList(/** @type {any[]} */ models)}
  {#if models.length > 0}
    <h3 class="sub" tabindex="-1" data-anchor-focus>{t('settings.models.heading')}</h3>
    <ul class="rows">
      {#each models as model (model.id)}
        {@render fileRow(model)}
      {/each}
    </ul>
  {/if}
{/snippet}

<!-- One logical model: one name, its total size and one state, the presses
     that apply to the whole of it, and its component files in the details
     below. A multi-file model installs, checks and deletes as one unit; the
     details keep per-file Check and Delete for troubleshooting. -->
{#snippet modelRow(/** @type {import('../model/pipelines.js').LogicalModel} */ entry, /** @type {boolean} */ choice = false)}
  {@const view = /** @type {any} */ (viewOf(entry))}
  {@const group = entry.group ? groupById(entry.group) : null}
  {@const single = entry.source === 'download' && !entry.group ? view.files?.[0] : null}
  {@const needed = view.known && !view.installed && neededNow(entry.id)}
  {@const deleteId = `${rowId(entry.id)}-delete`}
  {@const selected = choice && runSelection.detectorModels.includes(entry.id)}
  {@const onCloud = placement.cloud.includes(entry.id)}
  <li class="row model" class:choice id={rowId(entry.id)} data-model={entry.id} data-settings-anchor={entry.id} tabindex="-1">
    {#if choice}
      <!-- The model's own choice leads its row: one list for choosing and
           installing, rather than a checklist above and the same names again
           below it. The last model selected cannot be cleared, and on the
           cloud GPU none can: that run's combination is fixed. -->
      <input
        id="{rowId(entry.id)}-choice"
        class="check"
        type="checkbox"
        data-anchor-focus
        checked={selected}
        disabled={cloudFixed || (selected && session.detectorModels.length === 1)}
        aria-describedby={cloudFixed ? `${rowId(entry.id)}-where ${uid}-cloud-combo` : `${rowId(entry.id)}-where`}
        onchange={(event) => chooseDetectorModel(entry.id, event.currentTarget.checked, event.currentTarget)}
      />
    {/if}
    <div class="row-text">
      {#if choice}
        <label class="row-name" for="{rowId(entry.id)}-choice">{entry.product ?? t(entry.nameKey)}</label>
      {:else}
        <span class="row-name">{entry.product ?? t(entry.nameKey)}</span>
      {/if}
      <span class="row-meta">
        {#if view.known}{metaOf(view)}{/if}{#if needed}<span class="needed">{` · ${t('settings.models.status.neededNow')}`}</span>{/if}
      </span>
      {#if choice}
        <!-- Where it can run, in words: the two with a cloud version and the
             two that always run here. While detection is on the cloud GPU,
             where each model of that run's fixed combination runs. -->
        <span class="row-where" id="{rowId(entry.id)}-where">
          {#if cloudFixed && onCloud}
            {t('settings.models.where.onCloud')}
          {:else if cloudFixed && selected}
            {t('settings.models.where.cloudLocal')}
          {:else if cloudFixed}
            {t('settings.models.where.cloudUnused')}
          {:else}
            {t(runsOnCloud(entry.id) ? 'settings.models.where.either' : 'settings.models.where.localOnly')}
          {/if}
        </span>
      {/if}
      <span class="row-role">{t(entry.roleKey)}</span>
      {#if entry.id === 'samTs' && view.installed && workflowCaps && !workflowCaps.samMemoryReady}
        <span class="row-role">{t('settings.detection.sam.memory')}</span>
      {/if}
      {#if single && failures[single.id]}<span class="row-error">{failureText(single.id)}</span>{/if}
      {#if single && notes[single.id]}<span class="row-error">{t(notes[single.id])}</span>{/if}
      {#if group && groupFailures[group.id]}<span class="row-error">{groupFailures[group.id]}</span>{/if}
      {#if group && groupNotes[group.id]}<span class="row-error">{t(groupNotes[group.id])}</span>{/if}
      {#if importFailures[entry.id]}<span class="row-error">{importFailures[entry.id]}</span>{/if}
      {#if single?.partialBytes && !progress[single.id]}
        <span class="row-partial">{t('settings.models.status.partial', { bytes: single.partialBytes })}</span>
      {/if}
    </div>
    <div class="row-actions">
      {#if view.kind === 'download'}
        {#if view.downloading}
          <Button size="sm" onclick={() => (group ? cancelGroup(group) : cancel(single.id))}>
            {t('settings.models.action.cancel')}
          </Button>
        {:else}
          {#if view.installed}
            <Button size="sm" onclick={() => (group ? verifyGroup(group) : verify(single.id))}>
              {t('settings.models.action.verify')}
            </Button>
          {:else}
            <Button size="sm" onclick={() => (group ? downloadGroup(group) : download(single.id))}>
              {t('settings.models.action.download')}
            </Button>
          {/if}
          {#if view.installedCount > 0}
            <Button size="sm" id={deleteId} disabled={view.readOnly} onclick={() => askRemove(entry.id, null, deleteId)}>
              {t('settings.models.action.delete')}
            </Button>
          {/if}
          {#if single?.partialBytes}
            <Button size="sm" onclick={() => discard(single.id)}>
              {t('settings.models.action.discard')}
            </Button>
          {/if}
        {/if}
      {:else if view.kind === 'import'}
        {#if !view.installed}
          {#if entry.id === 'samTs'}
            <Button size="sm" disabled={importBusy[entry.id]} onclick={installSamTs}>
              {t('settings.models.action.install')}
            </Button>
          {/if}
          <Button size="sm" disabled={importBusy[entry.id] || !canImport(entry)} onclick={() => importModel(entry)}>
            {t('settings.models.action.import')}
          </Button>
        {:else}
          {#if entry.importId === 'samTs'}
            <Button size="sm" disabled={importBusy[entry.id]} onclick={() => checkImport(entry)}>
              {t('settings.models.action.verify')}
            </Button>
          {/if}
          {#if view.managed}
            <Button size="sm" id={deleteId} disabled={importBusy[entry.id]} onclick={() => askRemove(entry.id, null, deleteId)}>
              {t('settings.models.action.delete')}
            </Button>
          {/if}
        {/if}
      {/if}
    </div>
    {#if confirming && confirming.modelId === entry.id}
      {@render confirmStrip()}
    {/if}
    {#if (view.files?.length ?? 0) > 0 || view.revision}
      <div class="details">
        <Disclosure
          variant="plain"
          open={detailsOpen[entry.id] === true}
          ontoggle={(open) => (detailsOpen = { ...detailsOpen, [entry.id]: open })}
        >
          {#snippet summary()}{t('settings.models.details')}{/snippet}
          {#if view.kind === 'import' && view.revision}
            <p class="file-note">{t('settings.models.revision', { revision: view.revision })}</p>
          {/if}
          <ul class="files">
            {#each view.files as file (file.id ?? file.name)}
              {@const nameId = `${rowId(entry.id)}-file-${file.id ?? file.name}`}
              {@const controls = view.kind === 'download' && file.installed && !view.downloading}
              {@const held = controls && !file.readOnly ? fileDeleteHeld(entry) : null}
              <li class="file">
                <div class="file-text">
                  <span class="file-name" id={nameId}>{file.fileName ?? file.name}</span>
                  {#if view.kind === 'download'}
                    {@const status = statusOf(file.id, file.installed, file.sha256Ok)}
                    <span class="file-meta">
                      {t('models.value.size', { bytes: file.bytes })} · {t(status.key, status.params)}{#if file.installed && file.readOnly}
                        · {t('settings.models.status.readOnly')}{/if}
                    </span>
                  {:else}
                    <span class="file-meta">{t('models.value.size', { bytes: file.bytes })}</span>
                  {/if}
                  <span class="file-meta">SHA-256 <code>{file.sha256}</code></span>
                  {#if group && failures[file.id]}<span class="row-error">{failureText(file.id)}</span>{/if}
                  {#if held}<span class="file-held" id="{nameId}-held">{t(held)}</span>{/if}
                </div>
                <!-- Check and Delete per file, for a single-file model as for
                     a group. A held Delete stays in the tab order with its
                     reason read beside it (aria-disabled, not disabled). -->
                {#if controls}
                  <div class="file-actions">
                    <Button size="sm" aria-describedby={nameId} onclick={() => verify(file.id)}>
                      {t('settings.models.action.verify')}
                    </Button>
                    <Button
                      size="sm"
                      id="{nameId}-delete"
                      aria-describedby={held ? `${nameId} ${nameId}-held` : nameId}
                      aria-disabled={held ? 'true' : undefined}
                      disabled={file.readOnly}
                      onclick={() => {
                        if (!held) askRemove(entry.id, file.id, `${nameId}-delete`)
                      }}
                    >
                      {t('settings.models.action.delete')}
                    </Button>
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
          {#if view.kind === 'download'}
            <p class="file-note">{t('settings.models.revisionUnavailable')}</p>
          {:else if entry.importId === 'samTs'}
            <p class="file-note">{t('settings.models.importPair')}</p>
          {/if}
        </Disclosure>
      </div>
    {/if}
  </li>
{/snippet}

<!-- The Hayai text reader switch, in Detection right under the models it
     completes (its files stay listed under language filtering), under both
     policies, with what it can actually do now: nothing until its files are
     here. On the cloud GPU it is ticked and fixed with the models, and the
     section's own combination note describes it. The label sits left and the box
     right, the shape every switch on this screen has; the description and the
     status are both read with the box. -->
{#snippet rescueOption()}
  <div class="option">
    <div class="option-line">
      <label class="option-label" for="settings-ocr-rescue">{t('pipelines.workflow.ocrRescue')}</label>
      <input
        id="settings-ocr-rescue"
        class="check"
        type="checkbox"
        aria-describedby={cloudFixed
          ? `settings-ocr-rescue-description ${uid}-cloud-combo settings-ocr-rescue-status`
          : 'settings-ocr-rescue-description settings-ocr-rescue-status'}
        checked={runSelection.ocrRescue}
        disabled={cloudFixed}
        onchange={(event) => setOcrRescue(event.currentTarget.checked)}
      />
    </div>
    <p class="option-description" id="settings-ocr-rescue-description">{t('pipelines.workflow.ocrRescueDescription')}</p>
    <div class="option-status" class:shown={rescueStatus !== null}>
      <span id="settings-ocr-rescue-status" role="status">{rescueStatus ? t(rescueStatus.key) : ''}</span>
      {#if rescueStatus?.download}
        {#if rescueStatus.view.downloading}
          <span class="option-progress">{t(rescueStatus.view.state.key, rescueStatus.view.state.params)}</span>
        {:else}
          <Button size="sm" onclick={() => downloadRescue()}>
            {t('settings.detection.download', { bytes: rescueStatus.view.missingBytes })}
          </Button>
        {/if}
      {/if}
    </div>
  </div>
{/snippet}

<!-- The optional text-shaped review: collapsed under legacy, open under
     all-text, and mounted only while open so its readiness probe and graph
     check run when someone asks for the review, not whenever Settings opens. -->
{#snippet review()}
  <div class="review">
    <Disclosure variant="plain" open={reviewOpen} ontoggle={toggleReview}>
      {#snippet summary()}
        <span class="review-summary">
          <span class="review-title">{t('settings.detection.review.summary')}</span>
          <span class="review-tag">{t('settings.detection.review.optional')}</span>
        </span>
      {/snippet}
      <p class="line review-note">{t('settings.detection.review.note')}</p>
      <WorkflowAnalysis initialWorkflow={workflowForDetectorModels(runSelection.detectorModels)} initialRtProfile={runSelection.detectorModels.includes('rtFull') ? 'full-halves' : 'small-whole'} />
    </Disclosure>
  </div>
{/snippet}

<!-- Language filtering, collapsed: the policy that decides whether the script
     gate runs, the languages it keeps, the OCR rescue, and the two models
     behind them. Its summary says the policy in force and any failure on its
     rows, so nothing inside it is hidden by the fold. -->
{#snippet filtering()}
  <section class="advanced" aria-labelledby="{uid}-filtering" data-settings-anchor="filtering">
    <Disclosure variant="plain" open={filteringOpen} ontoggle={(open) => (filteringOpen = open)}>
      {#snippet summary()}
        <span class="adv-summary">
          <span class="adv-title" id="{uid}-filtering">{t('settings.detection.capability.japanese')}</span>
          <span class="adv-note">{filteringNote}</span>
        </span>
      {/snippet}
      <div class="adv-body">
        {#if !allText}
          <div class="languages">
            {#each LANGUAGES as language (language.id)}
              <Field label={t(language.labelKey)} layout="row" controlId="settings-detector-{language.id}">
                {#snippet children()}
                  <div class="pick-lang">
                    <Select
                      id="settings-detector-{language.id}"
                      options={detectorOptions(language.id)}
                      value={session.detection[language.id] ?? SKIP}
                      label={t('pipelines.detectorFor', { language: t(language.labelKey) })}
                      onchange={(value) => setDetection(language.id, value || null)}
                    />
                  </div>
                {/snippet}
              </Field>
            {/each}
          </div>
        {:else}
          <p class="line">{t('settings.detection.languagesAllText')}</p>
        {/if}
        {#if filteringEntries.some((entry) => viewOf(entry).known)}
          <ul class="rows">
            {#each filteringEntries.filter((entry) => viewOf(entry).known) as entry (entry.id)}
              {@render modelRow(entry)}
            {/each}
          </ul>
        {/if}
      </div>
    </Disclosure>
  </section>
{/snippet}

<!-- The FLUX helper, collapsed: an external install the redraw can run
     through, its backend, and which of its models. -->
{#snippet fluxHelper()}
  <section class="advanced" aria-labelledby="{uid}-flux" data-settings-anchor="flux">
    <Disclosure variant="plain" open={fluxOpen} ontoggle={(open) => (fluxOpen = open)}>
      {#snippet summary()}
        <span class="adv-summary">
          <span class="adv-title" id="{uid}-flux">{t('settings.sidecar.heading')}</span>
          <span class="adv-note">{t(fluxReady ? 'settings.sidecar.ready' : 'settings.sidecar.notSetUp')}</span>
        </span>
      {/snippet}
      <div class="adv-body">
        <Field label={t('settings.sidecar.label')} controlId="settings-sidecar-path">
          {#snippet children()}
            <div class="inline">
              <TextInput
                id="settings-sidecar-path"
                value={sidecarDraft ?? session.sidecarPath}
                placeholder={t('settings.sidecar.automaticFolder')}
                onchange={(value) => (sidecarDraft = value)}
                onblur={commitSidecar}
                onkeydown={(/** @type {KeyboardEvent} */ event) => {
                  if (event.key === 'Enter') commitSidecar()
                }}
              />
              <Button onclick={browseSidecar} disabled={choosing}>
                {t('shell.action.chooseFolder')}
              </Button>
            </div>
            {#if session.sidecarPath && !capabilities.sidecar}
              <p class="line">{t('settings.sidecar.notFound')}</p>
            {/if}
          {/snippet}
        </Field>

        <Field label={t('settings.fluxBackend.label')} layout="row">
          {#snippet children({ labelId })}
            <Segmented
              options={fluxBackends}
              value={session.fluxBackend}
              labelledBy={labelId}
              onchange={(value) => {
                setFluxBackend(/** @type {any} */ (value))
                push()
              }}
            />
          {/snippet}
        </Field>
        {#if session.fluxBackend === 'mflux' && fluxPlatform !== 'macos-arm64'}
          <p class="line">{t('settings.fluxBackend.mfluxReason')}</p>
        {/if}

        <Field label={t('settings.sidecar.accelerator')} layout="row" controlId="settings-flux-accelerator">
          {#snippet children()}
            <Select
              id="settings-flux-accelerator"
              options={fluxAcceleratorChoices}
              value={fluxUsesMlx ? 'auto' : fluxAccelerator}
              disabled={installingFlux || fluxUsesMlx}
              onchange={(value) => (fluxAccelerator = value)}
            />
          {/snippet}
        </Field>
        {#if fluxUsesMlx}<p class="line">{t('settings.sidecar.mlxAutomatic')}</p>{/if}
        <div class="inline install">
          <Button onclick={installFlux} disabled={installingFlux || !fluxAcceleratorAllowed || (session.fluxBackend === 'mflux' && fluxPlatform !== 'macos-arm64')}>
            {installingFlux ? t('settings.sidecar.installing') : t('settings.sidecar.install')}
          </Button>
          {#if installingFlux}<span role="status">{t(fluxStageKey)}</span>{/if}
        </div>
        {#if fluxInstallError}<p class="line" role="alert">{t('settings.sidecar.installFailed', { detail: fluxInstallError })}</p>{/if}

        {#if capabilities.sidecar}
          <Field label={t('settings.sidecarModel.label')} layout="row" controlId="settings-sidecar-model">
            {#snippet children()}
              <Select
                id="settings-sidecar-model"
                fit
                disabled={sidecarModels.length === 0}
                options={sidecarChoices}
                value={session.fluxModel || sidecarChoices[0].value}
                onchange={(value) => {
                  setFluxModel(value)
                  push()
                }}
              />
            {/snippet}
          </Field>
        {/if}
      </div>
    </Disclosure>
  </section>
{/snippet}

<Screen label={t(spec.titleKey)} onclose={() => closeModal(null)}>
  <div class="settings">
    <nav class="side" aria-labelledby="{uid}-title">
      <div class="side-head">
        <button
          type="button"
          class="back"
          aria-label={t('shell.action.done')}
          title={t('shell.action.done')}
          onclick={() => closeModal('done')}
        ><Icon name="chevron-left" size={16} /></button>
        <h1 id="{uid}-title">{t(spec.titleKey)}</h1>
      </div>

      <!-- One tab stop for the whole list: the selected tab. The handler is on
           the tabs because the list itself is not focusable. -->
      <div class="tabs" role="tablist" aria-orientation="vertical" aria-label={t('settings.tabs.label')}>
        {#each TABS as tab, index (tab.id)}
          <button
            bind:this={tabButtons[index]}
            type="button"
            role="tab"
            class="tab"
            class:on={tab.id === active}
            id={tabId(tab.id)}
            aria-selected={tab.id === active}
            aria-controls={panelId(tab.id)}
            tabindex={tab.id === active ? 0 : -1}
            title={t(tab.labelKey)}
            onclick={() => select(tab.id)}
            onkeydown={onlistkeydown}
          >
            <Icon name={tab.icon} size={16} />
            <span class="tab-label">{t(tab.labelKey)}</span>
          </button>
        {/each}
      </div>
    </nav>

    <!-- General: the app's own preferences, in three short groups. -->
    <div
      class="panel"
      role="tabpanel"
      id={panelId('general')}
      aria-labelledby={tabId('general')}
      hidden={active !== 'general'}
    >
      <div class="column">
        <h2>{t('settings.section.general')}</h2>

        <h3 class="sub first">{t('settings.general.appearance')}</h3>
        <div class="block theme">
          <Field label={t('settings.theme.label')}>
            {#snippet children({ labelId })}
              <ThemePicker
                options={themes}
                value={session.theme}
                labelledBy={labelId}
                onchange={(value) => {
                  setTheme(/** @type {any} */ (value))
                  push()
                }}
              />
            {/snippet}
          </Field>
        </div>

        <!-- How a detected region's mask is drawn on the page: the area Clean
             will erase, and what the selection tool edits. A preference of
             this window's only, so it is saved with the session and nothing
             is written to the backend. The swatch opens the colour picker,
             whose hex field is the keyboard's route to an exact value. Two
             colours, so text in a speech bubble and text outside one are told
             apart at a glance; a region whose place is unknown takes the
             first. -->
        <Field label={t('settings.mask.color')} description={t('settings.mask.description')} layout="row">
          {#snippet children()}
            <span class="mask-row">
              <span class="mask-hex">{session.maskColor}</span>
              <ColorPicker
                value={session.maskColor}
                label={t('settings.mask.color')}
                align="end"
                onchange={(hex) => setMaskColor(hex)}
              />
            </span>
          {/snippet}
        </Field>
        <Field label={t('settings.mask.outsideColor')} layout="row">
          {#snippet children()}
            <span class="mask-row">
              <span class="mask-hex">{session.outsideMaskColor}</span>
              <ColorPicker
                value={session.outsideMaskColor}
                label={t('settings.mask.outsideColor')}
                align="end"
                onchange={(hex) => setOutsideMaskColor(hex)}
              />
            </span>
          {/snippet}
        </Field>
        <Field label={t('settings.mask.opacity')} layout="row">
          {#snippet children()}
            <span class="mask-row">
              <Range
                label={t('settings.mask.opacity')}
                value={session.maskOpacity}
                min={0}
                max={100}
                step={5}
                width={140}
                valueText={t('settings.mask.percent', { value: session.maskOpacity })}
                onchange={(value) => setMaskOpacity(value)}
              />
              <output class="mask-readout" aria-hidden="true">{t('settings.mask.percent', { value: session.maskOpacity })}</output>
            </span>
          {/snippet}
        </Field>

        <h3 class="sub">{t('settings.general.reading')}</h3>
        <Field label={t('settings.direction.label')} layout="row">
          {#snippet children({ labelId })}
            <Segmented
              options={directions}
              value={session.readingDirection}
              labelledBy={labelId}
              onchange={(value) => {
                setReadingDirection(/** @type {any} */ (value))
                push()
              }}
            />
          {/snippet}
        </Field>

        <Field label={t('settings.originalView.label')} layout="row">
          {#snippet children({ labelId })}
            <Segmented
              options={originalViews}
              value={session.originalView}
              labelledBy={labelId}
              onchange={(value) => {
                setOriginalView(/** @type {any} */ (value))
                push()
              }}
            />
          {/snippet}
        </Field>

        <h3 class="sub">{t('settings.general.app')}</h3>
        <Field
          label={t('settings.background.label')}
          description={t('settings.background.description')}
          layout="row"
          controlId="settings-close-to-tray"
        >
          {#snippet children({ descriptionId })}
            <input
              id="settings-close-to-tray"
              class="check"
              type="checkbox"
              aria-describedby={descriptionId}
              checked={session.closeToTray}
              onchange={(event) => updateCloseToTray(/** @type {HTMLInputElement} */ (event.currentTarget))}
            />
          {/snippet}
        </Field>

        {#if backgroundError}<p class="error" role="alert">{t('settings.background.saveFailed')}</p>{/if}

        <Field label={t('settings.language.label')} layout="row">
          {#snippet children({ labelId })}
            <Select options={languages} value={session.language} labelledBy={labelId} fit onchange={setLanguage} />
          {/snippet}
        </Field>

        <!-- Where someone who skipped part of the setup goes looking for it. -->
        <Field
          label={t('onboarding.replay.label')}
          description={t('onboarding.replay.description')}
          layout="row"
        >
          {#snippet children({ descriptionId })}
            <Button size="sm" onclick={replayOnboarding} disabled={replaying} aria-describedby={descriptionId}>
              {t('onboarding.replay.action')}
            </Button>
          {/snippet}
        </Field>
        {#if replayError}<p class="error" role="alert">{t('onboarding.replay.failed')}</p>{/if}
      </div>
    </div>

    <!-- Models: whether the selected models can run, then the two pipelines
         as two groups, then the files no model claims and the download token.
         It ends in the token's field, so the scroller needs no tab stop. -->
    <div
      class="panel"
      role="tabpanel"
      id={panelId('models')}
      aria-labelledby={tabId('models')}
      hidden={active !== 'models'}
    >
      <div class="column">
        <h2>{t('settings.section.models')}</h2>

        <!-- Whether the selected workflow can run with what is here, from the
             same needs the downloads follow. -->
        {#if readiness}
          <div class="readiness" class:complete={readiness.complete}>
            <span class="readiness-icon" aria-hidden="true">
              <Icon name={readiness.complete ? 'check' : 'info'} size={14} />
            </span>
            <p class="readiness-text" role="status">
              {#each readiness.lines as line (line)}<span>{line}</span>{/each}
            </p>
            {#if readiness.toDownload.length > 0 || (needs.includes('samTs') && workflowCaps?.samInstalled !== true) || readiness.runtime === 'missing' || readiness.runtime === 'unloadable'}
              <div class="readiness-actions">
                {#if readiness.toDownload.length > 0 || (needs.includes('samTs') && workflowCaps?.samInstalled !== true)}
                  <Button size="sm" onclick={downloadNeeded}>
                    {readiness.toDownload.length > 0 ? t('settings.detection.download', { bytes: readiness.missing }) : t('settings.models.action.install')}
                  </Button>
                {/if}
                <!-- The runtime has its build choice and its own row in
                     Performance, so the press goes there rather than
                     downloading from here. A runtime that will not load is
                     replaced there too, and its row repeats why. -->
                {#if readiness.runtime === 'missing' || readiness.runtime === 'unloadable'}
                  <Button size="sm" onclick={() => select('performance', { focus: true })}>
                    {t('settings.detection.ready.openPerformance')}
                  </Button>
                {/if}
              </div>
            {/if}
          </div>
        {/if}

        <!-- Detection: the four choices, each with its files, then the review
             and the language filtering, both collapsed. -->
        <section class="group" aria-labelledby="{uid}-group-detection" data-settings-anchor="detection">
          <h3 class="sub" id="{uid}-group-detection" tabindex="-1" data-anchor-focus>{t('pipelines.detection')}</h3>
          <fieldset class="choices" aria-describedby={cloudFixed ? `${uid}-cloud-combo ${uid}-choices-note` : `${uid}-choices-note`}>
            <legend class="sr">{t('settings.detection.modelsLegend')}</legend>
            <ul class="rows">
              {#each detectEntries as entry (entry.id)}
                {@render modelRow(entry, true)}
              {/each}
            </ul>
          </fieldset>
          {@render rescueOption()}
          {#if cloudFixed}<p class="line" id="{uid}-cloud-combo" data-cloud-combo>{t('pipelines.cloudCombo')}</p>{/if}
          <p class="line" id="{uid}-choices-note">{t('settings.detection.profiles')}</p>
          {@render review()}
          {@render filtering()}
        </section>

        <!-- Cleaning: the redraw weight, and the FLUX helper collapsed. -->
        <section class="group" aria-labelledby="{uid}-group-cleaning" data-settings-anchor="cleaning">
          <h3 class="sub" id="{uid}-group-cleaning" tabindex="-1" data-anchor-focus>{t('pipelines.cleaning')}</h3>
          {#if cleaningEntries.some((entry) => viewOf(entry).known)}
            <ul class="rows">
              {#each cleaningEntries.filter((entry) => viewOf(entry).known) as entry (entry.id)}
                {@render modelRow(entry)}
              {/each}
            </ul>
          {/if}
          {@render fluxHelper()}
        </section>

        {#if catalogue}
          {#if unclaimed.length > 0}
            <section class="group" data-settings-anchor="other">{@render fileList(unclaimed)}</section>
          {/if}
        {:else}
          <p class="note">{t('settings.models.unavailable')}</p>
        {/if}

        <!-- Download access: every model downloads from Hugging Face, so the
             token belongs to Models rather than to either group. Write-only:
             the stored token never comes back across the seam, so the box is
             empty on every open and "a token is saved" is said in words. -->
        <section class="group" aria-labelledby="{uid}-group-access" data-settings-anchor="access">
          <h3 class="sub" id="{uid}-group-access" tabindex="-1" data-anchor-focus>{t('settings.models.access.heading')}</h3>
          <div class="block token">
            <Field label={t('settings.models.token.label')} controlId="settings-hf-token">
              {#snippet children()}
                <div class="inline">
                  <TextInput
                    id="settings-hf-token"
                    type="password"
                    value={tokenDraft}
                    placeholder={t('settings.models.token.placeholder')}
                    onchange={(value) => (tokenDraft = value)}
                  />
                  <Button disabled={tokenDraft.trim().length === 0} onclick={saveToken}>
                    {t('settings.models.token.save')}
                  </Button>
                  <Button disabled={!catalogue?.hasToken} onclick={clearToken}>
                    {t('settings.models.token.clear')}
                  </Button>
                </div>
                {#if tokenFailure}
                  <p class="line failed">{t(tokenFailure)}</p>
                {:else if tokenNote}
                  <p class="line">{tokenNote}</p>
                  {#if tokenReason}
                    <p class="line">{tokenReason}</p>
                  {/if}
                {/if}
              {/snippet}
            </Field>
          </div>
        </section>
      </div>
    </div>

    <!-- Cloud: connection, endpoints, credentials and their remediation. -->
    <div
      class="panel"
      role="tabpanel"
      id={panelId('cloud')}
      aria-labelledby={tabId('cloud')}
      hidden={active !== 'cloud'}
    >
      <div class="column">
        <h2>{t('settings.section.cloud')}</h2>
        <InferenceSettings />
      </div>
    </div>

    <!-- Denoise: where page denoise runs, its preset, and the local model
         with its measured time. It ends in the Measure button. -->
    <div
      class="panel"
      role="tabpanel"
      id={panelId('denoise')}
      aria-labelledby={tabId('denoise')}
      hidden={active !== 'denoise'}
    >
      <div class="column">
        <h2>{t('settings.section.denoise')}</h2>
        <DenoiseSettings view={catalogue} refresh={refreshCatalogue} active={active === 'denoise'} />
      </div>
    </div>

    <!-- Performance: the local runtime, the accelerator, where models are
         stored, and the per-model backends collapsed. It ends in that
         disclosure's button when there are backends to list; otherwise in
         prose, and the scroller carries the tab stop. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="panel"
      role="tabpanel"
      tabindex={performanceTailFocusable ? undefined : 0}
      id={panelId('performance')}
      aria-labelledby={tabId('performance')}
      hidden={active !== 'performance'}
    >
      <div class="column">
        <h2>{t('settings.section.performance')}</h2>

        {#if catalogue}
          <!-- The runtime is an archive that gets unpacked, not a catalogue
               row. The build named is the one a Download would fetch. -->
          <ul class="rows">
            <li class="row" data-settings-anchor="runtime" tabindex="-1">
              <div class="row-text">
                <span class="row-name">{t('settings.models.runtime.label')}</span>
                <span class="row-meta">
                  {#if catalogue.runtime.available}
                    {#if catalogue.runtime.bytes}
                      {t('models.value.size', { bytes: catalogue.runtime.bytes })} ·
                    {/if}
                    {catalogue.runtime.version} · {catalogue.runtime.flavour} ·
                    {t(statusOf(RUNTIME_ID, catalogue.runtime.installed, null).key,
                      statusOf(RUNTIME_ID, catalogue.runtime.installed, null).params)}{#if catalogue.runtime.installed && catalogue.runtime.readOnly}
                      · {t('settings.models.status.readOnly')}{/if}
                  {:else}
                    {t('settings.models.runtime.unavailable')}
                  {/if}
                </span>
                {#if failures[RUNTIME_ID]}
                  <span class="row-error">{failureText(RUNTIME_ID)}</span>
                {/if}
                {#if notes[RUNTIME_ID]}
                  <span class="row-error">{t(notes[RUNTIME_ID])}</span>
                {/if}
                <!-- Here is not the same as usable: the reason a runtime on
                     disk would not load, where the readiness row sends. -->
                {#if catalogue.runtime.installed && runtimeLoad.state === 'failed'}
                  <span class="row-error">{t(runtimeLoad.reasonKey)}</span>
                {/if}
                <!-- Which build is actually here, said only when it is not the
                     one the row names. -->
                {#if installedNote}
                  <span class="row-partial">{installedNote}</span>
                {/if}
                {#if catalogue.runtime.partialBytes && !progress[RUNTIME_ID]}
                  <span class="row-partial">
                    {t('settings.models.status.partial', { bytes: catalogue.runtime.partialBytes })}
                  </span>
                {/if}
              </div>
              <div class="row-actions">
                {#if progress[RUNTIME_ID]}
                  <Button size="sm" onclick={() => cancel(RUNTIME_ID)}>
                    {t('settings.models.action.cancel')}
                  </Button>
                {:else}
                  {#if catalogue.runtime.partialBytes}
                    <Button size="sm" onclick={() => discard(RUNTIME_ID)}>
                      {t('settings.models.action.discard')}
                    </Button>
                  {/if}
                  {#if catalogue.runtime.installed}
                    <Button
                      size="sm"
                      disabled={catalogue.runtime.readOnly}
                      onclick={() => remove(RUNTIME_ID)}
                    >
                      {t('settings.models.action.delete')}
                    </Button>
                  {/if}
                  <!-- Over an installed runtime only where there is a choice of
                       build: elsewhere it would re-download what is there. -->
                  {#if !catalogue.runtime.installed || catalogue.runtime.flavours.length > 1}
                    <Button
                      size="sm"
                      disabled={!catalogue.runtime.available}
                      onclick={() => download(RUNTIME_ID)}
                    >
                      {t('settings.models.action.download')}
                    </Button>
                  {/if}
                {/if}
              </div>
            </li>
          </ul>

          <!-- Only where the platform publishes more than one build: a select
               with one option only asks a question it has already answered. -->
          {#if catalogue.runtime.flavours.length > 1}
            <Field label={t('settings.models.runtime.flavour')} layout="row" controlId="settings-runtime-flavour">
              {#snippet children()}
                <Select
                  id="settings-runtime-flavour"
                  fit
                  options={catalogue.runtime.flavours.map((build) => ({
                    value: build.id,
                    label: `${build.id} · ${build.ortVersion} · ${t('models.value.size', { bytes: build.bytes })}`,
                  }))}
                  value={catalogue.runtime.flavour}
                  onchange={chooseFlavour}
                />
              {/snippet}
            </Field>
            {#if flavourNeeds}
              <p class="line">{flavourNeeds}</p>
            {/if}
          {/if}

          {#if catalogue.modelsDir}
            <p class="path">{t('settings.models.folder', { path: catalogue.modelsDir })}</p>
          {/if}
        {:else}
          <p class="note">{t('settings.models.unavailable')}</p>
        {/if}

        <div class="accel">
          <Field label={t('settings.accel.label')} layout="row" controlId="settings-accelerator">
            {#snippet children()}
              <!-- The note is on the option's face as well as its tooltip: a
                   caveat only a hover can find is one a keyboard never sees. -->
              <Select
                id="settings-accelerator"
                fit
                options={acceleratorChoices}
                value={acceleratorValue}
                onchange={chooseAccelerator}
              />
            {/snippet}
          </Field>
        </div>

        {#if accelFailure}
          <p class="line">{t('settings.accel.unreadable')}</p>
        {/if}
        {#if modelAccelFailure}
          <p class="line failed" role="alert">{t('settings.accel.saveFailed')}</p>
        {/if}

        {#if accelerators && accelerators.models.length > 0}
          <section class="advanced" aria-labelledby="{uid}-backends">
            <Disclosure variant="plain" open={backendsOpen} ontoggle={(open) => (backendsOpen = open)}>
              {#snippet summary()}
                <span class="adv-summary">
                  <span class="adv-title" id="{uid}-backends">{t('settings.accel.models')}</span>
                  <span class="adv-note">{t('settings.accel.modelsCount', { count: accelerators.models.length })}</span>
                </span>
              {/snippet}
              <div class="adv-body">
                <p class="line">{t('settings.accel.modelHelp')}</p>
                <ul class="rows">
                  {#each accelerators.models as row (row.id)}
                    <li class="row">
                      <div class="row-text">
                        <span class="row-name">{modelDisplayName(row)}</span>
                        <span class="row-meta">{t('settings.accel.predicted', { backend: placementOf(row) })}</span>
                        {#if runsOnCloud(row.id) && session.analysisTargets[row.id] === 'cloud'}
                          <span class="row-meta">{t('settings.detection.runOn.performance')}</span>
                        {/if}
                        {#if row.backendStatus}
                          <span class="row-meta">{row.backendStatus.map((status) => {
                            const name = accelerators.providers.find((provider) => provider.id === status.id)?.labelKey
                            const level = status.verified ? 'verified' : status.available ? 'available' : status.installed ? 'installed' : status.supported ? 'supported' : 'unsupported'
                            return `${name ? t(name) : status.id}: ${t(ACCEL_STATE_KEYS[level])}`
                          }).join(' · ')}</span>
                        {/if}
                      </div>
                      {#if row.id}
                        <div class="model-backend-choice">
                          <Select
                            id="settings-model-backend-{row.id}"
                            label={t('settings.accel.modelLabel', { model: modelDisplayName(row) })}
                            options={modelBackendChoices(row)}
                            value={session.modelAccelerators[row.id] ?? 'inherit'}
                            onchange={(value) => chooseModelAccelerator(row.id, value)}
                          />
                        </div>
                      {/if}
                    </li>
                  {/each}
                </ul>
              </div>
            </Disclosure>
          </section>
        {/if}
      </div>
    </div>

    <!-- Shortcuts -->
    <div
      class="panel"
      role="tabpanel"
      id={panelId('shortcuts')}
      aria-labelledby={tabId('shortcuts')}
      hidden={active !== 'shortcuts'}
    >
      <div class="column">
        <h2>{t('settings.section.shortcuts')}</h2>
        <ShortcutSheet headingLevel="h3" />
      </div>
    </div>

    <!-- About ends in the community invitation, a link, so the scroller needs
         no tab stop of its own. The written offer of source above it is a
         GPL-3.0 obligation and this screen is its only route. -->
    <div
      class="panel"
      role="tabpanel"
      id={panelId('about')}
      aria-labelledby={tabId('about')}
      hidden={active !== 'about'}
    >
      <div class="column">
        <h2>{t('settings.section.about')}</h2>
        <AboutSection />
      </div>
    </div>
  </div>
</Screen>

<style>
  .settings {
    flex: 1;
    min-height: 0;
    display: flex;
  }

  /* ---- sidebar ---------------------------------------------------------- */

  .side {
    flex: none;
    width: 212px;
    display: flex;
    flex-direction: column;
    gap: var(--s-6);
    padding: var(--s-4) var(--s-3);
    background: var(--sb);
    border-right: 1px solid var(--line);
    overflow-y: auto;
  }

  .side-head {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .back {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: none;
    border-radius: var(--r-lg);
    background: none;
    color: var(--t2);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }
  .back:hover { background: var(--accent-soft); color: var(--text) }
  .back:active { transform: scale(.96) }
  h1 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    letter-spacing: -.005em;
  }

  .tabs {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  /* Weight never changes with selection; the fill and the icon's colour carry
     it, so a label never shifts under the pointer. */
  .tab {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    height: 30px;
    padding: 0 var(--s-3);
    border: none;
    border-radius: var(--r-chip);
    background: none;
    color: var(--t2);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }
  .tab:hover { background: var(--accent-soft); color: var(--text) }
  .tab.on { background: var(--accent-soft); color: var(--text) }
  .tab.on :global(svg) { color: var(--accent) }
  .tab:focus-visible { outline-offset: -2px }
  .tab-label { overflow: hidden; text-overflow: ellipsis }

  /* ---- content ---------------------------------------------------------- */

  .panel {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-gutter: stable;
  }
  .panel:focus-visible { outline-offset: -2px }
  .panel[hidden] { display: none }

  .column {
    max-width: 680px;
    margin: 0 auto;
    padding: var(--s-8) var(--s-6) 64px;
  }

  h2 {
    margin: 0 0 var(--s-6);
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -.01em;
    line-height: 1.2;
  }
  .sub {
    margin: var(--s-8) 0 var(--s-2);
    font-size: 12.5px;
    font-weight: 600;
  }
  .sub.first { margin-top: 0 }
  /* A heading a deep link lands on takes focus without being a tab stop. */
  .sub:focus { outline: none }
  .sub:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: var(--r-chip) }

  /* Models' groups: Detection, Cleaning, the files no model claims, and
     download access, each under its own heading and hairline. */
  .group { margin-top: var(--s-8) }
  .group > .sub { margin-top: 0 }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  /* Rows at screen scale: the Field row's 32px floor was set for a 560px
     dialog. */
  .column :global(.field.row .line) { min-height: 40px }
  .column :global(.field.row .label) { font-size: 12.5px }

  .block {
    padding: var(--s-3) 0 var(--s-5);
    border-bottom: 1px solid var(--line);
  }
  .theme { container-type: inline-size; padding-top: 0 }
  .mask-row {
    display: inline-flex;
    align-items: center;
    gap: var(--s-2);
  }
  .mask-hex,
  .mask-readout {
    min-width: 36px;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--t2);
    text-align: right;
  }
  /* Six swatches: one row of six where they fit, two rows of three where
     they do not, never five and one. */
  .theme :global(.picker) { grid-template-columns: repeat(6, minmax(0, 1fr)) }
  @container (max-width: 520px) {
    .theme :global(.picker) { grid-template-columns: repeat(3, minmax(0, 1fr)) }
  }
  .token { border-bottom: none; padding-top: var(--s-6) }

  .check {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
    cursor: pointer;
  }

  .inline {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    width: 100%;
  }
  .inline :global(> *:first-child) {
    flex: 1;
    min-width: 0;
  }

  .languages { margin-bottom: var(--s-4) }
  /* One width for the three pickers, so a language with a longer engine name
     does not make its row look different from the others. */
  .pick-lang { width: 15rem; max-width: 100% }

  .accel { margin-top: var(--s-6) }
  .model-backend-choice { width: min(17rem, 45%); flex: none }
  @media (max-width: 640px) {
    .model-backend-choice { width: 100% }
    .row:has(.model-backend-choice) { flex-wrap: wrap }
  }

  /* One row is a name and a meta line on the left and its buttons on the
     right, with the hairline every row on this screen uses. */
  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--s-4);
    padding: var(--s-3) 0;
    border-bottom: 1px solid var(--line);
  }
  .row-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .row-name {
    font-size: 12.5px;
    color: var(--text);
  }
  .row-meta,
  .row-partial {
    font-size: 11px;
    color: var(--t3);
    line-height: 1.4;
  }
  /* `--t2` rather than `--t3`: a failure is the one line the reader has to act
     on, and `--t3` is under 4.5:1 against the dark field. */
  .row-error {
    font-size: 11px;
    color: var(--t2);
    line-height: 1.4;
    word-break: break-word;
  }
  .row-actions {
    display: flex;
    flex: none;
    gap: var(--s-2);
  }

  /* ---- the capability graph ------------------------------------------- */

  /* Whether the selected workflow can run: one quiet line under the policy,
     with its one action. Words carry the state; the icon only repeats it. */
  .readiness {
    display: flex;
    align-items: flex-start;
    gap: var(--s-3);
    margin-top: var(--s-4);
    padding: var(--s-3) var(--s-4);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel2);
  }
  .readiness-icon {
    flex: none;
    display: flex;
    padding-top: 1px;
    color: var(--t2);
  }
  .readiness.complete .readiness-icon { color: var(--accent) }
  .readiness-text {
    flex: 1;
    min-width: 0;
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text);
  }
  .readiness-text span + span::before { content: ' ' }
  .readiness-actions {
    display: flex;
    flex: none;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--s-2);
    max-width: 50%;
  }
  .readiness :global(.btn) { flex: none; margin-top: -2px }

  /* The four detection choices: a plain fieldset, its legend for screen
     readers only, since the group heading above already says it. */
  .choices {
    margin: 0;
    padding: 0;
    border: none;
    min-width: 0;
  }

  /* A model row wraps so the confirmation and the details can take the
     full width under the name and the buttons. */
  .row.model { flex-wrap: wrap; align-items: flex-start }
  .row.model .row-actions { padding-top: 1px }
  /* A choice row leads with its checkbox, aligned to the name's first line. */
  .row.choice > .check { flex: none; margin-top: 1px }
  .row.choice .row-name { cursor: pointer }
  /* A row a deep link lands on shows where focus went; a pointer never
     focuses it. */
  .row:focus { outline: none }
  .row:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: var(--r-chip) }
  /* Where a choice runs, in words; while detection is on the cloud GPU,
     where each model of that run's fixed combination runs. */
  .row-where {
    font-size: 11px;
    line-height: 1.4;
    color: var(--t3);
  }
  .row-role {
    font-size: 11px;
    line-height: 1.4;
    color: var(--t2);
    max-width: 62ch;
  }
  /* A missing model the selected workflow needs: said in words, and in the
     warning colour as well. */
  .needed { color: var(--warn) }

  .confirm {
    flex-basis: 100%;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--s-3) var(--s-4);
    padding: var(--s-3) var(--s-4);
    border: 1px solid var(--line2);
    border-radius: var(--r-md);
    background: var(--panel2);
    animation: mcFade var(--dur-fast) var(--ease);
  }
  .confirm-text {
    flex: 1 1 280px;
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text);
  }
  .confirm-text span + span::before { content: ' ' }
  .confirm-actions { display: flex; gap: var(--s-2); flex: none }

  .details { flex-basis: 100%; margin-top: -2px }
  .details :global(.summary) { color: var(--t2); font-size: 11px }
  .details :global(.summary:hover) { color: var(--text) }
  .files {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .file {
    display: flex;
    align-items: flex-start;
    gap: var(--s-4);
    padding: var(--s-2) 0;
  }
  .file + .file { border-top: 1px solid var(--line) }
  .file-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .file-name { font-size: 11.5px; color: var(--text); overflow-wrap: anywhere }
  .file-meta { font-size: 11px; color: var(--t2); line-height: 1.4; overflow-wrap: anywhere }
  .file-meta code {
    font-family: var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 10.5px;
    user-select: all;
  }
  .file-actions { display: flex; gap: var(--s-2); flex: none }
  /* Held, not disabled: it keeps its tab stop so the reason beside it can be
     reached, and looks like the native disabled state beside it. */
  .file-actions :global(.btn[aria-disabled='true']) { opacity: .38; cursor: default }
  .file-held { font-size: 11px; color: var(--t2); line-height: 1.4; max-width: 60ch }
  .file-note {
    margin: var(--s-2) 0 0;
    font-size: 11px;
    line-height: 1.4;
    color: var(--t2);
    max-width: 68ch;
  }

  /* The OCR rescue switch: label left, box right, the shape of every switch
     on this screen, with its description and its honest status under it. */
  .option {
    padding: var(--s-2) 0 var(--s-3);
    border-bottom: 1px solid var(--line);
  }
  .option-line {
    display: flex;
    align-items: center;
    gap: var(--s-4);
    min-height: 32px;
  }
  .option-label { flex: 1; font-size: 12.5px; color: var(--t2); cursor: pointer }
  .option-description {
    margin: 0;
    font-size: 11px;
    line-height: 1.45;
    color: var(--t3);
    max-width: 62ch;
  }
  .option-status {
    display: none;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--s-2) var(--s-3);
    margin-top: var(--s-2);
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--warn);
  }
  .option-status.shown { display: flex }
  .option-progress { color: var(--t2) }

  .review { margin-top: var(--s-4) }
  .review :global(.summary) { font-size: 12.5px; color: var(--text) }
  .review-summary { display: inline-flex; align-items: baseline; gap: var(--s-2) }
  .review-title { font-weight: 600 }
  .review-tag { font-size: 11px; color: var(--t3) }
  .review-note { margin-top: 0 }
  /* The panel draws its own top rule and spacing for a stand-alone mount;
     inside the disclosure the summary already separates it. */
  .review :global(.workflow-analysis) { border-top: none; margin-top: var(--s-3); padding-top: 0 }

  /* Progressive disclosure: an advanced group is one summary line, its
     title and its state, until opened. */
  .advanced { margin-top: var(--s-4) }
  .advanced :global(.summary) { font-size: 12.5px; color: var(--text) }
  .adv-summary { display: inline-flex; flex-wrap: wrap; align-items: baseline; gap: 0 var(--s-2) }
  .adv-title { font-weight: 600 }
  .adv-note { font-size: 11px; color: var(--t3) }
  .adv-body > :global(:first-child) { margin-top: 0 }

  .note,
  .line {
    margin: var(--s-2) 0 0;
    font-size: 11px;
    color: var(--t3);
    line-height: 1.45;
    max-width: 68ch;
  }
  .note { margin-top: var(--s-6) }
  .line.failed,
  .error { color: var(--warn) }
  .error {
    margin: var(--s-2) 0 0;
    font-size: 11px;
    line-height: 1.45;
  }

  /* A path is data: selectable, and broken anywhere rather than overflowing. */
  .path {
    margin: var(--s-3) 0 0;
    font-size: 11px;
    color: var(--t3);
    line-height: 1.4;
    word-break: break-all;
    user-select: text;
  }

  /* A narrow window keeps the list and drops its words: the icons stay, and
     the labels remain the tabs' accessible names. */
  @media (max-width: 720px) {
    .side { width: 52px; padding-inline: var(--s-2) }
    .side-head h1,
    .tab-label {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip-path: inset(50%);
      white-space: nowrap;
    }
    .tab { justify-content: center; padding: 0 }
    .back { margin-inline: auto }
    .column { padding-inline: var(--s-5) }
  }
</style>
