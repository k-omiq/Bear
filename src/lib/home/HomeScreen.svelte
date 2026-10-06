<script>
  /**
   * Home. Both levels of the hierarchy live here - `app.route.name` is
   * `library` (the project grid) or `chapters` (one project's chapter list) -
   * because they are one screen with one header, one scroll container and one
   * set of primary actions.
   *
   * Home owns the library fetch (`./library.svelte.js`) and, while it is the
   * mounted screen, the backend's notice channel: creating a project, adding a
   * chapter and resuming a job all announce themselves from the backend, and
   * the editor's subscription is not live yet.
   */
  import { Button, Empty } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import { app, goLibrary } from '../state/app.svelte.js'
  import { library, loadLibrary, projectById, subscribeHomeNotices } from './library.svelte.js'
  import HomeHeader from './HomeHeader.svelte'
  import LibraryView from './LibraryView.svelte'
  import ChaptersView from './ChaptersView.svelte'
  import { onJobSettled } from '../state/jobs.svelte.js'

  // Mount-time load. Home remounts whenever the editor is left, so the numbers
  // are refetched after a cleaning run without watching anything.
  $effect(() => {
    loadLibrary()
  })

  $effect(subscribeHomeNotices)

  // A job that ends while Home is up changed what its chapter row says: the
  // pages a run cleaned, the files a denoise wrote.
  $effect(() => onJobSettled(() => loadLibrary()))

  const project = $derived(projectById(app.route.projectId))
  const inChapters = $derived(app.route.name === 'chapters')
</script>

<div class="home">
  <div class="inner">
    <HomeHeader />

    <div class="content">
      {#if inChapters}
        {#if project}
          <ChaptersView {project} />
        {:else if library.status === 'loading'}
          <div class="fallback"><Empty title={t('home.state.loading')} /></div>
        {:else}
          <div class="fallback">
            <Empty
              title={t('home.state.projectMissingTitle')}
              body={t('home.state.projectMissingBody')}
            >
              <Button onclick={goLibrary}>{t('home.action.backToProjects')}</Button>
            </Empty>
          </div>
        {/if}
      {:else}
        <LibraryView />
      {/if}
    </div>
  </div>
</div>

<style>
  .home {
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 26px 34px 64px;
    background: var(--bg);
  }

  /* Cards are 168px minimum, so an ultrawide window would otherwise lay out
     fifteen columns of them and a chapter row two metres long. */
  .inner {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 1480px;
    min-height: 100%;
    margin: 0 auto;
  }

  /* No gap of its own: each level owns its drop from the header - the
     library's centred action block (168px) and the chapter list's 96px. */
  .content { width: 100% }

  /* Stands in for the chapter list, so it starts where the chapter list does. */
  .fallback { padding-top: 96px }
</style>
