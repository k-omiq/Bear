<script>
  /**
   * Settings > Cloud > Nano Banana: the user's own Gemini API keys, as
   * profiles, beside the cloud endpoints and drawn like them
   * (`InferenceSettings.svelte` mounts it under Endpoints).
   *
   * **Profiles.** Each has a name, a model (`model/nanobanana.js`) and its own
   * key. One is active: the next Nano Banana window pays with its key, and the
   * window, the grant, the batch report and each layer's record name it
   * (`inference/nano_banana_profiles.rs`). The list here is the profile
   * commands' answer, never settings: a key never crosses the seam, and the
   * list only says whether each one is saved. The single key of before is the
   * first profile, with nothing for the person to do.
   *
   * A key is write-only. It goes to the OS credential store, or is kept for
   * this session only where the system has none (`sessionOnly`), and the row
   * says so. There is no size setting: native picks each region's crop size.
   *
   * **Finishing is not here.** A Nano Banana result is finished by the shared
   * Settings > Post-processing choice, as every other engine's is.
   *
   * Two choices say what goes to Google, for every profile. "Show each crop
   * before it is sent" (`nanoBananaShowCrop`) holds every request on a window
   * with the crop (`NanoBananaCropDialog.svelte`); native reads it at each
   * request. "Restrictive crop" (`nanoBananaRestrictive`) sends only small
   * areas around each region and the rest as plain white; native reads it
   * when a window is confirmed, so it counts from the next one.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { onMount, tick, untrack } from 'svelte'
  import Icon from '../icons/Icon.svelte'
  import { Button, Field, Select, TextInput } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import { refreshNanoBanana } from '../state/capabilities.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import { DEFAULT_NANO_BANANA_MODEL, NANO_BANANA_MODELS, nanoBananaModel } from '../model/nanobanana.js'
  import { hasKey as hasMessage, t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const uid = $props.id()
  const ids = {
    heading: `${uid}-heading`,
    name: `${uid}-name`, key: `${uid}-key`, model: `${uid}-model`,
    rename: `${uid}-rename`, replace: `${uid}-replace`, removeText: `${uid}-remove-text`,
    showCrop: `${uid}-show-crop`, restrictive: `${uid}-restrictive`,
  }
  /** The settings keys of what is sent, by part. */
  const SENDING_KEYS = { showCrop: 'nanoBananaShowCrop', restrictive: 'nanoBananaRestrictive' }
  const models = NANO_BANANA_MODELS.map((entry) => ({ value: entry.id, label: t(entry.labelKey) }))

  /** A Gemini key is 39 characters; this only stops a paste of something else entirely. */
  const MAX_KEY_CHARS = 256

  /** @type {import('../api/backend.js').NanoBananaProfiles} */
  let listed = $state({ active: null, profiles: [] })
  let loaded = $state(false)
  let loadFailed = $state(false)
  let busy = $state(false)
  /** The message for the last change that failed, and the profile it was about. @type {{key: string, id: string|null}|null} */
  let failed = $state(null)
  let sending = $state({ showCrop: false, restrictive: false })

  /** Which inline form is open: one at a time, as on the endpoint list. @type {{kind: 'add'}|{kind: 'rename'|'key'|'remove', id: string}|null} */
  let open = $state(null)
  let draftName = $state('')
  let draftKey = $state('')
  let draftModel = $state(DEFAULT_NANO_BANANA_MODEL)

  onMount(async () => {
    await reload()
    let settings = null
    try { settings = await backend.readSettings() } catch { /* the defaults stand */ }
    sending = {
      showCrop: settings?.[SENDING_KEYS.showCrop] === true,
      restrictive: settings?.[SENDING_KEYS.restrictive] === true,
    }
  })

  async function reload() {
    loadFailed = false
    try {
      listed = await backend.listNanoBananaProfiles()
      loaded = true
    } catch (error) {
      console.error('the Nano Banana profiles could not be read', error)
      loadFailed = true
    }
  }

  /** @param {unknown} error */
  function messageOf(error) {
    const code = error instanceof Error ? error.message : String(error ?? '')
    return /^nanoBanana\.(key|profile)\./.test(code) && hasMessage(code) ? code : 'nanoBanana.settings.failed'
  }

  /**
   * Run one profile command, take the list it answers, and refresh whether
   * Nano Banana can be offered. A failure keeps the form open with its message.
   *
   * @param {() => Promise<import('../api/backend.js').NanoBananaProfiles>} action
   * @param {string|null} id - the profile it is about, or null for a new one
   * @param {string} [fallback] - the message for a refusal it does not know
   * @returns {Promise<boolean>}
   */
  async function change(action, id, fallback) {
    if (busy) return false
    busy = true
    failed = null
    try {
      try {
        listed = await action()
      } catch (error) {
        console.error('a Nano Banana profile could not be changed', error)
        const key = messageOf(error)
        failed = { key: key === 'nanoBanana.settings.failed' && fallback ? fallback : key, id }
        return false
      }
      await refreshNanoBanana(backend)
      return true
    } finally {
      busy = false
    }
  }

  /** @param {{kind: 'add'}|{kind: 'rename'|'key'|'remove', id: string}|null} next @param {string} [focusId] */
  async function openForm(next, focusId) {
    open = next
    failed = null
    draftKey = ''
    if (next?.kind === 'add') {
      draftName = listed.profiles.length === 0 ? t('nanoBanana.settings.heading') : ''
      draftModel = DEFAULT_NANO_BANANA_MODEL
    }
    if (next?.kind === 'rename') draftName = listed.profiles.find((profile) => profile.id === next.id)?.name ?? ''
    await tick()
    if (focusId) /** @type {HTMLElement|null} */ (document.getElementById(focusId))?.focus()
  }

  function closeForm() {
    open = null
    draftKey = ''
    failed = null
  }

  async function create() {
    if (!draftName.trim() || !draftKey.trim()) return
    const made = await change(() => backend.createNanoBananaProfile({ name: draftName.trim(), model: draftModel, key: draftKey }), null)
    if (made) closeForm()
  }

  /** @param {string} id */
  async function rename(id) {
    if (!draftName.trim()) return
    if (await change(() => backend.updateNanoBananaProfile({ profileId: id, name: draftName.trim() }), id)) closeForm()
  }

  /** @param {string} id */
  async function replaceKey(id) {
    if (!draftKey.trim()) return
    if (await change(() => backend.storeNanoBananaKey({ profileId: id, key: draftKey }), id)) closeForm()
  }

  /** @param {string} id */
  async function remove(id) {
    if (await change(() => backend.deleteNanoBananaProfile({ profileId: id }), id)) closeForm()
  }

  /** @param {string} id */
  function choose(id) {
    if (listed.active === id) return
    void change(() => backend.selectNanoBananaProfile({ profileId: id }), id, 'nanoBanana.settings.selectFailed')
  }

  /** @param {string} id @param {string} model */
  function changeModel(id, model) {
    void change(() => backend.updateNanoBananaProfile({ profileId: id, model }), id, 'nanoBanana.settings.modelFailed')
  }

  /** @param {import('../api/backend.js').NanoBananaProfile} profile */
  function keyText(profile) {
    if (profile.hasKey === null) return t('nanoBanana.settings.keyUnknown')
    if (!profile.hasKey) return t('nanoBanana.settings.keyMissing')
    return t(profile.sessionOnly ? 'nanoBanana.settings.sessionOnly' : 'nanoBanana.settings.keySaved')
  }

  /**
   * Save one choice of what is sent: a later change of the same choice wins,
   * a failed save puts back what was there.
   *
   * @param {'showCrop'|'restrictive'} part
   * @param {boolean} next
   */
  async function saveSending(part, next) {
    const before = sending[part]
    sending = { ...sending, [part]: next }
    failed = null
    try {
      await writeSettingsSerialized(backend, { [SENDING_KEYS[part]]: next })
    } catch (error) {
      console.error('the Nano Banana sending choice could not be saved', error)
      if (sending[part] === next) sending = { ...sending, [part]: before }
      failed = { key: 'nanoBanana.settings.sendingFailed', id: null }
    }
  }

  /** @param {KeyboardEvent} event */
  function escapeCloses(event) {
    if (event.key !== 'Escape') return
    // Closes this form, not the Settings dialog around it.
    event.stopPropagation()
    closeForm()
  }
</script>

{#snippet secretBox(id, label)}
  <div class="key">
    <label class="key-label" for={id}>{label}</label>
    <input
      {id}
      class="secret"
      type="password"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
      maxlength={MAX_KEY_CHARS}
      disabled={busy}
      bind:value={draftKey}
      onkeydown={escapeCloses}
    />
  </div>
{/snippet}

{#snippet formError(id)}
  {#if failed && failed.id === id && failed.key !== 'nanoBanana.settings.sendingFailed'}
    <p class="error" role="alert">{t(failed.key)}</p>
  {/if}
{/snippet}

<section class="nano" aria-labelledby={ids.heading} data-settings-anchor="nano-banana">
  <h3 class="title" id={ids.heading} tabindex="-1" data-anchor-focus>{t('nanoBanana.settings.heading')}</h3>
  <p class="note">{t('nanoBanana.settings.description')}</p>
  {#if !session.cloudAllowed}
    <p class="note">{t('nanoBanana.settings.cloudOff')}</p>
  {/if}

  {#if loadFailed}
    <div class="problem" role="alert">
      <Icon name="warning-triangle" size={13} />
      <p>{t('nanoBanana.settings.loadFailed')}</p>
      <Button size="sm" onclick={() => void reload()}>{t('nanoBanana.settings.retry')}</Button>
    </div>
  {:else if loaded && listed.profiles.length === 0}
    <p class="empty">{t('nanoBanana.settings.empty')}</p>
  {:else if listed.profiles.length > 0}
    {#if listed.profiles.length > 1}<p class="note switch">{t('nanoBanana.settings.switchNote')}</p>{/if}
    <ul class="list">
      {#each listed.profiles as profile, index (profile.id)}
        {@const chosen = listed.active === profile.id}
        <li class="row" class:chosen data-nano-banana-profile={profile.id}>
          <div class="main">
            <input
              type="radio"
              class="pick"
              id="{uid}-pick-{index}"
              name="{uid}-active"
              value={profile.id}
              checked={chosen}
              disabled={busy}
              aria-label={t('nanoBanana.settings.useProfile', { name: profile.name })}
              onchange={() => choose(profile.id)}
            />
            <div class="text">
              <div class="name-line">
                <label class="name" for="{uid}-pick-{index}">{profile.name}</label>
                {#if chosen}<span class="badge">{t('nanoBanana.settings.active')}</span>{/if}
                <button type="button" class="link" data-rename={profile.id}
                  onclick={() => openForm({ kind: 'rename', id: profile.id }, ids.rename)}>
                  {t('nanoBanana.settings.rename')}
                </button>
              </div>
              <div class="facts">
                <span class="token" class:missing={profile.hasKey !== true}>{keyText(profile)}</span>
                <button type="button" class="link" data-key={profile.id}
                  onclick={() => openForm({ kind: 'key', id: profile.id }, ids.replace)}>
                  {profile.hasKey ? t('nanoBanana.settings.replaceKey') : t('nanoBanana.settings.addKey')}
                </button>
              </div>
              <div class="model">
                <Select
                  fit
                  options={models}
                  value={nanoBananaModel(profile.model).id}
                  label={t('nanoBanana.settings.modelOf', { name: profile.name })}
                  disabled={busy}
                  onchange={(next) => changeModel(profile.id, next)}
                />
              </div>
              {#if !open && failed?.id === profile.id}<p class="error" role="alert">{t(failed.key)}</p>{/if}
            </div>
            <div class="row-actions">
              <Button size="sm" data-remove={profile.id}
                onclick={() => openForm({ kind: 'remove', id: profile.id }, ids.removeText)}>
                {t('nanoBanana.settings.remove')}
              </Button>
            </div>
          </div>

          {#if open?.kind === 'rename' && open.id === profile.id}
            <form class="inline" onsubmit={(event) => { event.preventDefault(); void rename(profile.id) }}>
              <div class="key">
                <label class="key-label" for={ids.rename}>{t('nanoBanana.settings.renameLabel', { name: profile.name })}</label>
                <TextInput id={ids.rename} value={draftName} maxlength="128" autocomplete="off"
                  onchange={(/** @type {string} */ value) => (draftName = value)} onkeydown={escapeCloses} />
              </div>
              {@render formError(profile.id)}
              <div class="actions end">
                <Button size="sm" onclick={closeForm}>{t('nanoBanana.settings.cancel')}</Button>
                <Button size="sm" variant="primary" type="submit" disabled={busy || !draftName.trim()}>{t('nanoBanana.settings.renameSave')}</Button>
              </div>
            </form>
          {/if}

          {#if open?.kind === 'key' && open.id === profile.id}
            <form class="inline" onsubmit={(event) => { event.preventDefault(); void replaceKey(profile.id) }}>
              {@render secretBox(ids.replace, t('nanoBanana.settings.keyLabel', { name: profile.name }))}
              {@render formError(profile.id)}
              <div class="actions end">
                <Button size="sm" onclick={closeForm}>{t('nanoBanana.settings.cancel')}</Button>
                <Button size="sm" variant="primary" type="submit" disabled={busy || !draftKey.trim()}>{t('nanoBanana.settings.save')}</Button>
              </div>
            </form>
          {/if}

          {#if open?.kind === 'remove' && open.id === profile.id}
            <div class="inline confirm" role="group" aria-labelledby={ids.removeText}>
              <p class="confirm-text" id={ids.removeText} tabindex="-1">{t('nanoBanana.settings.removeConfirm', { name: profile.name })}</p>
              {@render formError(profile.id)}
              <div class="actions end">
                <Button size="sm" onclick={closeForm}>{t('nanoBanana.settings.cancel')}</Button>
                <Button size="sm" variant="primary" disabled={busy} onclick={() => void remove(profile.id)}>
                  {t('nanoBanana.settings.removeButton')}
                </Button>
              </div>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if open?.kind === 'add'}
    <form class="inline add" onsubmit={(event) => { event.preventDefault(); void create() }}>
      <div class="key">
        <label class="key-label" for={ids.name}>{t('nanoBanana.settings.name')}</label>
        <TextInput id={ids.name} value={draftName} maxlength="128" autocomplete="off"
          onchange={(/** @type {string} */ value) => (draftName = value)} onkeydown={escapeCloses} />
      </div>
      {@render secretBox(ids.key, t('nanoBanana.settings.key'))}
      <div class="key">
        <label class="key-label" for={ids.model}>{t('nanoBanana.settings.model')}</label>
        <Select id={ids.model} options={models} value={draftModel} onchange={(next) => (draftModel = next)} />
        <p class="note">{t('nanoBanana.settings.modelHint')}</p>
      </div>
      {@render formError(null)}
      <div class="actions end">
        <Button size="sm" onclick={closeForm}>{t('nanoBanana.settings.cancel')}</Button>
        <Button size="sm" variant="primary" type="submit" disabled={busy || !draftName.trim() || !draftKey.trim()}>
          {t('nanoBanana.settings.create')}
        </Button>
      </div>
    </form>
  {:else if loaded}
    <div class="actions">
      <Button size="sm" variant="soft" data-add-profile onclick={() => openForm({ kind: 'add' }, ids.name)}>
        {t('nanoBanana.settings.add')}
      </Button>
    </div>
  {/if}

  <h4 class="part">{t('nanoBanana.settings.sendingHeading')}</h4>
  <Field
    label={t('nanoBanana.settings.showCrop')}
    description={t('nanoBanana.settings.showCropHint')}
    layout="row"
    controlId={ids.showCrop}
  >
    {#snippet children({ descriptionId })}
      <input id={ids.showCrop} class="check" type="checkbox" aria-describedby={descriptionId}
        checked={sending.showCrop}
        onchange={(event) => saveSending('showCrop', event.currentTarget.checked)} />
    {/snippet}
  </Field>
  <Field
    label={t('nanoBanana.settings.restrictive')}
    description={t('nanoBanana.settings.restrictiveHint')}
    layout="row"
    controlId={ids.restrictive}
  >
    {#snippet children({ descriptionId })}
      <input id={ids.restrictive} class="check" type="checkbox" aria-describedby={descriptionId}
        checked={sending.restrictive}
        onchange={(event) => saveSending('restrictive', event.currentTarget.checked)} />
    {/snippet}
  </Field>
  {#if failed?.key === 'nanoBanana.settings.sendingFailed'}
    <p class="error" role="alert">{t(failed.key)}</p>
  {/if}
</section>

<style>
  /* Drawn as the endpoint list above it is (`InferenceSettings.svelte`). */
  .nano {
    display: grid;
    gap: var(--s-2);
    min-width: 0;
  }
  .title {
    margin: 0;
    font-size: 11px;
    font-weight: 600;
    color: var(--t3);
    outline: none;
  }
  .note,
  .empty,
  .error {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.5;
  }
  .note,
  .empty { color: var(--t3) }
  .error { color: var(--warn) }
  .problem {
    display: flex;
    gap: var(--s-3);
    align-items: center;
    padding: 10px 12px;
    border-radius: var(--r-md);
    background: var(--panel2);
    color: var(--warn);
  }
  .problem p {
    flex: 1;
    margin: 0;
    line-height: 1.5;
    color: var(--text);
  }
  .list {
    display: grid;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
    list-style: none;
  }
  .row {
    display: grid;
    gap: var(--s-3);
    padding: 10px 0;
    border-bottom: 1px solid var(--line);
  }
  .main {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: var(--s-3);
    align-items: start;
  }
  .pick {
    margin: 3px 0 0;
    accent-color: var(--accent);
  }
  .text {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .name-line {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
    align-items: baseline;
  }
  .name {
    min-width: 0;
    overflow-wrap: anywhere;
    color: var(--text);
    cursor: pointer;
  }
  .badge {
    padding: 0 6px;
    border-radius: var(--r-pill);
    background: var(--accent-soft);
    color: var(--text);
    font-size: 10.5px;
    line-height: 16px;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--s-3);
    align-items: baseline;
    font-size: 11.5px;
  }
  .token { color: var(--t2) }
  .token.missing { color: var(--warn) }
  .model { justify-self: start }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--t2);
    font: inherit;
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .link:hover,
  .link:focus { color: var(--text) }
  .name-line .link { font-size: 11.5px }
  .switch { margin-bottom: var(--s-1) }
  .row-actions {
    display: flex;
    gap: var(--s-2);
  }
  .inline {
    display: grid;
    gap: var(--s-3);
    margin-left: 22px;
    padding: 10px 12px;
    border-radius: var(--r-md);
    background: var(--panel2);
  }
  .inline.add { margin-left: 0 }
  .confirm-text {
    margin: 0;
    line-height: 1.5;
    color: var(--text);
    outline: none;
  }
  .key { display: grid; gap: var(--s-1) }
  .key-label { font-size: 11.5px; color: var(--t2) }
  .secret {
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
  }
  .actions.end { justify-content: flex-end }
  .part {
    margin: var(--s-3) 0 var(--s-1);
    font-size: 12px;
    font-weight: 600;
    color: var(--t2);
  }
  .check {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
    cursor: pointer;
  }
  @media (max-width: 560px) {
    .main { grid-template-columns: auto minmax(0, 1fr) }
    .row-actions { grid-column: 2 }
  }
</style>
