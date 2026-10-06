<script>
  /**
   * Export - format, layout, destination, masks, and the things the user needs
   * to know before the button is pressed.
   *
   * **The note is not decoration.** "Masks drawn by hand and masks made
   * automatically export identically" is the answer to the
   * question the Masks row raises, and the flagged count is the answer to a
   * question nothing else on screen asks: exporting with regions still flagged
   * is legal, and a user is owed the figure before they do it rather than
   * after.
   *
   * **The gutter statement is an obligation.**
   * A strip whose pages are not all the same width has columns beside the
   * narrow ones that no source file has a pixel for, and "an interface offering
   * stitched output states the count before it runs, exactly as
   * it requires of every other change
   * the user did not ask for". `gutterPixels` below is that count, computed
   * from the page dimensions this screen already holds; the backend reports
   * what it actually wrote back on `gutterPixels`, and the two are the same
   * arithmetic from the two ends.
   *
   * **What the format row does not offer.** JPEG is gone from it. It is lossy,
   * the export contract puts lossy output below the fidelity line and the build
   * plan puts it behind an acknowledgement flow that does not exist - so the
   * backend refuses it, and a button whose only outcome is a refusal is worse
   * than no button. TIFF took its place: it is the one format here that carries
   * CMYK, and the contract names it for stitched longstrip. PSD is the contract's
   * layered file, per page only.
   *
   * **The Masks row means two things, and says which.** For PNG and TIFF,
   * "Separate layer" is a mask file beside each page - `001.png` and
   * `001_mask.png`. For PSD it is the layered document: the untouched page as
   * the Background, one masked layer per region. The row's description
   * follows the format. For CBZ the row is not shown at all, because a mask
   * file inside a reader's archive would be read as a page and the backend
   * refuses the pair; the seam is sent `'flattened'` for it.
   *
   * **The overwrite refusal, and the ten that are not it.**
   * `backend.exportChapter` answers `status: 'refused'` for eleven different
   * reasons now, and only one of them has a way out this dialog can offer.
   * `notice.export.refusedOverwrite` (output never
   * overwrites input) is pushed *on top of* this dialog with `dismissable:
   * false` - no Escape, no backdrop - and exactly one action, "Choose another
   * folder…", because the doc's way out of a refusal is a named choice and not
   * a dismissal. Every other refusal is answered by changing what was asked
   * for, so the adapter's notice says why and this dialog simply stays open
   * with the choices still in it. Branching on `status` alone would push the
   * overwrite modal over a user who asked for JPEG.
   *
   * **The draft lives in `spec.props`, not in this component.** Pushing the
   * refusal unmounts this dialog - the host mounts only the top of the stack -
   * and `{#key spec.id}` makes the return a fresh mount. Component state would
   * be gone by then, and the user would find the format they chose reset along
   * with the destination. The spec survives on the stack, so the draft does.
   */
  import { untrack } from 'svelte'
  import { Button, Field, Modal, Segmented } from '../ui/index.js'
  import SourceFolderField from '../home/dialogs/SourceFolderField.svelte'
  import { closeModal, modalWidth, pushModal } from '../state/app.svelte.js'
  import { editor, pageCount, pages, reportJobConflict, reviewEntries } from '../state/editor.svelte.js'
  import { getBackend } from '../api/backend.js'
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

  let busy = $state(false)
  let plan = $state(null)
  let planning = $state(false)
  let planError = $state('')
  let refresh = $state(0)

  const formats = [
    { value: 'PNG', label: t('export.format.png') },
    { value: 'TIFF', label: t('export.format.tiff') },
    { value: 'PSD', label: t('export.format.psd') },
    { value: 'CBZ', label: t('export.format.cbz') },
  ]
  const layouts = [
    { value: 'per-page', label: t('export.layout.perPage') },
    { value: 'stitched', label: t('export.layout.stitched') },
  ]
  const destinations = [
    { value: 'new-folder', label: t('export.destination.newFolder') },
    { value: 'source-folder', label: t('export.destination.sourceFolder') },
    { value: 'custom', label: t('export.destination.customFolder') },
  ]
  const maskModes = [
    { value: 'flattened', label: t('export.masks.flattened') },
    { value: 'separate-layer', label: t('export.masks.separateLayer') },
  ]

  const pageList = $derived(pages())
  const flagged = $derived(reviewEntries().length)

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

  $effect(() => {
    const chapterId = editor.chapter?.id
    const options = { chapterId, format: draft.format, destination, masks, layout }
    void refresh
    let cancelled = false
    plan = null
    planError = ''
    if (!chapterId || !ready) return
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
      if (result.reasonKey === 'notice.export.stalePlan') refresh += 1
      return
    }
    // The adapter announces the export itself (`notice.export.finished`, or
    // `notice.export.stitched` with the gutter count on it), so there is
    // nothing left for the dialog to say.
    if (result?.status === 'exported') closeModal('export')
    else planError = result?.detail ?? t(result?.reasonKey ?? 'notice.export.partial')
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
  <div class="rows">
    <Field label={t('export.format.label')} layout="row">
      {#snippet children({ labelId })}
        <Segmented
          options={formats}
          value={draft.format}
          labelledBy={labelId}
          disabled={busy}
          onchange={(value) => (draft.format = value)}
        />
      {/snippet}
    </Field>

    {#if longstrip}
      <Field label={t('export.layout.label')} layout="row">
        {#snippet children({ labelId })}
          <Segmented
            options={layouts}
            value={layout}
            labelledBy={labelId}
            disabled={busy || !stitchable}
            onchange={(value) => (draft.layout = value)}
          />
        {/snippet}
      </Field>
    {/if}

    <Field label={t('export.destination.label')} layout="row">
      {#snippet children({ labelId })}
        <Segmented
          options={destinations}
          value={draft.destination}
          labelledBy={labelId}
          disabled={busy}
          onchange={(value) => (draft.destination = value)}
        />
      {/snippet}
    </Field>

    {#if draft.destination === 'custom'}
      <SourceFolderField
        value={draft.path}
        onchange={(value) => (draft.path = value)}
        label={t('export.destination.pathLabel')}
        placeholder={t('export.destination.pathPlaceholder')}
        browseLabel={t('shell.action.chooseFolder')}
        chooserTitle={t('export.destination.chooserTitle')}
        disabled={busy}
      />
    {/if}

    {#if maskable}
      <Field label={t('export.masks.label')} layout="row">
        {#snippet children({ labelId })}
          <Segmented
            options={maskModes}
            value={draft.masks}
            labelledBy={labelId}
            disabled={busy}
            onchange={(value) => (draft.masks = value)}
          />
        {/snippet}
      </Field>
    {/if}
  </div>

  <p class="note">
    {t('export.note.flagged', { count: flagged })}
    {#if layout === 'stitched'}
      {t('export.note.gutter', { count: gutter })}
    {/if}
  </p>

  <div aria-live="polite" class="note">
    {#if planning}<p>{t('export.state.planning')}</p>{/if}
    {#if planError}<p role="alert">{planError}</p>{/if}
    {#if plan?.status === 'refused'}
      <p role="alert">{t(plan.reasonKey, { format: draft.format, path: plan.path ?? '' })} {plan.detail ?? ''}</p>
    {/if}
    {#if plan?.status === 'planned'}
      <p>{t('export.note.actualFormats', { formats: plan.actualFormats.join(' / ') })}</p>
      {#each plan.declared as declaration}
        <p>{t(declaration.key, declaration.params)}</p>
      {/each}
    {/if}
  </div>

  {#snippet buttons()}
    <Button disabled={busy} onclick={() => closeModal(null)}>{t('shell.action.cancel')}</Button>
    <Button variant="primary" disabled={busy || !ready || planning || plan?.status !== 'planned'} onclick={run}>
      {busy ? t('export.state.exporting') : t('export.action.export', { format: draft.format })}
    </Button>
  {/snippet}
</Modal>

<style>
  .rows { margin-top: 16px }

  .note {
    margin: 12px 0 0;
    font-size: 11px;
    line-height: 1.6;
    color: var(--t3);
  }
</style>
