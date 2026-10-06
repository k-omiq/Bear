<script>
  /**
   * About, inside Settings.
   *
   * It is an obligation, not a nicety: the app must carry an attribution list, the **written offer
   * of source**, the app's own version, the model versions, and the cloud
   * provider's terms. Task 7r's ruling put it here because the editor's action
   * pill is two icon buttons and Settings is the only pointer route to it.
   *
   * The facts come from `backend.about()` as `{labelKey, value}`: the *label*
   * is translated, the *value* never is. Licence identifiers, URLs, provider
   * and model names and version strings are proper nouns, and a translated
   * `GPL-3.0-only` is a licensing claim, not a translation.
   *
   * Rendered as a `<dl>` on the design file's provenance-row metrics - a
   * fixed-width key column in `--t3`, values in `--t2`.
   *
   * It ends in the community invitation, which is the one place Settings
   * offers it.
   */
  import { getBackend } from '../api/backend.js'
  import { Button } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import { checkForUpdate } from '../updater.js'
  import UpdateDialog from '../home/UpdateDialog.svelte'
  import DiscordInvite from './DiscordInvite.svelte'

  /** @type {{appVersion: string, facts: Array<{labelKey: string, value: string}>}|null} */
  let about = $state(null)
  let checking = $state(false)
  let checkStatus = $state(/** @type {'' | 'upToDate' | 'checkFailed'} */ (''))
  let updateFound = $state(/** @type {import('@tauri-apps/plugin-updater').Update | null} */ (null))
  let updateDialogOpen = $state(false)

  $effect(() => {
    let live = true
    getBackend()
      .about()
      .then((info) => {
        if (live) about = info
      })
    return () => {
      live = false
    }
  })

  async function onCheckUpdate() {
    if (checking) return
    checking = true
    checkStatus = ''
    try {
      const update = await checkForUpdate()
      if (update) {
        updateFound = update
        updateDialogOpen = true
        checkStatus = ''
      } else {
        checkStatus = 'upToDate'
      }
    } catch {
      checkStatus = 'checkFailed'
    } finally {
      checking = false
    }
  }
</script>

{#if about}
  <dl class="facts">
    <dt>{t('about.fact.version')}</dt>
    <dd>{about.appVersion}</dd>
    {#each about.facts as fact (fact.labelKey)}
      <dt>{t(fact.labelKey)}</dt>
      <dd>{fact.value}</dd>
    {/each}
  </dl>

  <div class="update-check-row">
    <Button size="sm" variant="ghost" onclick={onCheckUpdate} disabled={checking}>
      {checking ? t('update.action.checking') : t('update.action.check')}
    </Button>
    {#if checkStatus === 'upToDate'}
      <span class="status-msg">{t('update.status.upToDate')}</span>
    {:else if checkStatus === 'checkFailed'}
      <span class="status-msg error">{t('update.status.checkFailed')}</span>
    {/if}
  </div>

  <p class="offer">{t('about.offer.written')}</p>
  <p class="terms">{t('about.note.cloudTerms')}</p>

  <UpdateDialog bind:open={updateDialogOpen} update={updateFound} />
{/if}

<DiscordInvite compact />

<style>
  .facts {
    display: grid;
    grid-template-columns: 90px 1fr;
    column-gap: 12px;
    margin: 0;
    font-size: 10.5px;
    line-height: 1.7;
  }
  dt { color: var(--t3) }
  dd {
    margin: 0;
    color: var(--t2);
    overflow-wrap: anywhere;
  }

  .update-check-row {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    margin-top: 10px;
  }
  .status-msg {
    font-size: 11px;
    color: var(--t3);
  }
  .status-msg.error {
    color: var(--warn);
  }

  .offer,
  .terms {
    margin: 10px 0 0;
    font-size: 10.5px;
    line-height: 1.6;
    color: var(--t3);
  }
</style>
