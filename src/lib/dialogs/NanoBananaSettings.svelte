<script module>
  /**
   * Whether the key saved this session is held for this session only. Kept
   * past the panel, which Settings mounts again each time it opens: the key
   * outlives the panel, and so does what the panel knows about it.
   */
  const stored = $state({ sessionOnly: false })
</script>

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
   * Where the system has no credential store the key is kept for this session
   * only (`sessionOnly`), and the panel says so: it is gone when the app closes.
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

  /** A Gemini key is 39 characters; this only stops a paste of something else entirely. */
  const MAX_KEY_CHARS = 256
  /** The codes the key commands reject with, each its own message. */
  const KEY_ERRORS = ['nanoBanana.key.noStore', 'nanoBanana.key.invalid', 'nanoBanana.key.failed']

  let draft = $state('')
  let hasKey = $state(false)
  let busy = $state(false)
  /** The message for the last change that failed. @type {string|null} */
  let failed = $state(null)
  let model = $state(nanoBananaModel(null).id)

  onMount(async () => {
    try { hasKey = await backend.nanoBananaHasKey() } catch { hasKey = false }
    let settings = null
    try { settings = await backend.readSettings() } catch { /* the defaults stand */ }
    model = nanoBananaModel(settings?.nanoBananaModel).id
  })

  /** @param {unknown} error */
  function keyErrorOf(error) {
    const code = error instanceof Error ? error.message : String(error ?? '')
    return KEY_ERRORS.includes(code) ? code : 'nanoBanana.settings.failed'
  }

  /**
   * Store or remove the key, then read again whether one is there. The read
   * is its own step: a key that changed and could not be read back is still
   * changed, and the engines are refreshed either way.
   *
   * @param {() => Promise<unknown>} action
   * @param {boolean} storing
   */
  async function changeKey(action, storing) {
    if (busy) return
    busy = true
    failed = null
    try {
      try {
        const answer = /** @type {any} */ (await action())
        draft = ''
        stored.sessionOnly = storing && answer?.sessionOnly === true
        hasKey = storing
      } catch (error) {
        console.error('the Nano Banana key could not be changed', error)
        failed = keyErrorOf(error)
        return
      }
      try {
        hasKey = await backend.nanoBananaHasKey()
      } catch (error) {
        console.error('whether a Nano Banana key is saved could not be read', error)
      }
      await refreshNanoBanana(backend)
    } finally {
      busy = false
    }
  }

  function saveKey() {
    if (!draft.trim()) return
    void changeKey(() => backend.storeNanoBananaKey({ key: draft }), true)
  }

  /** @param {string} next */
  async function saveModel(next) {
    const before = model
    model = next
    failed = null
    try {
      await writeSettingsSerialized(backend, { nanoBananaModel: next })
    } catch (error) {
      console.error('the Nano Banana model could not be saved', error)
      // A later pick has already replaced this one: that pick is what stands.
      if (model === next) model = before
      failed = 'nanoBanana.settings.modelFailed'
    }
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
    description={t(!hasKey ? 'nanoBanana.settings.keyMissing'
      : stored.sessionOnly ? 'nanoBanana.settings.sessionOnly' : 'nanoBanana.settings.keySaved')}
    layout="row"
    controlId={ids.key}
  >
    {#snippet children({ descriptionId })}
      <!-- A form, so Enter in the box saves as the button does. -->
      <form class="key" onsubmit={(event) => { event.preventDefault(); saveKey() }}>
        <input
          id={ids.key}
          class="text"
          type="password"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          maxlength={MAX_KEY_CHARS}
          aria-describedby={descriptionId}
          disabled={busy}
          bind:value={draft}
        />
        <Button type="submit" size="sm" variant="soft" disabled={busy || !draft.trim()}>
          {t('nanoBanana.settings.save')}
        </Button>
        {#if hasKey}
          <Button size="sm" variant="ghost" disabled={busy}
            onclick={() => changeKey(() => backend.deleteNanoBananaKey(), false)}>
            {t('nanoBanana.settings.remove')}
          </Button>
        {/if}
      </form>
    {/snippet}
  </Field>
  {#if failed}
    <p class="intro" role="alert">{t(failed)}</p>
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
