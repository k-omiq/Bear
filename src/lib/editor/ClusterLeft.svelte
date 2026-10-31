<script>
  /**
   * Top left: Home, who and what is open, and the two window toggles.
   *
   * `Home` goes to the library root, which is what its tooltip's `H` does. A
   * control that names a shortcut and then goes somewhere else is worse than a
   * second click through the project card; `back()` is untouched and still the
   * one level up.
   */
  import { goLibrary } from '../state/app.svelte.js'
  import { session, toggleWindow, shortcutProps } from '../state/session.svelte.js'
  import { editor, readingDirection } from '../state/editor.svelte.js'
  import { IconButton } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import Pill from './Pill.svelte'
  import ProjectName from './ProjectName.svelte'

  const modeKey = $derived(`project.mode.${editor.project?.mode ?? 'single'}`)
  const directionKey = $derived(`editor.direction.${readingDirection()}`)
</script>

<div class="cluster" role="group" aria-label={t('editor.region.identity')}>
  <!-- The Home button floats on its own, so it sits on the same surface the
       pills do rather than growing a second kind of raised control. -->
  <Pill gap={0} pad="0">
    <IconButton icon="home" label={t('editor.action.home')} {...shortcutProps('app.home')} size={34} onclick={goLibrary} />
  </Pill>

  <Pill gap={10}>
    <div class="name">
      <ProjectName />
      {#if editor.chapter}
        <span class="chapter">{t('editor.identity.chapter', { number: editor.chapter.number })}</span>
      {/if}
    </div>
    <span class="rule"></span>
    <div class="meta">
      {#if editor.chapter}<span class="fact">{editor.chapter.name}</span>{/if}
      <span class="fact">{t(modeKey)}</span>
      <span class="fact">{t(directionKey)}</span>
    </div>
  </Pill>

  <Pill height={34} gap={3} pad="0 4px" label={t('editor.region.windows')}>
    <!-- `data-window-toggle` is how a closing window finds the control that
         opens it again, so focus goes there rather than to `<body>`
         (see `FloatingWindow.svelte`). -->
    <IconButton
      icon="pages"
      label={t('editor.action.pagesWindow')}
      {...shortcutProps('panel.pages')}
      active={session.windows.pages.open}
      pressed={session.windows.pages.open}
      data-window-toggle="pages"
      onclick={() => toggleWindow('pages')}
    />
    <IconButton
      icon="layers"
      label={t('editor.action.layersWindow')}
      {...shortcutProps('panel.masks')}
      active={session.windows.layers.open}
      pressed={session.windows.layers.open}
      data-window-toggle="layers"
      onclick={() => toggleWindow('layers')}
    />
  </Pill>
</div>

<style>
  .cluster {
    position: absolute;
    left: 16px;
    top: 16px;
    z-index: 40;
    display: flex;
    align-items: center;
    gap: var(--s-3);
    max-width: calc(100% - 32px);
    min-width: 0;
  }

  .name {
    display: flex;
    align-items: baseline;
    gap: 4px;
    min-width: 0;
    max-width: 230px;
    font-size: 12px;
    font-weight: 600;
    white-space: nowrap;
  }
  .chapter { flex: none }

  .rule {
    flex: none;
    width: 1px;
    height: 14px;
    background: var(--line);
  }

  .meta {
    display: flex;
    align-items: baseline;
    min-width: 0;
    max-width: 250px;
    font-size: 10.5px;
    color: var(--t3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .fact {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* A separator glyph, not a translated word - each fact stays its own span so
     no sentence has to survive translation. */
  .fact + .fact::before {
    content: '·';
    margin: 0 5px;
    color: var(--line2);
  }
</style>
