<script>
  import { onMount } from 'svelte'
  import { Button, IconButton } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import { pushModal } from '../state/app.svelte.js'
  import { checkForUpdate } from '../updater.js'
  import UpdateDialog from './UpdateDialog.svelte'
  import JobsIndicator from '../shell/JobsIndicator.svelte'

  /**
   * Home's header, and the whole of it: the wordmark, update badge if available,
   * and one quiet Settings button. Nothing else - no action row, no search, no
   * sort, no app menu.
   *
   * Settings is an **icon** button - the two-slider glyph, which is what
   * `icons/paths.js` draws for this because a gear turns to mush at 16px. Its
   * accessible name is still `home.action.settings`, so the control is reached
   * and announced exactly as it was; only the pixels changed. It sits after the
   * update badge, which is the one thing on this bar that comes and goes, so
   * the button that is always there does not move when the badge appears.
   *
   * Jobs comes first of the three: like the badge it comes and goes, and it is
   * the one a user started, so it sits furthest from the fixed button.
   */

  let update = $state(/** @type {import('@tauri-apps/plugin-updater').Update | null} */ (null))
  let updateDialogOpen = $state(false)

  onMount(() => {
    checkForUpdate()
      .then((u) => {
        update = u
      })
      .catch(() => {
        // Ignore updater errors on startup.
      })
  })
</script>

<header class="bar">
  <h1 class="wordmark">{t('app.name.mangaCleaner')}</h1>

  <div class="spacer"></div>

  <JobsIndicator variant="home" />

  {#if update}
    <Button
      size="lg"
      variant="soft"
      onclick={() => (updateDialogOpen = true)}
      title={update.version ? t('update.action.updateTo', { version: update.version }) : t('update.action.available')}
    >
      <span class="update-dot"></span>
      {t('update.action.availableTag', { version: update.version ? (update.version.startsWith('v') ? update.version : `v${update.version}`) : '' })}
    </Button>
  {/if}

  <IconButton
    icon="settings"
    label={t('home.action.settings')}
    onclick={() => pushModal({ kind: 'settings' })}
  />
</header>

<UpdateDialog bind:open={updateDialogOpen} {update} />

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: var(--s-5);
    flex: none;
  }
  .spacer { flex: 1 }

  .wordmark {
    margin: 0;
    font-size: 12.5px;
    font-weight: 600;
    letter-spacing: .3em;
    text-transform: uppercase;
    white-space: nowrap;
  }

  .update-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text);
    display: inline-block;
  }
</style>
