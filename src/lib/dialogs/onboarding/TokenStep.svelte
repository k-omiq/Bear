<script>
  /**
   * Setup step: an optional Hugging Face key. The draft lives in the store so
   * the frame's Save button can read it; a saved key goes to the backend alone
   * (`saveFirstLaunchToken`) and never into the session.
   *
   * @type {{ onsubmit: () => unknown }}
   */
  import { TextInput } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { firstLaunch } from '../firstlaunch.svelte.js'

  let { onsubmit } = $props()
  const uid = $props.id()
</script>

<p class="lead">{t('onboarding.token.body')}</p>
<label class="label" for="{uid}-token">{t('onboarding.token.label')}</label>
<div class="field">
  <TextInput
    id="{uid}-token"
    type="password"
    autocomplete="off"
    spellcheck="false"
    value={firstLaunch.tokenDraft}
    placeholder={t('onboarding.token.placeholder')}
    onchange={(value) => (firstLaunch.tokenDraft = value)}
    onkeydown={(event) => {
      if (event.key === 'Enter' && firstLaunch.tokenDraft.trim()) onsubmit()
    }}
  />
</div>
{#if firstLaunch.plan?.hasToken}<p class="note">{t('onboarding.token.saved')}</p>{/if}
{#if firstLaunch.tokenFailed}<p class="alert" role="alert">{t('onboarding.token.failed')}</p>{/if}

<style>
  .label { display: block; margin-bottom: var(--s-3); font-size: 11.5px; font-weight: 600 }
  .field { max-width: 360px }
  .field :global(.wrap) { height: 32px }
</style>
