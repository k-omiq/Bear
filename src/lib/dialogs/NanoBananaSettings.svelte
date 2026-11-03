<script>
  /**
   * Settings > Cloud > Nano Banana: the user's own Gemini API key and the tier
   * crops start at.
   *
   * The key is write-only. It goes to the OS credential store
   * (`inference/nano_banana.rs`), and this panel can only ask whether one is
   * there. The model is an ordinary setting, one of three (`model/nanobanana.js`).
   * There is no size setting: native picks each region's crop size.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { onMount, untrack } from 'svelte'
  import { Button, Field, Select } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import { refreshNanoBanana } from '../state/capabilities.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import { NANO_BANANA_MODELS, nanoBananaModel } from '../model/nanobanana.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const uid = $props.id()
  const ids = { key: `${uid}-key`, model: `${uid}-model` }
  const models = NANO_BANANA_MODELS.map((entry) => ({ value: entry.id, label: t(entry.labelKey) }))

  let draft = $state('')
  let hasKey = $state(false)
  let failed = $state(false)
  let model = $state(nanoBananaModel(null).id)

  onMount(async () => {
    try { hasKey = await backend.nanoBananaHasKey() } catch { hasKey = false }
    let settings = null
    try { settings = await backend.readSettings() } catch { /* the defaults stand */ }
    model = nanoBananaModel(settings?.nanoBananaModel).id
  })

  /** @param {() => Promise<unknown>} action */
  async function changeKey(action) {
    failed = false
    try {
      await action()
      draft = ''
      hasKey = await backend.nanoBananaHasKey()
      await refreshNanoBanana(backend)
    } catch {
      failed = true
    }
  }

  /** @param {string} next */
  function saveModel(next) {
    model = next
    void writeSettingsSerialized(backend, { nanoBananaModel: next })
  }
</script>

<section aria-labelledby="{uid}-heading" data-settings-anchor="nano-banana">
  <h3 class="sub" id="{uid}-heading">{t('nanoBanana.settings.heading')}</h3>
  <p class="intro">{t('nanoBanana.settings.description')}</p>
  {#if !session.cloudAllowed}
    <p class="intro">{t('nanoBanana.settings.cloudOff')}</p>
  {/if}

  <Field
    label={t('nanoBanana.settings.key')}
    description={t(hasKey ? 'nanoBanana.settings.keySaved' : 'nanoBanana.settings.keyMissing')}
    layout="row"
    controlId={ids.key}
  >
    {#snippet children({ descriptionId })}
      <div class="key">
        <input
          id={ids.key}
          class="text"
          type="password"
          autocomplete="off"
          spellcheck="false"
          aria-describedby={descriptionId}
          bind:value={draft}
        />
        <Button size="sm" variant="soft" disabled={!draft.trim()}
          onclick={() => changeKey(() => backend.storeNanoBananaKey({ key: draft }))}>
          {t('nanoBanana.settings.save')}
        </Button>
        {#if hasKey}
          <Button size="sm" variant="ghost" onclick={() => changeKey(() => backend.deleteNanoBananaKey())}>
            {t('nanoBanana.settings.remove')}
          </Button>
        {/if}
      </div>
    {/snippet}
  </Field>
  {#if failed}
    <p class="intro" role="alert">{t('nanoBanana.settings.failed')}</p>
  {/if}

  <Field
    label={t('nanoBanana.settings.model')}
    description={t('nanoBanana.settings.modelHint')}
    layout="row"
    controlId={ids.model}
  >
    {#snippet children({ descriptionId })}
      <Select id={ids.model} options={models} value={model} describedBy={descriptionId} onchange={saveModel} />
    {/snippet}
  </Field>
</section>

<style>
  .key {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .text {
    min-width: 0;
    width: 180px;
  }
</style>
