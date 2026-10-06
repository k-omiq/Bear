<script>
  /**
   * The setup a first launch opens, and Settings' "Run setup again", as a
   * full-window screen.
   *
   * Eleven steps, one choice each (`FIRST_LAUNCH_STEPS`). This file is the frame
   * around them: the progress bar, the step heading, Back, at most one button
   * beside the primary, the quiet Skip setup, and the keyboard. What a step
   * shows is its own component in `onboarding/`, and everything that has to
   * outlive a mount - the plan, the choices, the download run, what the cloud
   * setup did - is the store in `firstlaunch.svelte.js`, because a dialog
   * raised over this screen unmounts it (`App.svelte`).
   *
   * Every way out goes through `dismissFirstLaunch`, which records that the
   * offer was made. Escape is one of them. While the provisioner is working in
   * the user's account there is no way out here at all, only its own Cancel.
   */
  import { tick } from 'svelte'
  import { Button, Screen } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import { t } from '../i18n/index.js'
  import { session } from '../state/session.svelte.js'
  import { denoise } from '../state/denoise.svelte.js'
  import { openNewProject } from '../home/actions.js'
  import { FIRST_LAUNCH_STEPS } from './firstlaunch.js'
  import {
    chosenBytes,
    closeFirstLaunchProvisioner,
    dismissFirstLaunch,
    finishedFiles,
    firstLaunch,
    nextFirstLaunchStep,
    openFirstLaunchProvisioner,
    pauseAll,
    prevFirstLaunchStep,
    resumeAll,
    saveFirstLaunchToken,
    startFirstLaunchDownloads,
    chosenFiles,
  } from './firstlaunch.svelte.js'
  import WelcomeStep from './onboarding/WelcomeStep.svelte'
  import ThemeStep from './onboarding/ThemeStep.svelte'
  import TokenStep from './onboarding/TokenStep.svelte'
  import BackgroundStep from './onboarding/BackgroundStep.svelte'
  import DetectionStep from './onboarding/DetectionStep.svelte'
  import CleaningStep from './onboarding/CleaningStep.svelte'
  import CloudStep from './onboarding/CloudStep.svelte'
  import DenoiseStep from './onboarding/DenoiseStep.svelte'
  import DiscordInvite from './DiscordInvite.svelte'
  import DependenciesStep from './onboarding/DependenciesStep.svelte'
  import DownloadsStep from './onboarding/DownloadsStep.svelte'

  /** Whole keys chosen between, never built, so the catalogue test sees each one. */
  const HEADINGS = {
    welcome: 'onboarding.welcome.heading',
    theme: 'onboarding.theme.heading',
    token: 'onboarding.token.heading',
    background: 'onboarding.background.heading',
    detection: 'onboarding.detection.heading',
    cleaning: 'onboarding.cleaning.heading',
    cloud: 'onboarding.cloud.heading',
    denoise: 'onboarding.denoise.heading',
    community: 'onboarding.community.heading',
    dependencies: 'onboarding.dependencies.heading',
    downloads: 'onboarding.downloads.heading',
  }

  /** Steps that hold a table, and so get the wider column. */
  const WIDE = ['detection', 'cleaning', 'denoise', 'dependencies', 'downloads']

  const step = $derived(firstLaunch.step)
  /** Welcome is the cover, not a step, so the bar counts from the one after it. */
  const position = $derived(FIRST_LAUNCH_STEPS.indexOf(/** @type {any} */ (step)))
  const total = FIRST_LAUNCH_STEPS.length - 1

  /** @type {HTMLElement|undefined} */
  let heading = $state()

  /** What the chosen files still need from the network, installed ones aside. */
  const pending = $derived.by(() => {
    const done = finishedFiles()
    return chosenFiles().filter((id) => !firstLaunch.plan?.files[id]?.installed && !done[id])
  })

  /** The download step's rows: what was already on disk is not a download. */
  const queue = $derived(firstLaunch.queue.filter((id) => !firstLaunch.plan?.files[id]?.installed))
  const anyPausable = $derived(queue.some((id) => firstLaunch.status[id] === 'waiting' || (id !== 'samTs' && firstLaunch.status[id] === 'active')))
  const anyResumable = $derived(queue.some((id) => ['paused', 'failed'].includes(firstLaunch.status[id])))
  const allDone = $derived(queue.every((id) => firstLaunch.status[id] === 'done'))

  /**
   * @typedef {{label: string, run: () => unknown, enter?: boolean, icon?: string}} Action
   *
   * What the footer offers on this step: Back or not, at most one button
   * beside the primary, and the primary itself. `enter` is whether Enter on
   * the heading may press the primary. It is withheld from Download, so a key
   * pressed twice on the step before cannot start a transfer of hundreds of
   * megabytes.
   */
  const actions = $derived.by(() => {
    /** @type {Action} */
    const next = { label: t('onboarding.action.next'), run: nextFirstLaunchStep, enter: true }
    if (step === 'token') {
      const hasToken = firstLaunch.plan?.hasToken === true
      // Empty and nothing saved, Skip is the step's primary action, so the
      // first footer button a Tab reaches is never mistaken for it.
      if (firstLaunch.tokenDraft.trim()) {
        return {
          back: true,
          secondary: hasToken ? null : { label: t('onboarding.action.skipStep'), run: nextFirstLaunchStep },
          primary: { label: t('onboarding.token.save'), run: saveToken, enter: true },
        }
      }
      return {
        back: true,
        secondary: null,
        primary: hasToken ? next : { label: t('onboarding.action.skipStep'), run: nextFirstLaunchStep, enter: true },
      }
    }
    if (step === 'cloud' && firstLaunch.provisioning) {
      // The provisioner draws its own buttons. Once it has finished, Continue
      // is offered here too, so leaving does not depend on which button its
      // last screen happens to have.
      return {
        back: false,
        secondary: null,
        primary: firstLaunch.cloud ? { label: t('onboarding.action.next'), run: leaveProvisioner } : null,
      }
    }
    if (step === 'cloud' && !firstLaunch.cloud && !session.cloudAllowed) {
      return {
        back: true,
        secondary: { label: t('onboarding.cloud.setUp'), run: openFirstLaunchProvisioner },
        primary: { label: t('onboarding.action.notNow'), run: nextFirstLaunchStep, enter: true },
      }
    }
    // Cloud is on already: the provisioner offers to update that setup (new
    // cloud code, or new choices such as page denoise) before a new one.
    // Page denoise on the cloud GPU was chosen and this setup lacks it: Update is the way on.
    if (step === 'cloud' && !firstLaunch.cloud && session.denoiseTarget === 'cloud' && denoise.cloud === 'missing') {
      return {
        back: true,
        secondary: { label: t('onboarding.action.notNow'), run: nextFirstLaunchStep },
        primary: { label: t('onboarding.cloud.update'), run: openFirstLaunchProvisioner, enter: true },
      }
    }
    if (step === 'cloud' && !firstLaunch.cloud) {
      return {
        back: true,
        secondary: { label: t('onboarding.cloud.update'), run: openFirstLaunchProvisioner },
        primary: { label: t('onboarding.action.next'), run: nextFirstLaunchStep, enter: true },
      }
    }
    if (step === 'dependencies' && pending.length > 0) {
      return {
        back: true,
        secondary: null,
        primary: {
          label: chosenBytes() > 0 ? `${t('onboarding.dependencies.start')} · ${t('models.value.size', { bytes: chosenBytes() })}` : t('onboarding.dependencies.start'),
          run: beginDownloads,
          enter: false,
          icon: 'download',
        },
      }
    }
    if (step === 'downloads') {
      return {
        back: true,
        secondary: anyPausable
          ? { label: t('onboarding.downloads.pauseAll'), run: pauseAll, icon: 'pause' }
          : anyResumable
            ? { label: t('onboarding.downloads.resumeAll'), run: resumeAll, icon: 'play' }
            : null,
        primary: allDone
          ? { label: t('home.action.newProject'), run: startProject, enter: true }
          : {
              label: firstLaunch.running ? t('onboarding.downloads.later') : t('onboarding.downloads.close'),
              run: dismissFirstLaunch,
              enter: true,
            },
      }
    }
    return { back: true, secondary: null, primary: next }
  })

  async function saveToken() {
    if (await saveFirstLaunchToken()) nextFirstLaunchStep()
  }

  function beginDownloads() {
    startFirstLaunchDownloads()
    nextFirstLaunchStep()
  }

  function leaveProvisioner() {
    closeFirstLaunchProvisioner()
    nextFirstLaunchStep()
  }

  /**
   * Close the setup, then raise New project over whatever it was opened on.
   * The tick lets the screen hand focus back before the next dialog takes it,
   * so closing that one returns focus to a real element.
   */
  async function startProject() {
    dismissFirstLaunch()
    await tick()
    openNewProject()
  }

  // A new step moves focus to its heading, so a screen reader starts where
  // the user now is and Enter can press the step's primary action. Not while
  // the provisioner is up: the cloud step puts focus inside it instead, and
  // closing it brings focus back here.
  $effect(() => {
    void step
    if (firstLaunch.provisioning) return
    tick().then(() => heading?.focus())
  })

  /**
   * Enter presses the primary action, but only from the heading. Anywhere
   * else Enter already belongs to the focused control, and taking it from a
   * button or a select would change what that control does.
   *
   * @param {KeyboardEvent} event
   */
  function onkeydown(event) {
    if (event.key !== 'Enter' || event.repeat || event.isComposing || event.defaultPrevented) return
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    if (!heading || event.target !== heading) return
    const primary = step === 'welcome' ? { run: nextFirstLaunchStep, enter: true } : actions.primary
    if (!primary?.enter) return
    event.preventDefault()
    primary.run()
  }
</script>

<svelte:window {onkeydown} />

<Screen
  label={t('onboarding.title')}
  onclose={firstLaunch.provisionerBusy
    ? undefined
    : firstLaunch.provisioning
      ? closeFirstLaunchProvisioner
      : dismissFirstLaunch}
>
  {#if step !== 'welcome'}
    <div
      class="progress"
      role="progressbar"
      aria-label={t('onboarding.progressLabel')}
      aria-valuetext={t('onboarding.stepOf', { current: position, total })}
      aria-valuemin={1}
      aria-valuemax={total}
      aria-valuenow={position}
    >
      {#each FIRST_LAUNCH_STEPS.slice(1) as name, index (name)}
        <span class:on={index < position}></span>
      {/each}
    </div>
  {/if}

  <main class="stage">
    {#key step}
      <section class="step" class:wide={WIDE.includes(step)}>
        <h1 class="heading" class:hero={step === 'welcome'} tabindex="-1" bind:this={heading}>
          {t(HEADINGS[/** @type {keyof typeof HEADINGS} */ (step)])}
        </h1>
        {#if step === 'welcome'}
          <WelcomeStep />
        {:else if step === 'theme'}
          <ThemeStep />
        {:else if step === 'token'}
          <TokenStep onsubmit={saveToken} />
        {:else if step === 'background'}
          <BackgroundStep />
        {:else if step === 'detection'}
          <DetectionStep />
        {:else if step === 'cleaning'}
          <CleaningStep />
        {:else if step === 'cloud'}
          <CloudStep />
        {:else if step === 'denoise'}
          <DenoiseStep />
        {:else if step === 'community'}
          <DiscordInvite />
        {:else if step === 'dependencies'}
          <DependenciesStep />
        {:else}
          <DownloadsStep />
        {/if}
      </section>
    {/key}
  </main>

  {#if step !== 'welcome'}
    <footer class="foot">
      <div class="foot-inner" class:wide={WIDE.includes(step)}>
        {#if step !== 'downloads'}
          <Button variant="plain" size="sm" disabled={firstLaunch.provisionerBusy} onclick={dismissFirstLaunch}>
            {t('onboarding.action.skip')}
          </Button>
        {/if}
        <span class="spacer"></span>
        {#if actions.back}
          <Button onclick={prevFirstLaunchStep}>{t('onboarding.action.back')}</Button>
        {/if}
        {#if actions.secondary}
          <Button onclick={actions.secondary.run}>
            {#if actions.secondary.icon}<Icon name={actions.secondary.icon} size={13} />{/if}
            {actions.secondary.label}
          </Button>
        {/if}
        {#if actions.primary}
          <Button variant="primary" onclick={actions.primary.run}>
            {#if actions.primary.icon}<Icon name={actions.primary.icon} size={13} />{/if}
            {actions.primary.label}
          </Button>
        {/if}
      </div>
    </footer>
  {/if}
</Screen>

<style>
  .progress {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    gap: 3px;
    padding: 14px 18px 0;
  }
  .progress span {
    flex: 1;
    height: 3px;
    border-radius: var(--r-pill);
    background: var(--line2);
    transition: background var(--dur-slow) var(--ease);
  }
  .progress span.on { background: var(--accent) }

  .stage {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    justify-content: center;
    padding: 48px 24px;
  }

  /* Auto block margins centre the step in the stage while it fits, and fall
     to zero once it does not, so a tall step scrolls from its heading instead
     of being clipped above the fold as `align-items: center` would. */
  .step {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 520px;
    margin-block: auto;
    animation: mcIn var(--dur-slow) var(--ease);
  }
  .step.wide { max-width: 680px }

  .heading {
    margin: 0 0 var(--s-6);
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -.01em;
    line-height: 1.2;
    color: var(--text);
  }
  .heading.hero {
    margin-bottom: var(--s-5);
    font-size: clamp(28px, 4vw, 38px);
    letter-spacing: -.02em;
    line-height: 1.1;
  }
  /* The heading takes focus only so a screen reader starts there. It is not a
     control, and a ring around it would read as one. */
  .heading:focus { outline: none }

  /* The steps' shared type, set here so the ten read as one piece. Each step
     uses these classes rather than restating them. */
  .step :global(.lead) {
    margin: calc(-1 * var(--s-3)) 0 var(--s-7);
    max-width: 52ch;
    font-size: 13px;
    line-height: 1.55;
    color: var(--t2);
  }
  .step :global(.note) {
    margin: var(--s-3) 0 0;
    max-width: 62ch;
    font-size: 12px;
    line-height: 1.5;
    color: var(--t2);
  }
  .step :global(.alert) {
    margin: var(--s-3) 0 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--warn);
  }

  .foot {
    display: flex;
    justify-content: center;
    padding: var(--s-5) 24px;
    border-top: 1px solid var(--line);
    background: var(--bg);
  }
  .foot-inner {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    width: 100%;
    max-width: 520px;
  }
  .foot-inner.wide { max-width: 680px }
  .foot :global(.btn) { gap: var(--s-2) }
  .spacer { flex: 1 }
</style>
