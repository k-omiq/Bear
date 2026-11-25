<script>
  /**
   * The window held while the data folder changes place: a setup, a move or a
   * switch started here (from Settings or Home's card), or a move the other
   * app is running (`data_folder_moving:`). Nothing behind it may open a
   * chapter or start a run meanwhile, so the shell makes the rest inert and
   * drops its shortcuts (`App.svelte`). It goes when the change ends; the
   * status is read again then (`state/datafolder.svelte.js`).
   */
  import { onMount } from 'svelte'
  import { dataFolder } from '../state/datafolder.svelte.js'
  import { t } from '../i18n/index.js'

  /** @type {HTMLElement|undefined} */
  let root = $state()

  onMount(() => root?.focus())
</script>

<div
  bind:this={root}
  class="cover"
  role="alertdialog"
  aria-modal="true"
  aria-busy="true"
  aria-labelledby="data-folder-busy"
  tabindex="-1"
>
  <p id="data-folder-busy" class="line" aria-live="polite">
    {t(dataFolder.moving && dataFolder.busy === null ? 'shell.dataFolder.moving' : 'settings.dataFolder.busy')}
  </p>
</div>

<style>
  .cover {
    position: fixed;
    inset: 0;
    z-index: 80;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--s-8);
    background: var(--scrim);
    color: var(--text);
  }
  .cover:focus { outline: none }

  .line {
    max-width: 420px;
    margin: 0;
    padding: var(--s-5) var(--s-6);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    font-size: 12.5px;
    line-height: 1.5;
    text-align: center;
  }
</style>
