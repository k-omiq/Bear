<script>
  import { untrack } from 'svelte'
  import { Button, Field, Menu, Modal, TextInput } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { closeModal, modalWidth } from '../../state/app.svelte.js'
  import { createChapter, library } from '../library.svelte.js'
  import { REJECTED_CHARACTERS, isChapterNumber, parseChapterNumber, sameChapterNumber } from '../chapternumber.js'
  import SourceFolderField from './SourceFolderField.svelte'

  /**
   * New chapter.
   *
   * The project is picked from a menu rather than from a row of chips: chips
   * are the design file's shape and they are right for two options, but a
   * scanlator with forty projects gets four wrapped rows of them and a dialog
   * that changes height as it filters.
   *
   * Mode is not offered here at all - it belongs to the project.
   *
   * **The number is the user's.** It used to be derived - `max + 1` here for
   * display and `max + 1` again in the backend - so a scanlator adding Ch. 12
   * to a project holding Ch. 1 got Ch. 2, with no field to say otherwise. The
   * field below is the number the chapter gets; the backend stores it verbatim.
   * `max + 1` is only its starting value.
   *
   * **A split chapter is typed as it is numbered**: `12.5`, `12-1`, `12/2` or
   * `12,5` (`../chapternumber.js`). The field drops any other character as it
   * arrives. A plain number goes to the seam as `number`; a split one goes as
   * `label` too, and the backend keeps the leading whole number for its own
   * numbering. The title is free text, so `Ch. 12, part 2` was always
   * typeable there.
   *
   * **The source folder is offered, and left empty.** A chapter's pages come
   * from a folder, and until that seam gap was closed the seam
   * had nowhere to put one, so the backend guessed - with the result that
   * two chapters which both fell back to the project's folder
   * held the same files. Leaving it empty keeps the guess for the case it is
   * right for, the first chapter of a project that already points at a folder
   * of scans; the field is what makes the second chapter possible. It is not
   * pre-filled, because a path put there by this dialog would be this dialog
   * guessing in the user's name - and the note under it says what empty means
   * rather than leaving them to find out.
   *
   * @type {{ spec: import('../../state/app.svelte.js').ModalSpec }}
   */
  let { spec } = $props()

  const uid = $props.id()
  const titleId = `${uid}-title`
  const numberId = `${uid}-number`

  const projects = $derived(library.projects)

  // The spec is fixed for the life of this dialog (the host keys it by modal
  // id), so the preselected project is an initial value, not a binding.
  let chosenId = $state(
    untrack(() => /** @type {string|null} */ (/** @type {any} */ (spec.props)?.projectId ?? null)),
  )
  const project = $derived(
    projects.find((candidate) => candidate.id === chosenId) ?? projects[0] ?? null,
  )

  // From each chapter's whole number: a split `12.5` counts as 12.
  const suggestedNumber = $derived(
    project
      ? Math.max(...project.chapters.map((chapter) => parseChapterNumber(chapter.number)?.whole ?? 0), 0) + 1
      : 1,
  )

  // Held as the string the field holds, not as a number: an empty field is a
  // state the user passes through while retyping, and `0`/`NaN` are both lies
  // about what is in it.
  let numberText = $state('')
  let numberTouched = $state(false)
  let title = $state('')
  let titleTouched = $state(false)
  let sourcePath = $state('')

  $effect(() => {
    if (!numberTouched) numberText = String(suggestedNumber)
  })

  // The text as the user typed it, trimmed; `null` while the field is empty
  // or holds something the rule refuses (`12.`, half way to `12.5`).
  const number = $derived(isChapterNumber(numberText) ? numberText.trim() : null)
  const parsed = $derived(number === null ? null : parseChapterNumber(number))

  // Until it is edited, the title tracks the number the chapter will be given,
  // so adding a run of chapters needs no typing at all.
  $effect(() => {
    const suggested = t('home.newChapter.defaultTitle', { number: number ?? suggestedNumber })
    if (!titleTouched) title = suggested
  })

  // A number already in the project is refused here rather than in the backend:
  // the two chapters would be indistinguishable in every list that names a
  // chapter by its number, and the user is standing in front of the field that
  // fixes it.
  const taken = $derived(
    number !== null && !!project && project.chapters.some((chapter) => sameChapterNumber(chapter.number, number)),
  )

  const numberValid = $derived(number !== null && !taken)

  const canCreate = $derived(!!project && title.trim() !== '' && numberValid && !library.busy)

  async function create() {
    if (!canCreate || !project || number === null || !parsed) return
    const chapter = await createChapter({
      projectId: project.id,
      name: title.trim(),
      number: parsed.whole,
      ...(parsed.label !== null ? { label: parsed.label } : {}),
      sourcePath: sourcePath.trim() || undefined,
    })
    // Only on success: a failed create keeps the dialog, the typed title and
    // the typed path. A create refused because the folder the backend would
    // have inferred is already read by another chapter comes back null with a
    // notice saying which - and this dialog standing open with the source field
    // in it is exactly what that notice asks the user to do something about.
    if (chapter) closeModal('create')
  }

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    if (event.key === 'Enter') {
      event.preventDefault()
      create()
    }
  }
</script>

<Modal
  title={t(spec.titleKey)}
  width={modalWidth(spec.kind)}
  onclose={() => closeModal(null)}
>
  <div class="form">
    <Field label={t('home.newChapter.project')}>
      {#snippet children()}
        <Menu
          items={projects.map((candidate) => ({
            id: candidate.id,
            label: candidate.name,
            selected: candidate.id === project?.id,
          }))}
          onselect={(id) => {
            chosenId = id
          }}
          label={t('home.newChapter.project')}
        >
          {#snippet trigger({ toggle, triggerProps })}
            <Button onclick={toggle} {...triggerProps}>
              {project ? project.name : t('home.newChapter.noProject')}
            </Button>
          {/snippet}
        </Menu>
      {/snippet}
    </Field>

    <Field label={t('home.newChapter.number')} controlId={numberId}>
      {#snippet children()}
        <TextInput
          id={numberId}
          numeric
          reject={REJECTED_CHARACTERS}
          value={numberText}
          onchange={(value) => {
            numberTouched = true
            numberText = value
          }}
          {onkeydown}
        />
      {/snippet}
    </Field>

    <Field label={t('home.newChapter.title')} controlId={titleId}>
      {#snippet children()}
        <TextInput
          id={titleId}
          value={title}
          onchange={(value) => {
            titleTouched = true
            title = value
          }}
          {onkeydown}
        />
      {/snippet}
    </Field>

    <SourceFolderField
      value={sourcePath}
      onchange={(value) => (sourcePath = value)}
      label={t('home.newChapter.source')}
      placeholder={t('home.newChapter.sourcePlaceholder')}
      browseLabel={t('shell.action.chooseFolder')}
      chooserTitle={t('home.newChapter.source')}
      defaultPath={project?.sourcePath}
      {onkeydown}
    />

    <!-- The one thing about this field a user cannot see: the pages are taken
         into the library, so the folder is read once and never again. Said
         here rather than in a notice afterwards, because it is what decides
         whether the folder can be tidied away. -->
    <p class="note">{t('home.newChapter.sourceNote')}</p>

    {#if taken}
      <p class="note">{t('home.newChapter.taken', { number })}</p>
    {/if}
  </div>

  {#snippet buttons()}
    <Button onclick={() => closeModal(null)}>{t('shell.action.cancel')}</Button>
    <Button variant="primary" disabled={!canCreate} onclick={create}>
      {t('home.newChapter.create', { number: number ?? suggestedNumber })}
    </Button>
  {/snippet}
</Modal>

<style>
  .form { display: flex; flex-direction: column; gap: var(--s-5); margin-top: 16px }
  .note { margin: 0; font-size: 10.5px; line-height: 1.5; color: var(--t3) }
</style>
