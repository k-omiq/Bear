<script>
  /**
   * Setup step: whether the close button hides the window or quits.
   *
   * Two cards, each saying what it costs. The choice goes through the
   * session's setter and the backend, which is what the window's close
   * handler reads (`src-tauri/src/lib.rs`); a refused write puts it back.
   */
  import { t } from '../../i18n/index.js'
  import { session, setCloseToTray } from '../../state/session.svelte.js'
  import { saveFirstLaunchSetting } from '../firstlaunch.svelte.js'

  let saveFailed = $state(false)

  /** @param {boolean} keep */
  async function choose(keep) {
    saveFailed = !(await saveFirstLaunchSetting(setCloseToTray, keep, session.closeToTray))
  }

  /**
   * Two options, so every arrow is "the other one". Selection follows focus,
   * as in every radio group here.
   *
   * @param {KeyboardEvent & {currentTarget: HTMLElement}} event
   */
  function onkeydown(event) {
    const group = event.currentTarget.parentElement
    /** @type {boolean|null} */
    let keep = null
    if (['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(event.key)) keep = !session.closeToTray
    else if (event.key === 'Home') keep = true
    else if (event.key === 'End') keep = false
    if (keep === null) return
    event.preventDefault()
    event.stopPropagation()
    choose(keep)
    const target = keep ? group?.firstElementChild : group?.lastElementChild
    if (target instanceof HTMLElement) target.focus()
  }
</script>

<div class="choices" role="radiogroup" aria-label={t('onboarding.background.heading')}>
  {#each [true, false] as keep (keep)}
    <button
      type="button"
      role="radio"
      class="choice"
      class:on={session.closeToTray === keep}
      aria-checked={session.closeToTray === keep}
      tabindex={session.closeToTray === keep ? 0 : -1}
      onclick={() => choose(keep)}
      {onkeydown}
    >
      <span class="name">{t(keep ? 'onboarding.background.keep' : 'onboarding.background.quit')}</span>
      <span class="about">{t(keep ? 'onboarding.background.keepNote' : 'onboarding.background.quitNote')}</span>
    </button>
  {/each}
</div>
{#if saveFailed}<p class="alert" role="alert">{t('onboarding.saveFailed')}</p>{/if}

<style>
  .choices { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-4) }
  @media (max-width: 560px) { .choices { grid-template-columns: 1fr } }
  .choice {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-5) var(--s-5) var(--s-6);
    border: none;
    border-radius: var(--r-xl);
    background: var(--panel);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .choice:hover { box-shadow: 0 0 0 1px var(--line2) }
  .choice.on { background: var(--accent-soft); box-shadow: 0 0 0 1.5px var(--accent) }
  .choice:active { transform: scale(.99) }
  .name { font-size: 13px; font-weight: 600 }
  .about { color: var(--t2); font-size: 12px; line-height: 1.5 }
</style>
