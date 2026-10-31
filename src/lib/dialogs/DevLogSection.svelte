<script>
  /**
   * Settings > Dev log: what the app wrote down, to read and send to the
   * developer when something goes wrong.
   *
   * The text comes from `backend.readDevLog()` and is never translated: it is
   * the app's own lines, for the person fixing it. `src-tauri/src/devlog.rs`
   * has already taken the secrets and the home folder out by the time it gets
   * here. It is read each time the section opens, because a panel stays mounted
   * behind the others and the log moves while it waits.
   *
   * @type {{ visible: boolean }}
   */
  import { tick, untrack } from 'svelte'
  import { getBackend } from '../api/backend.js'
  import { Button } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  let { visible } = $props()

  let text = $state('')
  /** The sentence under the buttons: an i18n key, or nothing. */
  let status = $state('')
  let loaded = $state(false)
  /** @type {HTMLElement|undefined} */
  let view = $state()
  /** Only the newest read may draw: an older one can answer after it. */
  let latest = 0
  /**
   * Where the reader is, or null at the end. Noted while the panel shows: a
   * hidden panel has no height and has lost its scroll offset.
   * @type {number|null}
   */
  let place = null

  function notePlace() {
    if (!view || view.clientHeight === 0) return
    const atEnd = view.scrollHeight - view.clientHeight - view.scrollTop < 40
    place = atEnd ? null : view.scrollTop
  }

  async function load() {
    const mine = ++latest
    status = ''
    notePlace()
    try {
      const answer = await getBackend().readDevLog()
      if (mine !== latest) return
      text = answer
      loaded = true
      // The newest lines are the ones asked about, and they are at the end,
      // unless this person has scrolled up to read something.
      await tick()
      if (view) view.scrollTop = place ?? view.scrollHeight
    } catch {
      if (mine === latest) status = 'settings.devlog.loadFailed'
    }
  }

  async function copy() {
    try {
      await globalThis.navigator.clipboard.writeText(text)
      status = 'settings.devlog.copied'
    } catch {
      status = 'settings.devlog.copyFailed'
    }
  }

  async function clear() {
    const mine = ++latest
    status = ''
    try {
      await getBackend().clearDevLog()
      if (mine === latest) {
        text = ''
        place = null
      }
    } catch {
      if (mine === latest) status = 'settings.devlog.clearFailed'
    }
  }

  $effect(() => {
    // Only `visible` starts a read: what `load` looks at must not.
    if (visible) untrack(load)
  })
</script>

<p class="intro">{t('settings.devlog.intro')}</p>
<p class="intro">{t('settings.devlog.privacy')}</p>

<div class="actions">
  <Button onclick={load}>{t('settings.devlog.refresh')}</Button>
  <Button onclick={copy} disabled={!text}>{t('settings.devlog.copy')}</Button>
  <Button onclick={clear} disabled={!text}>{t('settings.devlog.clear')}</Button>
</div>
<p class="status" class:failed={status.endsWith('Failed')} role="status">
  {status ? t(status) : ''}
</p>

{#if text}
  <!-- A tab stop of its own: it scrolls, and a keyboard has to reach that. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <pre class="log" tabindex="0" aria-label={t('settings.section.devlog')} bind:this={view} onscroll={notePlace}>{text}</pre>
{:else if loaded}
  <p class="empty">{t('settings.devlog.empty')}</p>
{/if}

<style>
  .intro {
    margin: 0 0 var(--s-3);
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--t2);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
    margin-top: var(--s-4);
  }

  .status {
    min-height: 18px;
    margin: var(--s-2) 0;
    font-size: 12px;
    color: var(--t3);
  }
  .status.failed { color: var(--warn) }

  .empty {
    margin: 0;
    font-size: 12.5px;
    color: var(--t3);
  }

  .log {
    margin: 0;
    max-height: 60vh;
    overflow: auto;
    padding: var(--s-3);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--t2);
    font-family: var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 11.5px;
    line-height: 1.5;
    /* Long paths wrap: a sideways scroll in a narrow window hides the end of
       every line, and the end is where the error is. */
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
</style>
