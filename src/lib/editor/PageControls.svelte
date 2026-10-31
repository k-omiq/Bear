<script>
  import { tick } from 'svelte'
  import {
    editor,
    goToPage,
    pageCount,
    pageByArrow,
    readingDirection,
  } from '../state/editor.svelte.js'
  import { pageNavControls } from '../model/paging.js'
  import { shortcutProps } from '../state/session.svelte.js'
  import { IconButton, Readout, TextInput } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  /**
   * `‹ ›` - page navigation.
   *
   * The two controls sit in fixed positions and are **labelled by reading
   * order, not by screen position**: manga is right-to-left by default, so the
   * left control is *next* and the right control is *previous*.
   * No component decides that -
   * `model/paging.js#pageNavControls` does, and this one asks it for both the
   * action and the label.
   *
   * The readout between them is a button: pressing it turns the page number
   * into a field. Enter jumps there, Escape or a click elsewhere leaves the
   * page alone. The field opens empty, with the current page as its
   * placeholder, so the first digit typed is the first digit of the answer.
   *
   * A blur does not commit, unlike `ProjectName`: a longstrip keeps scrolling
   * under an open field, and a commit on blur would drag the reader back to
   * a number they typed and then walked away from.
   */
  const nav = $derived(pageNavControls(readingDirection()))
  const total = $derived(pageCount())
  const index = $derived(editor.pageIndex + 1)
  // Two digits, as the design file prints it: the readout must not change
  // width as the chapter passes page nine.
  const shown = $derived(String(index).padStart(2, '0'))

  const atFirst = $derived(editor.pageIndex <= 0)
  const atLast = $derived(editor.pageIndex >= total - 1)

  /** @param {'next'|'prev'} action */
  function spent(action) {
    return action === 'next' ? atLast : atFirst
  }

  let jumping = $state(false)
  let draft = $state('')
  /** @type {{focus: () => void}|undefined} */
  let field = $state()
  /** @type {HTMLElement|undefined} */
  let group = $state()

  function begin() {
    draft = ''
    jumping = true
  }

  /**
   * `goToPage` clamps, so a number past the end lands on the last page. An
   * empty field is "never mind".
   *
   * @param {{commit: boolean, refocus: boolean}} how
   */
  async function end({ commit, refocus }) {
    if (!jumping) return
    jumping = false
    if (commit && draft !== '') goToPage(Number(draft) - 1)
    if (refocus) {
      await tick()
      group?.querySelector('button.readout')?.focus()
    }
  }

  // Both keys are stopped here: the keyboard layer's Escape would blur the
  // field, and its Enter is not this field's to give away.
  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    if (event.key !== 'Enter' && event.key !== 'Escape') return
    event.preventDefault()
    event.stopPropagation()
    end({ commit: event.key === 'Enter', refocus: true })
  }

  $effect(() => {
    if (jumping) field?.focus()
  })
</script>

<IconButton
  icon="chevron-left"
  label={t(nav.left.tooltipKey)}
  {...shortcutProps('page.left')}
  size={28}
  disabled={spent(nav.left.action)}
  onclick={() => pageByArrow('left')}
/>
<span class="position" bind:this={group}>
  {#if jumping}
    <span class="field" style:width="calc({Math.max(2, String(total).length)}ch + 22px)">
      <TextInput
        bind:this={field}
        numeric
        size="sm"
        value={draft}
        placeholder={shown}
        label={t('editor.readout.pageJumpField', { total })}
        maxlength={String(total).length}
        onchange={(value) => (draft = value)}
        {onkeydown}
        onblur={() => end({ commit: false, refocus: false })}
      />
    </span>
    <span class="total" aria-hidden="true">/ {total}</span>
  {:else}
    <Readout
      text="{shown} / {total}"
      label={t('editor.readout.pageJump', { index, total })}
      minWidth={48}
      onclick={begin}
    />
  {/if}
</span>
<IconButton
  icon="chevron-right"
  label={t(nav.right.tooltipKey)}
  {...shortcutProps('page.right')}
  size={28}
  disabled={spent(nav.right.action)}
  onclick={() => pageByArrow('right')}
/>

<style>
  .position { display: inline-flex; align-items: center; gap: var(--s-1) }
  .field { flex: none }
  .field :global(.input) { text-align: center }
  .total { padding-right: var(--s-2); color: var(--t2); font-size: 11px; white-space: nowrap }
</style>
