<script>
  /**
   * Export: format, pages, layout, colour, masks and destination, and what
   * the user needs to know before pressing the button.
   *
   * The format is the one big choice, so it is a row of cards that say what
   * each format is for. The rest are short closed lists, laid out as one
   * aligned grid, and everything the plan says sits in one summary panel
   * above the buttons.
   *
   * Every change asks the backend for a plan. The plan reads page headers and
   * the manifest only, so it is quick; it names the output formats, what the
   * export declares, and a revision the export must still match.
   *
   * Only the overwrite refusal opens its own modal ("Choose another
   * folder…"). Every other refusal is fixed by changing a choice here, so the
   * dialog stays open with the sentence under it. The draft lives in
   * `spec.props`, so it survives that modal unmounting this one.
   */
  import { untrack } from 'svelte'
  import { Button, Field, Modal, Segmented } from '../ui/index.js'
  import SourceFolderField from '../home/dialogs/SourceFolderField.svelte'
  import { chooseFolder } from '../api/folder.js'
  import { closeModal, modalWidth, notify, pushModal } from '../state/app.svelte.js'
  import { editor, pageCount, pages, reportJobConflict, reviewEntries } from '../state/editor.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { session } from '../state/session.svelte.js'
  import { t } from '../i18n/index.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()

  // Read once. `spec` is fixed for the life of this block - the host keys it by
  // modal id - and `draft` is the reactive object underneath it, not a snapshot.
  const draft = untrack(() => /** @type {Record<string, any>} */ (spec.props))
  if (draft.format === undefined) draft.format = 'PNG'
  if (draft.destination === undefined) draft.destination = 'new-folder'
  if (draft.masks === undefined) draft.masks = 'flattened'
  if (draft.layout === undefined) draft.layout = 'per-page'
  if (draft.path === undefined) draft.path = ''
  if (draft.scope === undefined) draft.scope = 'chapter'
  // Settings > Export chooses where the colour choice starts; this export's
  // choice is never written back.
  if (draft.tone === undefined) draft.tone = session.exportTone

  let busy = $state(false)
  let plan = $state(null)
  let planning = $state(false)
  let planError = $state('')
  let refresh = $state(0)

  const formats = [
    { value: 'PNG', label: t('export.format.png'), hint: t('export.format.pngHint') },
    { value: 'TIFF', label: t('export.format.tiff'), hint: t('export.format.tiffHint') },
    { value: 'PSD', label: t('export.format.psd'), hint: t('export.format.psdHint') },
    { value: 'CBZ', label: t('export.format.cbz'), hint: t('export.format.cbzHint') },
  ]
  const tones = [
    { value: 'source', label: t('export.tone.source') },
    { value: 'grayscale', label: t('export.tone.grayscale') },
  ]
  const uid = $props.id()
  const layouts = [
    { value: 'per-page', label: t('export.layout.perPage') },
    { value: 'stitched', label: t('export.layout.stitched') },
  ]
  const destinations = [
    { value: 'new-folder', label: t('export.destination.newFolder') },
    { value: 'source-folder', label: t('export.destination.sourceFolder') },
    { value: 'custom', label: t('export.destination.customFolder') },
  ]
  const scopes = $derived([
    { value: 'chapter', label: t('export.pages.chapter') },
    { value: 'page', label: t('export.pages.current', { page: editor.pageIndex + 1 }) },
  ])
  const maskModes = [
    { value: 'flattened', label: t('export.masks.flattened') },
    { value: 'separate-layer', label: t('export.masks.separateLayer') },
  ]

  const pageList = $derived(pages())

  /**
   * One file for the chapter only makes sense for a chapter that *is* one
   * document. A paginated project's pages are separate images that happen to be
   * listed in an order, so the row is not shown at all - and a CBZ already
   * holds one file per page, so it cannot hold one file for the chapter, and
   * a PSD holds one whole page as its Background, which the chapter never is.
   * The backend refuses all three combinations; not offering them is what
   * keeps a user from meeting a refusal they could not have predicted.
   */
  const longstrip = $derived(editor.project?.mode === 'longstrip')
  const stitchable = $derived(longstrip && draft.format !== 'CBZ' && draft.format !== 'PSD')
  const layout = $derived(stitchable && draft.layout === 'stitched' ? 'stitched' : 'per-page')

  /**
   * A stitched file and a CBZ are each one file for the whole chapter, so
   * they always hold every page; the backend refuses a part of one.
   */
  const pageable = $derived(layout !== 'stitched' && draft.format !== 'CBZ')
  const onePage = $derived(pageable && draft.scope === 'page')
  const selection = $derived(onePage ? [editor.pageIndex] : undefined)

  /** Flags on the pages this export writes. */
  const flagged = $derived(
    onePage ? reviewEntries().filter((entry) => entry.pageIndex === editor.pageIndex).length : reviewEntries().length,
  )

  /** The mask choice the seam is sent: a CBZ has no room for one. */
  const maskable = $derived(draft.format !== 'CBZ')
  const masks = $derived(maskable ? draft.masks : 'flattened')

  /**
   * The gutter count, from the pages themselves: the strip is as wide as its widest
   * page (the wider rule centres the rest), so every
   * narrower page contributes the columns beside it.
   */
  const gutter = $derived.by(() => {
    const width = pageList.reduce((widest, page) => Math.max(widest, page.width ?? 0), 0)
    return pageList.reduce((sum, page) => sum + (width - (page.width ?? 0)) * (page.height ?? 0), 0)
  })

  /** The destination the seam is sent: a sentinel, or the absolute path itself. */
  const destination = $derived(
    draft.destination === 'custom' ? String(draft.path ?? '').trim() : draft.destination,
  )
  const ready = $derived(draft.destination !== 'custom' || destination !== '')

  /**
   * The files the plan writes. A CBZ is one archive whatever it holds, and
   * a mask file goes beside each raster file when masks are separate (a
   * PSD keeps them as layer masks instead).
   */
  const summary = $derived.by(() => {
    if (plan?.status !== 'planned') return { count: 0, formats: '', masks: 0 }
    if (draft.format === 'CBZ') return { count: 1, formats: 'CBZ', masks: 0 }
    const count = plan.fileCount ?? plan.outputs?.length ?? 0
    return { count, formats: plan.actualFormats.join(' / '), masks: masks === 'separate-layer' && draft.format !== 'PSD' ? count : 0 }
  })

  /** The colour choice the seam is sent: a grey page as grayscale, or as it is. */
  const grayscale = $derived(draft.tone === 'grayscale')

  $effect(() => {
    const chapterId = editor.chapter?.id
    const options = { chapterId, format: draft.format, destination, masks, layout, pages: selection, grayscale }
    void refresh
    let cancelled = false
    plan = null
    planError = ''
    if (!chapterId || !ready) {
      planning = false
      return
    }
    planning = true
    getBackend().planExportChapter(options).then((value) => {
      if (!cancelled) { plan = value; planning = false }
    }).catch((error) => {
      if (!cancelled) { planError = String(error); planning = false }
    })
    return () => { cancelled = true }
  })

  async function run() {
    if (busy || !ready || planning || plan?.status !== 'planned' || !editor.chapter) return
    busy = true
    let result
    try {
      result = await getBackend().exportChapter({
        chapterId: editor.chapter.id,
        format: draft.format,
        destination,
        masks,
        layout,
        pages: selection,
        grayscale,
        planRevision: plan.revision,
      })
    } catch (error) {
      busy = false
      // Another Manga Cleaner process using the chapter is said as that.
      if (reportJobConflict(error)) return
      planError = String(error)
      return
    }
    busy = false
    if (!result) return

    if (result?.status === 'refused') {
      // One refusal leads somewhere; the rest were announced with a sentence
      // saying what to change, and this dialog is where it gets changed.
      if (result.reasonKey === 'notice.export.refusedOverwrite') refuse()
      // Both mean the plan on screen is no longer true: plan again, and the
      // dialog shows what the backend just said.
      if (result.reasonKey === 'notice.export.stalePlan') refresh += 1
      if (result.reasonKey === UNAVAILABLE) refresh += 1
      return
    }
    // The adapter announces the export itself (`notice.export.finished`, or
    // `notice.export.stitched` with the gutter count on it), so there is
    // nothing left for the dialog to say.
    if (result?.status === 'exported') closeModal('export')
    else planError = result?.detail ?? t(result?.reasonKey ?? 'notice.export.partial')
  }

  const UNAVAILABLE = 'notice.export.destinationUnavailable'
  let choosing = $state(false)

  /**
   * The way out of a destination that cannot be written. The destination
   * becomes a named folder first, so the path field is on screen whatever the
   * chooser answers: outside a Tauri window there is no chooser, and a
   * dismissed one leaves the field for a typed path.
   */
  async function chooseAnotherFolder() {
    if (choosing) return
    draft.destination = 'custom'
    choosing = true
    try {
      const chosen = await chooseFolder({ title: t('export.destination.chooserTitle') })
      if (chosen) draft.path = chosen
    } catch {
      notify({ key: 'notice.project.sourcePickFailed', params: {}, tone: 'warn' })
    } finally {
      choosing = false
    }
  }

  function refuse() {
    pushModal({
      kind: 'overwriteRefusal',
      titleKey: 'modal.title.overwriteRefusal',
      dismissable: false,
      props: {
        bodyKey: 'export.refusal.body',
        bodyParams: { path: editor.project?.sourcePath ?? '' },
      },
      actions: [
        {
          id: 'chooseFolder',
          labelKey: 'export.action.chooseAnotherFolder',
          variant: 'primary',
        },
      ],
      // The way out has to *be* a way out: the destination goes back to a new
      // folder, and this dialog is underneath, waiting, with the format and the
      // mask choice as they were.
      onresolve: () => {
        draft.destination = 'new-folder'
      },
    })
  }
</script>

<Modal
  title={t(spec.titleKey)}
  meta={t('export.meta.chapter', { count: pageCount(), chapter: editor.chapter?.number ?? '' })}
  width={modalWidth(spec.kind)}
  onclose={() => closeModal(null)}
>
  <fieldset class="formats" disabled={busy}>
    <legend class="heading">{t('export.format.label')}</legend>
    <div class="cards">
      {#each formats as format (format.value)}
        <label class="format" class:chosen={draft.format === format.value}>
          <input
            type="radio"
            name="{uid}-format"
            value={format.value}
            checked={draft.format === format.value}
            aria-labelledby="{uid}-{format.value}"
            aria-describedby="{uid}-{format.value}-hint"
            onchange={() => (draft.format = format.value)}
          />
          <span class="name" id="{uid}-{format.value}">{format.label}</span>
          <span class="hint" id="{uid}-{format.value}-hint">{format.hint}</span>
        </label>
      {/each}
    </div>
  </fieldset>

  <div class="options">
    {#if longstrip}
      <span class="heading" id="{uid}-layout">{t('export.layout.label')}</span>
      <Segmented
        block
        options={layouts}
        value={layout}
        labelledBy="{uid}-layout"
        disabled={busy || !stitchable}
        onchange={(value) => (draft.layout = value)}
      />
    {/if}

    <span class="heading" id="{uid}-pages">{t('export.pages.label')}</span>
    <Segmented
      block
      options={scopes}
      value={onePage ? 'page' : 'chapter'}
      labelledBy="{uid}-pages"
      disabled={busy || !pageable}
      onchange={(value) => (draft.scope = value)}
    />

    <span class="heading" id="{uid}-tone">{t('export.tone.label')}</span>
    <div class="control">
      <Segmented
        block
        options={tones}
        value={draft.tone}
        labelledBy="{uid}-tone"
        describedBy="{uid}-tone-description"
        disabled={busy}
        onchange={(value) => (draft.tone = value)}
      />
      <p class="description" id="{uid}-tone-description">
        {t(grayscale ? 'export.tone.grayscaleDescription' : 'export.tone.sourceDescription')}
      </p>
    </div>

    {#if maskable}
      <span class="heading" id="{uid}-masks">{t('export.masks.label')}</span>
      <Segmented
        block
        options={maskModes}
        value={draft.masks}
        labelledBy="{uid}-masks"
        disabled={busy}
        onchange={(value) => (draft.masks = value)}
      />
    {/if}

    <span class="heading" id="{uid}-destination">{t('export.destination.label')}</span>
    <div class="control">
      <Segmented
        block
        options={destinations}
        value={draft.destination}
        labelledBy="{uid}-destination"
        disabled={busy}
        onchange={(value) => (draft.destination = value)}
      />
      {#if draft.destination === 'custom'}
        <div class="path">
          <SourceFolderField
            value={draft.path}
            onchange={(value) => (draft.path = value)}
            label={t('export.destination.pathLabel')}
            placeholder={t('export.destination.pathPlaceholder')}
            browseLabel={t('shell.action.chooseFolder')}
            chooserTitle={t('export.destination.chooserTitle')}
            disabled={busy}
          />
        </div>
      {/if}
    </div>
  </div>

  <section class="summary" aria-live="polite" aria-label={t('export.summary.label')}>
    {#if planning}
      <p class="quiet">{t('export.state.planning')}</p>
    {:else if plan?.status === 'planned'}
      <p class="files">{t('export.summary.files', summary)}</p>
      {#if summary.masks}<p class="quiet">{t('export.summary.masks', { count: summary.masks })}</p>{/if}
      {#if plan.path}<p class="where" title={plan.path}>{plan.path}</p>{/if}
      {#each plan.declared as declaration}
        <p class="quiet">{t(declaration.key, declaration.params)}</p>
      {/each}
    {/if}
    {#if planError}<p class="problem" role="alert">{planError}</p>{/if}
    {#if plan?.status === 'refused'}
      <p class="problem" role="alert">{t(plan.reasonKey, { format: draft.format, path: plan.path ?? '' })} {plan.detail ?? ''}</p>
      {#if plan.reasonKey === UNAVAILABLE}
        <p><Button disabled={busy || choosing} onclick={chooseAnotherFolder}>{t('export.action.chooseAnotherFolder')}</Button></p>
      {/if}
    {/if}
    {#if layout === 'stitched'}<p class="quiet">{t('export.note.gutter', { count: gutter })}</p>{/if}
    <p class:quiet={!flagged} class:flagged={flagged > 0}>{t('export.note.flagged', { count: flagged })}</p>
  </section>

  {#snippet buttons()}
    <Button disabled={busy} onclick={() => closeModal(null)}>{t('shell.action.cancel')}</Button>
    <Button variant="primary" disabled={busy || !ready || planning || plan?.status !== 'planned'} onclick={run}>
      {busy ? t('export.state.exporting') : t('export.action.export', { format: draft.format })}
    </Button>
  {/snippet}
</Modal>

<style>
  .heading {
    font-size: 10.5px;
    letter-spacing: .14em;
    text-transform: uppercase;
    color: var(--t3);
  }

  /* ---- format cards ------------------------------------------------------ */
  .formats {
    min-width: 0;
    margin: var(--s-6) 0 0;
    padding: 0;
    border: 0;
  }
  .formats legend { margin-bottom: var(--s-3); padding: 0 }
  .cards {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--s-3);
  }
  .format {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 10px 11px 11px;
    border: 1px solid var(--line2);
    border-radius: var(--r-md);
    background: var(--surface);
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .format:hover { border-color: var(--tintline) }
  .format.chosen {
    border-color: var(--accent);
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .format:has(input:focus-visible) { outline: 2px solid var(--accent); outline-offset: 2px }
  .formats:disabled .format { cursor: default; opacity: .6 }
  .format input {
    position: absolute;
    inset: 0;
    margin: 0;
    opacity: 0;
    cursor: inherit;
  }
  .name { font-size: 13px; font-weight: 600; color: var(--text) }
  .hint { font-size: 10.5px; line-height: 1.35; color: var(--t3) }

  /* ---- option grid ------------------------------------------------------- */
  .options {
    display: grid;
    grid-template-columns: 88px minmax(0, 1fr);
    align-items: start;
    gap: var(--s-4) var(--s-5);
    margin-top: var(--s-6);
  }
  .options > .heading { padding-top: 7px }
  .control { display: flex; flex-direction: column; gap: var(--s-2); min-width: 0 }
  .description { margin: 0; font-size: 10.5px; line-height: 1.5; color: var(--t3) }
  .path { margin-top: var(--s-1) }

  /* ---- summary ----------------------------------------------------------- */
  .summary {
    display: flex;
    flex-direction: column;
    gap: 3px;
    margin-top: var(--s-6);
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--surface);
    font-size: 11px;
    line-height: 1.5;
    color: var(--t2);
  }
  .summary p { margin: 0 }
  .files { font-size: 12px; font-weight: 600; color: var(--text) }
  .where {
    overflow: hidden;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 10.5px;
    color: var(--t3);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .quiet { color: var(--t3) }
  .flagged, .problem { color: var(--warn) }
</style>
