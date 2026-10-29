<script>
  /**
   * Settings > Beta: tools that work but are still being tuned, off until
   * the user asks for them.
   *
   * One row today: the Patch tool (whose Content-Aware mode is PatchMatch)
   * and the Screentone fill engine, together, because they are one piece of
   * work on dot-screened pages. Off, the rail leaves the Patch tool out and
   * its key does nothing, the Screentone fill leaves every engine picker, and
   * the editor moves off the Patch tool if it is out
   * (`editor/tools.js#BETA_TOOLS`, `model/masks.js#BETA_ENGINES`). Layers they
   * already made stay on their pages, drawn and named as before.
   *
   * The choice is this window's, saved with the session at once, and written
   * to the settings file beside the rest (`session.svelte.js#backendSettingsPatch`),
   * which keeps a key it does not read as it came. A failed write changes
   * nothing here: the session is where the choice lives.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { untrack } from 'svelte'
  import { Field } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import { session, setBetaTools } from '../state/session.svelte.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const uid = $props.id()
  const toolsId = `${uid}-tools`

  /** @param {boolean} enabled */
  async function change(enabled) {
    setBetaTools(enabled)
    try {
      await writeSettingsSerialized(backend, { betaTools: enabled })
    } catch (error) {
      console.error('the beta setting could not be written to the settings file', error)
    }
  }
</script>

<p class="intro">{t('settings.beta.description')}</p>

<div data-settings-anchor="beta-tools">
  <Field label={t('settings.beta.tools')} description={t('settings.beta.toolsHint')} layout="row" controlId={toolsId}>
    {#snippet children({ descriptionId })}
      <span class="control">
        <span class="tag" aria-hidden="true">{t('settings.beta.tag')}</span>
        <input
          id={toolsId}
          class="check"
          type="checkbox"
          aria-describedby={descriptionId}
          checked={session.betaTools}
          onchange={(event) => void change(event.currentTarget.checked)}
        />
      </span>
    {/snippet}
  </Field>
</div>

<style>
  .intro {
    margin: 0 0 var(--s-2);
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--t3);
  }

  .control {
    display: inline-flex;
    align-items: center;
    gap: var(--s-3);
  }

  /* The small mark that says the row is a beta: outlined, so it reads as a
     label and not as a button. */
  .tag {
    padding: 1px 7px;
    border: 1px solid var(--accent);
    border-radius: var(--r-pill);
    color: var(--text);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: .06em;
    line-height: 1.5;
    text-transform: uppercase;
  }

  .check {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
    cursor: pointer;
  }
</style>
