<script>
  /**
   * The application shell.
   *
   * Five responsibilities, and no more - screens, panels and dialogs are
   * Tasks 6 to 11:
   *
   *  1. Route: library / chapters (both Home) and editor.
   *  2. Theme: apply the resolved theme to `<html data-theme>` and follow the
   *     OS live while `system` is selected.
   *  3. Settings: reconcile the session with the backend, once, at boot.
   *  4. Hosts: the notice stack and the modal host.
   *  5. The keyboard layer.
   *  6. The first-launch download offer - the one thing that has to be decided
   *     at launch rather than on a screen.
   *  7. The background jobs' subscription and the quit guard's question.
   */
  import { app, dismissNotice } from './lib/state/app.svelte.js'
  import { writeSettingsSerialized } from './lib/state/settingswrite.js'
  import { getBackend } from './lib/api/backend.js'
  import { downloadFailureShown, reportModelDownloadFailure } from './lib/api/model-download-notices.js'
  import { openModelSettings } from './lib/dialogs/settingslinks.js'
  import { session, installThemeSync, applyTheme, applyLanguage, backendSettingsPatch, reconcileSettings } from './lib/state/session.svelte.js'
  import { loadCapabilities } from './lib/state/capabilities.svelte.js'
  import { startCloud } from './lib/state/cloud.svelte.js'
  import { installReloadGuard } from './lib/shell/reload-guard.js'
  import { displayModelName } from './lib/model/model-names.js'
  import CloudJobStatus from './lib/editor/CloudJobStatus.svelte'
  import {
    loadedModels,
    pollLoadedModels,
    unloadModel,
  } from './lib/state/loadedmodels.svelte.js'
  import { cloudGpu, gpuDeployments, isStopping, scaledownLeftMs, startCloudGpu, stopCloudGpu } from './lib/state/cloudgpu.svelte.js'
  import { LoadedModels, Notices } from './lib/ui/index.js'
  import { t } from './lib/i18n/index.js'
  import HomeScreen from './lib/home/HomeScreen.svelte'
  import EditorScreen from './lib/editor/EditorScreen.svelte'
  import KeyboardLayer from './lib/shell/KeyboardLayer.svelte'
  import ModalHost from './lib/shell/ModalHost.svelte'
  import FirstLaunchDialog from './lib/dialogs/FirstLaunchDialog.svelte'
  import { firstLaunch, offerFirstLaunch } from './lib/dialogs/firstlaunch.svelte.js'
  import { jobs, startJobs } from './lib/state/jobs.svelte.js'
  import QuitJobsDialog from './lib/dialogs/QuitJobsDialog.svelte'
  import { untrack } from 'svelte'

  // Follow `prefers-color-scheme` for the life of the app, not just at load.
  $effect(installThemeSync)
  $effect(installReloadGuard)
  // Re-runs whenever the chosen theme or the OS preference changes.
  $effect(applyTheme)
  $effect(applyLanguage)

  // The session and the backend hold the same four preferences, and they are
  // reconciled **here, once, at boot** - not when Settings happens to be
  // opened. Reconciling only in the dialog left the two stores free to
  // disagree for any session in which the user never opened it: a cloud
  // permission granted last session and kept in the session record, against a
  // backend rebuilt to its blocked default, is an interface that offers a
  // cloud tool and an adapter that refuses it. Which way the reconciliation
  // runs, and why, is documented on `reconcileSettings()`.
  //
  // What this machine can run is queried directly after settings adoption so
  // sidecar availability checks read the reconciled settings rather than
  // racing disk state.
  $effect(() => {
    let live = true
    const backend = getBackend()
    backend
      .readSettings()
      .then((settings) => {
        if (live) {
          reconcileSettings(settings)
          return writeSettingsSerialized(backend, () => backendSettingsPatch())
        }
      })
      // A settings file that cannot be read or written must not also cost the
      // capability read and cloud start-up: readiness reads settings again
      // itself and fails closed, and recovery waits for the permission.
      .catch(() => {})
      .then(() => {
        if (!live) return
        loadCapabilities(backend)
        // Cloud readiness and start-up recovery read the reconciled permission.
        startCloud(backend)
        startCloudGpu(backend)
      })
    return () => {
      live = false
    }
  })

  // Model transfers can outlive Settings, onboarding, and the screen that
  // started them. Keep a root observer for failures so background errors are
  // visible after those inline rows have unmounted. A screen that shows the
  // row keeps the more detailed per-file error and suppresses a duplicate
  // notice; Settings counts only on the section that lists the row.
  $effect(() => getBackend().subscribe((event) => {
    if (event.type !== 'model-progress' || !event.done || !event.error || event.error === 'cancelled') return
    const covered = downloadFailureShown(event, { firstLaunch, topModal: app.modals.at(-1)?.kind ?? null })
    reportModelDownloadFailure(event, { covered })
  }))

  // Background jobs: runs and denoises outlive the dialog or the editor that
  // started them, so the one subscription that follows them is here, with the
  // quit guard's question (`state/jobs.svelte.js`).
  $effect(() => untrack(() => startJobs()))

  // The first-launch download offer.
  //
  // The catalogue is asked once, here, because this is where "the application
  // has just started" is known - a dialog that asked on its own mount would
  // have to be mounted first, which is the question. `loadCapabilities()`
  // above makes the same call and keeps none of it: it reduces the view to
  // booleans, and the offer needs the sizes, so this is a second `listModels`
  // rather than a shared one. That call is cheap by construction (presence and
  // size, never a digest) and this one happens once per launch.
  //
  // A backend that cannot answer offers nothing: the failure of a catalogue
  // read is the case `state/capabilities` treats as "assume everything is
  // here", and opening a download dialog over a machine whose weights simply
  // could not be listed would be the worse of the two wrong answers.
  //
  // What the offer *is* - the plan, the ticks, the progress, the sequence -
  // lives in `dialogs/firstlaunch.svelte.js` rather than in the dialog, so a
  // run survives the dialog losing the screen. This is the trigger and
  // nothing else.
  $effect(() => {
    if (session.firstLaunchOffered) return
    let live = true
    getBackend()
      .listModels()
      .then((view) => {
        if (live) offerFirstLaunch(view)
      })
      .catch(() => {})
    return () => {
      live = false
    }
  })

  // Notices are stored as i18n data and translated at the edge, here. A param
  // whose name ends in `Key` carries a key, which `t()` resolves.
  const notices = $derived(
    app.notices.map((notice) => ({
      id: notice.id,
      text: t(notice.key, notice.params),
      tone: notice.tone,
      icon: notice.icon,
      duration: notice.duration,
      actionLabel: notice.key === 'notice.run.modelsMissing' ? t('tools.target.chooseModels') : undefined,
      onaction: notice.key === 'notice.run.modelsMissing'
        ? () => { openModelSettings(typeof notice.params?.model === 'string' ? notice.params.model : null); dismissNotice(notice.id) }
        : undefined,
    }))
  )

  // What is loaded, polled while the editor is on screen and only there.
  //
  // Only there because that is where the memory is spent: Home starts no run
  // and opens no session, so the poll would be asking a question whose answer
  // is always an empty list. The teardown clears the list, so a return to Home
  // cannot leave a stale panel behind.
  $effect(() => {
    if (app.route.name !== 'editor') return
    return pollLoadedModels()
  })

  // Translated at the edge, like the notices below - the panel is a primitive
  // and holds no strings. `models.action.unload` takes the row's own name as a
  // `Key` param, so the button says which model it frees rather than relying on
  // the row it happens to sit in.
  const loaded = $derived(
    loadedModels.models.map((model) => ({
      id: model.id,
      name: model.modelName ? displayModelName(model.modelName) : t(model.kindKey),
      // A measured row says the number; every other row says about the number.
      // `basis` has been on the wire since the tab existed precisely so this
      // distinction could be drawn without re-deriving it here.
      size: t(model.basis === 'measured' ? 'models.value.size' : 'models.value.sizeApprox', {
        bytes: model.bytes,
      }),
      device: t(model.deviceKey),
      unloading: model.unloading,
      unloadLabel: t('models.action.unload', { name: model.modelName ? displayModelName(model.modelName) : t(model.kindKey) }),
    }))
  )

  // The cloud GPU shares the panel: it is the other thing an idle app can be
  // holding, and the one that bills by the hour. One row per container that is
  // up, keyed apart from the local rows, whose ids are numbers. Its close button
  // stops that container in the cloud (`state/cloudgpu.svelte.js#stopCloudGpu`).
  // When the last read failed the rows stay, saying their state is unknown.
  const GPU_NAME_KEYS = { render: 'cloud.gpu.name.render', analysis: 'cloud.gpu.name.analysis' }
  const GPU_STOP_KEYS = { render: 'cloud.gpu.action.stop', analysis: 'cloud.gpu.action.stopAnalysis' }
  const GPU_STATE_KEYS = { starting: 'cloud.gpu.state.starting', busy: 'cloud.gpu.state.busy', idle: 'cloud.gpu.state.idle' }
  /** @param {number} ms */
  const clock = (ms) => {
    const seconds = Math.ceil(ms / 1000)
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
  }
  const cloudRows = $derived(
    gpuDeployments().flatMap((deployment) => deployment.containers.map((container) => {
      const price = container.listPriceUsdPerHour
      const left = scaledownLeftMs(container, deployment.target)
      return {
        id: `cloud-${deployment.target.provider}:${deployment.target.profileId}:${container.role}`,
        target: deployment.target, role: container.role,
        name: `${t(GPU_NAME_KEYS[container.role], { gpu: container.gpu })} · ${deployment.name}`,
        size: typeof price === 'number' ? t('cloud.gpu.price', { price }) : '',
        device: typeof price === 'number' ? t('cloud.gpu.priceNote', { price }) : t('cloud.gpu.noPrice'),
        detail: deployment.stale
          ? t('cloud.gpu.state.unknown')
          : left !== null ? t('cloud.gpu.state.idleStops', { time: clock(left) }) : t(GPU_STATE_KEYS[container.state]),
        unloading: isStopping(container.role, deployment.target),
        unloadingLabel: t('cloud.gpu.state.stopping'),
        unloadLabel: t(GPU_STOP_KEYS[container.role]),
      }
    }))
  )
  const panelTitle = $derived(
    cloudRows.length === 0 ? t('models.title') : loaded.length === 0 ? t('cloud.gpu.title') : t('cloud.gpu.titleMixed')
  )

  // How far the notice stack has to rise to clear the panel. Measured rather
  // than assumed: the panel's height is its row count, and a constant here
  // would be wrong the moment a run loads a fourth model. `0` when nothing is
  // loaded, because the panel then renders nothing at all and the stack sits
  // where it always did.
  let panelHeight = $state(0)
  const GAP = 7

  // Bottom-left at 14/14 on every route, which is where the design file draws
  // the stack - and it draws it there in the same floating layout, with the
  // Pages window at left:16.
  //
  // The editor used to lift the stack by 54px to clear "the bottom bar".
  // There is no bottom bar: the floating layout has a
  // centred zoom pill and a right-hand nav pill, both of which a 230px column
  // at the left edge already clears. The lift only pushed the stack *up* into
  // the Pages and Layers windows, which is the collision it was meant to
  // avoid; 14 puts it back below them.
  //
  // The stack sits at `z-index: 10` below floating windows (`20 + rank`), so a
  // bottom-left window like Layers is never covered or blocked by notices,
  // while individual notices remain dismissible.
</script>

<KeyboardLayer />

<div class="shell">
  {#if app.route.name === 'editor'}
    <EditorScreen />
  {:else}
    <HomeScreen />
  {/if}
</div>

<!-- The bottom-left corner, floor first: the loaded-models panel sits on the
     14px anchor and the notice stack rises above it. Both are `z-index: 10`,
     below the floating windows at `20 + rank`. -->
<div class="corner" bind:clientHeight={panelHeight}>
  <CloudJobStatus />
  <LoadedModels
    models={[...loaded, ...cloudRows]}
    title={panelTitle}
    icon={loaded.length === 0 && cloudRows.length > 0 ? 'cloud' : 'cpu'}
    unloadingLabel={t('models.value.unloading')}
    onunload={(id) => {
      const row = cloudRows.find((row) => row.id === id)
      return row ? stopCloudGpu(row.role, undefined, row.target) : unloadModel(id)
    }}
  />
</div>

<Notices
  {notices}
  onclose={dismissNotice}
  dismissLabel={t('shell.action.dismissNotice')}
  bottom={14 + (panelHeight > 0 ? panelHeight + GAP : 0)}
/>

<ModalHost />

<!-- The offer waits for an empty modal stack rather than joining it. `Modal`
     captures focus and answers Escape on `window`, so two mounted dialogs
     fight over both - and this one is not on the stack because it belongs to
     the launch rather than to a screen, where a route change would dismiss it
     (`state/app.svelte.js#setRoute`). Nothing the user can reach pushes a
     dialog while it is up: the backdrop takes every click, and the shortcut
     table is told about a dialog it cannot see (`setDialogOutsideStack`), so
     `,` and `?` no longer answer from underneath. The guard is what is left -
     a net for a push from somewhere else - and it now costs nothing, because
     the run and everything drawn from it are in the store rather than in the
     component: the offer comes back as it was when the stack empties, with the
     transfer it started still running. -->
{#if firstLaunch.open && app.modals.length === 0}
  <FirstLaunchDialog />
{/if}

<!-- The quit guard's question goes over whatever is up, the stack or the
     offer, and last in the document so its backdrop is the top one. It is not
     pushed: that would unmount the dialog underneath
     (`state/jobs.svelte.js#askQuit`). -->
{#if jobs.quit}
  <QuitJobsDialog />
{/if}

<style>
  .shell {
    height: 100%;
    overflow: hidden;
  }

  /* Collapses to nothing when the panel renders nothing, which is what keeps
     the notice stack on its own 14px anchor for the whole of an ordinary
     session. */
  .corner {
    position: fixed;
    bottom: 14px;
    left: 14px;
    z-index: 10;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
</style>
