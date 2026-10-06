<script>
  /**
   * The modal host. Mounts **exactly one** dialog - the top of
   * `app.modals` - because Task 4's `Modal` listens for Escape on `window`
   * and captures focus on mount; two mounted dialogs would both answer one
   * keypress and fight over the focus ring.
   *
   * The stack is what lets one dialog raise another and be found again
   * underneath it. The Export dialog does exactly that: an export aimed at the
   * source folder is refused, the refusal goes on top, and answering it reveals
   * the export dialog again with its destination put right.
   *
   * The cloud asks **one question per action**: the consent dialog
   * (`src/lib/dialogs/CloudConsentDialog.svelte`), pushed by
   * `src/lib/editor/cloudflow.svelte.js#requestCloudConsent` and answered
   * through `closeModal` like any other. Nothing replaces it: `closeModal`
   * pops the stack *before* the resolved promise continues, so a flow that
   * wanted a second dialog after it would push one, since a `replaceModal`
   * there would overwrite whatever was underneath.
   *
   * A `kind` may claim a dialog of its own by registering a component in
   * `src/lib/dialogs/index.js`. Such a component receives the `spec` and
   * renders its own `Modal`, so it owns its width, its footnote and its
   * buttons.
   *
   * Everything else falls through to the generic dialog below: a title, an
   * optional line of copy (`props.bodyKey`, `props.bodyParams`) and the spec's
   * action buttons. That is the whole of a confirmation (`removeProject`) and
   * the whole of a refusal (`overwriteRefusal`, whose `dismissable: false`
   * leaves the named action as the only way out). The contract: read the spec
   * from the top of the stack, render `spec.actions` as the buttons, and
   * resolve by calling `closeModal(actionId)` - which pops the stack and fires
   * the spec's `onresolve`.
   */
  import { Modal, Button } from '../ui/index.js'
  import { app, closeModal, modalWidth } from '../state/app.svelte.js'
  import { t } from '../i18n/index.js'
  import { dialogFor } from '../dialogs/index.js'

  const spec = $derived(app.modals.length ? app.modals[app.modals.length - 1] : null)
  const Dialog = $derived(spec ? dialogFor(spec.kind) : null)
  const bodyKey = $derived(/** @type {string|undefined} */ (spec?.props?.bodyKey))
</script>

{#if spec}
  {#key spec.id}
    {#if Dialog}
      <Dialog {spec} />
    {:else}
      <!-- Read through `?.` deliberately. Every one of these is a getter over
           `spec`, and `Modal` reads them from a window keydown and from a
           bubbling backdrop click - both of which can arrive after the action
           that popped the stack and before this block has been torn down. -->
      <Modal
        title={t(spec?.titleKey ?? '')}
        width={modalWidth(spec?.kind ?? '')}
        blocking={spec?.blocking ?? false}
        onclose={spec?.dismissable ? () => closeModal(null) : undefined}
      >
        {#if bodyKey}
          <p class="body">{t(bodyKey, spec.props.bodyParams)}</p>
        {/if}

        {#snippet buttons()}
          {#each spec.actions as action (action.id)}
            <Button variant={action.variant ?? 'ghost'} onclick={() => closeModal(action.id)}>
              {t(action.labelKey)}
            </Button>
          {/each}
        {/snippet}
      </Modal>
    {/if}
  {/key}
{/if}

<style>
  .body { margin: 0 }
</style>
