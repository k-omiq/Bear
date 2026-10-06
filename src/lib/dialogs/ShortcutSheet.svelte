<script>
  /**
   * The shortcut sheet's body, built once and mounted in two places: inside
   * Settings (`SettingsDialog.svelte`) and on its own, because `?` pushes
   * `{kind: 'shortcuts'}` from anywhere.
   *
   * Everything it draws comes from `src/lib/shortcuts.js`, which is the only
   * place a keybinding may be declared - so adding a shortcut documents itself
   * and this component never needs editing.
   *
   * Two entries are not the same for every reader, and both are asked rather
   * than assumed:
   *
   * - the arrow keys, whose label depends on the open project's reading
   *   direction (`shortcutLabelKey`) - in RTL, ← is the *next* page;
   * - `app.openProject`, the one chord in the *default* table, whose modifier
   *   is named `⌘` or `Ctrl` depending on the platform (`shortcutChord`).
   *
   * **The sheet is also where a shortcut is rebound.** A chord is a button:
   * click it and the next combination pressed becomes the binding, Escape
   * cancels, Backspace or Delete clears it. That is deliberately the same list
   * rather than a second editor screen - the thing a user wants to change is
   * the row they are already reading, and a separate editor would have to
   * repeat every label and every chord to be usable at all.
   *
   * While a recording is live the keydown is taken in the **capture** phase and
   * goes no further: the keyboard layer, the dialog's own Escape handler and
   * the browser must all stay out of a chord the user is naming.
   *
   * `headingLevel` exists because the sheet sits under a section heading inside
   * Settings and directly under the dialog title on its own; the group headings
   * have to fall one level below whatever is above them.
   *
   * @type {{ headingLevel?: 'h3' | 'h4' }}
   */
  let { headingLevel = 'h3' } = $props()

  import { Button, KeyHint, Segmented } from '../ui/index.js'
  import {
    pointerModifiersFor,
    chordFromEvent,
    defaultShortcut,
    groupedShortcuts,
    isApplePlatform,
    modifierCap,
    shortcutChord,
    shortcutLabelKey,
  } from '../shortcuts.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import { getBackend } from '../api/backend.js'
  import { readingDirection } from '../state/editor.svelte.js'
  import {
    backendSettingsPatch,
    clearShortcutBinding,
    resetAllShortcutBindings,
    resetShortcutBinding,
    session,
    setCloneSourceModifier,
    setShortcutBinding,
  } from '../state/session.svelte.js'
  import { t } from '../i18n/index.js'

  /**
   * Whether the platform names its command modifier `⌘`. The detector is
   * `src/lib/shortcuts.js`'s, because the pointer-modifier row below asks the
   * same question and two detectors would eventually disagree.
   */
  const apple = isApplePlatform()

  /**
   * The one binding that is not a key: which modifier is held while clicking
   * the page to set where Clone / heal reads from.
   *
   * It sits in this sheet rather than in a preference row because it is a
   * *binding* - the same kind of thing every row above it is, changed for the
   * same reason - and because the sheet is where a user looking for "how do I
   * change that" will go. It is drawn as chips rather than as a recorder: a
   * recorder listens for a key, and a modifier pressed on its own is precisely
   * what `chordFromEvent` refuses (it is the first half of a chord), so
   * recording one is a gesture the recorder cannot take.
   *
   * The caps are the keys' own - `⌥` on an Apple keyboard, `Alt` elsewhere  - 
   * and the spelt-out names ride along as `title` and as the accessible name,
   * because a radio whose whole label is `⌥` reads as nothing at all.
   */
  const MODIFIER_NAME = {
    alt: 'settings.cloneSource.name.alt',
    meta: 'settings.cloneSource.name.meta',
    control: 'settings.cloneSource.name.control',
    shift: 'settings.cloneSource.name.shift',
  }

  const modifierOptions = $derived(
    pointerModifiersFor({ apple }).map((id) => ({
      value: id,
      label: modifierCap(id, { apple }),
      title: t(MODIFIER_NAME[id]),
      ariaLabel: t(MODIFIER_NAME[id]),
    })),
  )

  /** The id whose chord is being recorded, or `null`. */
  let recording = $state(/** @type {string|null} */ (null))

  /** The pointer row's label, for the radio group to point at. */
  const cloneModifierLabelId = $props.id()

  /** The last refusal, shown under the list until the next attempt. */
  let message = $state(/** @type {{key: string, params?: Record<string, unknown>}|null} */ (null))

  const sections = $derived.by(() => {
    // The bindings in force are `groupedShortcuts()`'s to know; this read is
    // what makes the sheet redraw when they change.
    void session.shortcuts
    return groupedShortcuts().map((section) => ({
      group: section.group,
      headingKey: `shortcuts.group.${section.group}`,
      rows: section.shortcuts.map((shortcut) => ({
        id: shortcut.id,
        label: t(shortcutLabelKey(shortcut, { readingDirection: readingDirection() })),
        keys: shortcutChord(shortcut, { apple }),
        fixed: shortcut.fixed === true,
        rebound: shortcut.rebound === true,
        unbound: shortcut.unbound === true,
      })),
    }))
  })

  const anyRebound = $derived(Object.keys(session.shortcuts).length > 0)

  /**
   * The backend half of the write. The session half is done by the setters;
   * this mirrors what `SettingsDialog.svelte` does after every preference
   * change, and is done here rather than there so the sheet behaves the same
   * mounted on its own as it does inside Settings.
   */
  function push() {
    // A settings store that will not take the write is not a reason to refuse
    // the rebinding: the session already has it, and the next successful write
    // carries it down.
    writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
      .catch(() => {})
  }

  /**
   * @param {string} id
   * @returns {string} the i18n key naming the shortcut, for a refusal to quote
   */
  function labelKeyOf(id) {
    const entry = defaultShortcut(id)
    return entry ? shortcutLabelKey(entry, { readingDirection: readingDirection() }) : ''
  }

  /** @param {import('../shortcuts.js').RebindResult} result */
  function report(result) {
    if (result.ok) {
      message = null
      push()
      return
    }
    message =
      result.conflictId === undefined
        ? { key: result.reasonKey }
        : { key: result.reasonKey, params: { nameKey: labelKeyOf(result.conflictId) } }
  }

  /** @param {string} id */
  function record(id) {
    if (recording === id) {
      recording = null
      return
    }
    message = null
    recording = id
  }

  /**
   * The recorder. Every key goes to it while a row is listening, including the
   * ones the app would otherwise act on.
   *
   * @param {KeyboardEvent} event
   */
  function onkeydown(event) {
    if (recording === null) return
    event.preventDefault()
    event.stopPropagation()
    const id = recording
    if (event.key === 'Escape') {
      recording = null
      return
    }
    // Clearing, and the reason neither key can itself be bound: a recorder in
    // which Backspace sometimes means "clear" and sometimes means "bind
    // Backspace" has no way to tell the user which it just did.
    if (event.key === 'Backspace' || event.key === 'Delete') {
      recording = null
      report(clearShortcutBinding(id))
      return
    }
    const { chord, reasonKey } = chordFromEvent(event)
    if (reasonKey) {
      recording = null
      message = { key: reasonKey }
      return
    }
    // A modifier on its own is the first half of a chord: keep listening.
    if (!chord) return
    recording = null
    report(setShortcutBinding(id, chord))
  }

  function resetAll() {
    recording = null
    message = null
    resetAllShortcutBindings()
    push()
  }
</script>

<svelte:window onkeydowncapture={onkeydown} />

<div class="sheet">
  {#each sections as section (section.group)}
    <section class="group">
      <svelte:element this={headingLevel} class="heading">
        {t(section.headingKey)}
      </svelte:element>
      <dl class="rows">
        {#each section.rows as row (row.id)}
          <dt class="label">{row.label}</dt>
          <dd class="keys">
            {#if row.fixed}
              <span class="locked" title={t('shortcuts.rebind.fixedHint')}>
                <KeyHint keys={row.keys} />
              </span>
            {:else}
              <button
                type="button"
                class="chord"
                class:listening={recording === row.id}
                aria-label={t('shortcuts.rebind.change', { name: row.label })}
                onclick={() => record(row.id)}
                onblur={() => {
                  if (recording === row.id) recording = null
                }}
              >
                {#if recording === row.id}
                  <span class="prompt">{t('shortcuts.rebind.listening')}</span>
                {:else if row.unbound}
                  <span class="prompt">{t('shortcuts.rebind.unbound')}</span>
                {:else}
                  <KeyHint keys={row.keys} />
                {/if}
              </button>
              {#if row.rebound}
                <button
                  type="button"
                  class="revert"
                  title={t('shortcuts.rebind.reset', { name: row.label })}
                  aria-label={t('shortcuts.rebind.reset', { name: row.label })}
                  onclick={() => report(resetShortcutBinding(row.id))}
                >
                  ↺
                </button>
              {/if}
            {/if}
          </dd>
        {/each}
      </dl>
    </section>
  {/each}

  <!-- The one binding that is a modifier rather than a chord. Last, under the
       key groups, because it is the exception and reads as one. -->
  <section class="group">
    <svelte:element this={headingLevel} class="heading">
      {t('shortcuts.group.pointer')}
    </svelte:element>
    <div class="pointer-row">
      <span class="label" id={cloneModifierLabelId}>{t('settings.cloneSource.label')}</span>
      <Segmented
        options={modifierOptions}
        value={session.cloneSourceModifier}
        labelledBy={cloneModifierLabelId}
        onchange={(value) => setCloneSourceModifier(value)}
      />
    </div>
  </section>

  <p
    class="status"
    class:speaking={recording !== null || message !== null}
    role="status"
    aria-live="polite"
  >
    {#if recording !== null}
      {t('shortcuts.rebind.hint')}
    {:else if message}
      {t(message.key, message.params)}
    {/if}
  </p>

  <div class="foot">
    <Button size="sm" disabled={!anyRebound} onclick={resetAll}>
      {t('shortcuts.rebind.resetAll')}
    </Button>
  </div>
</div>

<style>
  .group + .group { margin-top: 12px }

  .heading {
    margin: 0 0 5px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: .12em;
    text-transform: uppercase;
    color: var(--t3);
  }

  /* Two label/keys pairs per row, as the design file lays the sheet out. It
     collapses to one pair when the dialog is narrower than the two fit. */
  .rows {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto;
    align-items: center;
    column-gap: 12px;
    row-gap: 3px;
    margin: 0;
  }
  @media (max-width: 520px) {
    .rows { grid-template-columns: 1fr auto }
  }

  .label {
    min-width: 0;
    font-size: 11px;
    color: var(--t2);
  }
  .keys {
    display: flex;
    align-items: center;
    gap: 4px;
    margin: 0;
    justify-self: end;
  }

  /* The chord is a control, and reads as one only on approach: at rest it is
     the keycaps and nothing else, so a sheet that is being *read* is the sheet
     it always was. */
  .chord {
    display: inline-flex;
    align-items: center;
    padding: 1px 3px;
    border: 1px solid transparent;
    border-radius: var(--r-xs);
    background: none;
    font: inherit;
    color: inherit;
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease);
  }
  .chord:hover { border-color: var(--line2) }
  .chord:focus-visible {
    outline: none;
    border-color: var(--accent);
  }
  .chord.listening { border-color: var(--accent) }

  .prompt {
    font-size: 10.5px;
    color: var(--t3);
    white-space: nowrap;
  }
  .chord.listening .prompt { color: var(--accent) }

  .revert {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    padding: 0;
    border: none;
    border-radius: var(--r-xs);
    background: none;
    color: var(--t3);
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
  }
  .revert:hover { color: var(--text) }
  .revert:focus-visible {
    outline: none;
    color: var(--accent);
  }

  .locked { display: inline-flex; padding: 1px 3px }

  /* The pointer row. Label left, chips right - the shape of the `dl` rows
     above it without the grid, because one row cannot be a column of two. */
  .pointer-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 23px;
  }

  /* Sticky to the foot of the dialog's own scroller while it has anything to
     say. The list is long enough that a refusal at its end would be off-screen
     for a row near its start - and a refusal nobody sees reads as a rebinding
     that silently did nothing. It carries the dialog's own fill so the rows it
     covers do not show through, and takes no height at all when it is silent. */
  .status {
    margin: 0;
    font-size: 10.5px;
    line-height: 1.4;
    color: var(--accent);
  }
  .status.speaking {
    position: sticky;
    bottom: 0;
    margin-top: 10px;
    padding: 4px 0;
    background: var(--surface);
  }

  .foot {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-top: 14px;
  }
</style>
