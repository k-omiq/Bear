<script module>
  /**
   * The tab the sheet was last left on, for the next time it opens in this
   * window. Not saved: a sheet that reopens where it was is a convenience, and
   * one that opens on Tools after a restart is not a lost preference.
   */
  let lastTab = 'tools'
</script>

<script>
  /**
   * The shortcut sheet's body, built once and mounted in two places: inside
   * Settings (`SettingsDialog.svelte`) and on its own, because `?` pushes
   * `{kind: 'shortcuts'}` from anywhere.
   *
   * Everything it draws comes from `src/lib/shortcuts.js`, which is the only
   * place a keybinding may be declared - so adding a shortcut documents itself
   * and this component never needs editing, past naming a new group's tab.
   *
   * **Tabs and a search.** The table's groups are gathered into a few tabs by
   * what the keys are for (`TABS`), and the field above them searches every
   * tab at once: names, group names and keys alike, a single character being
   * read as a key rather than as a letter somewhere in a name. While a search
   * is typed the list is its results, under their groups, and each tab says
   * how many of them it holds; picking a tab ends the search.
   *
   * Two entries are not the same for every reader, and both are asked rather
   * than assumed:
   *
   * - the arrow keys, whose label depends on the open project's reading
   *   direction (`shortcutLabelKey`) - in RTL, ← is the *next* page;
   * - the chords, whose modifier is named `⌘` or `Ctrl` depending on the
   *   platform (`shortcutChord`).
   *
   * **The sheet is also where a shortcut is rebound.** A chord is a button:
   * click it and the next combination pressed - or an extra mouse button,
   * back or forward - becomes the binding, Escape cancels, Backspace or Delete
   * clears it. A few entries ship with no key at all, and read `Not set` until
   * the user gives them one. That is deliberately the same list rather than a
   * second editor screen - the thing a user wants to change is the row they
   * are already reading.
   *
   * While a recording is live the keydown is taken in the **capture** phase and
   * goes no further: the keyboard layer, the dialog's own Escape handler and
   * the browser must all stay out of a chord the user is naming.
   *
   * `headingLevel` exists because the sheet sits under a section heading inside
   * Settings and directly under the dialog title on its own; the group headings
   * have to fall one level below whatever is above them. The search is the
   * sheet's first control, so the dialog `?` opens puts the caret there.
   *
   * @type {{ headingLevel?: 'h3' | 'h4' }}
   */
  let { headingLevel = 'h3' } = $props()

  import { tick } from 'svelte'
  import { Button, KeyHint, Segmented, TextInput } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import {
    pointerModifiersFor,
    chordFromEvent,
    extraButtonPress,
    defaultShortcut,
    groupedShortcuts,
    isApplePlatform,
    modifierCap,
    shortcutChord,
    shortcutLabelKey,
  } from '../shortcuts.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import { BETA_SHORTCUTS } from '../editor/tools.js'
  import { getBackend } from '../api/backend.js'
  import { readingDirection } from '../state/editor.svelte.js'
  import {
    backendSettingsPatch,
    clearShortcutBinding,
    resetAllShortcutBindings,
    resetShortcutBinding,
    session,
    setCloneSourceModifier,
    setNavigation,
    setShortcutBinding,
  } from '../state/session.svelte.js'
  import { NAVIGATION_PRESETS, navigationPreset } from '../editor/navigation.js'
  import { t } from '../i18n/index.js'

  /**
   * The tabs, in order, and the table's groups each one holds. `pointer` holds
   * none: it is the bindings that are a mouse rather than a key, below.
   */
  const TABS = [
    { id: 'tools', labelKey: 'shortcuts.tab.tools', groups: ['tools', 'modes'] },
    { id: 'brush', labelKey: 'shortcuts.tab.brush', groups: ['brush'] },
    { id: 'run', labelKey: 'shortcuts.tab.run', groups: ['run'] },
    { id: 'edit', labelKey: 'shortcuts.tab.edit', groups: ['edit'] },
    { id: 'view', labelKey: 'shortcuts.tab.view', groups: ['view', 'zoom', 'navigation'] },
    { id: 'app', labelKey: 'shortcuts.tab.app', groups: ['panels', 'app'] },
    { id: 'pointer', labelKey: 'shortcuts.tab.pointer', groups: [] },
  ]

  /**
   * Whether the platform names its command modifier `⌘`. The detector is
   * `src/lib/shortcuts.js`'s, because the pointer-modifier row below asks the
   * same question and two detectors would eventually disagree.
   */
  const apple = isApplePlatform()

  /**
   * The one binding that is not a key: which modifier is held while clicking
   * the page to set where the clone stamp reads from.
   *
   * It sits in this sheet rather than in a preference row because it is a
   * *binding* - the same kind of thing every row above it is, changed for the
   * same reason - and because the sheet is where a user looking for "how do I
   * change that" will go. It is drawn as chips rather than as a recorder: a
   * recorder listens for a key, and a modifier pressed on its own is precisely
   * what `chordFromEvent` refuses (it is the first half of a chord), so
   * recording one is a gesture the recorder cannot take.
   *
   * The caps are the keys' own - `⌥` on an Apple keyboard, `Alt` elsewhere -
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

  /**
   * How a mouse moves the canvas, under the clone source: the wheel, the
   * middle button and a drag on empty canvas, plus the two presets that set
   * all three. The held pan key is a chord, so it is a row of the table
   * (Navigation, under View) and is rebound there like any other.
   *
   * A preset that no longer matches reads as Custom, which cannot itself be
   * picked: it is what the three rows below it say.
   */
  const presetOptions = $derived([
    { value: 'trackpad', label: t('shortcuts.navigation.trackpad') },
    { value: 'mouse', label: t('shortcuts.navigation.mouse') },
    { value: 'custom', label: t('shortcuts.navigation.custom'), disabled: true },
  ])
  const preset = $derived(navigationPreset(session.navigation) ?? 'custom')
  const wheelOptions = $derived([
    { value: 'scroll', label: t('shortcuts.navigation.scroll') },
    { value: 'zoom', label: t('shortcuts.navigation.zoom') },
  ])
  const panOptions = $derived([
    { value: 'on', label: t('shortcuts.navigation.pan') },
    { value: 'off', label: t('shortcuts.navigation.off') },
  ])

  /** The id whose chord is being recorded, or `null`. */
  let recording = $state(/** @type {string|null} */ (null))

  /** The pointer rows' labels, for their radio groups to point at. */
  const uid = $props.id()
  const cloneModifierLabelId = `${uid}-clone`
  const presetLabelId = `${uid}-preset`
  const wheelLabelId = `${uid}-wheel`
  const middleLabelId = `${uid}-middle`
  const emptyLabelId = `${uid}-empty`
  const panelId = `${uid}-panel`
  const searchId = `${uid}-search`

  /** The last refusal, shown under the list until the next attempt. */
  let message = $state(/** @type {{key: string, params?: Record<string, unknown>}|null} */ (null))

  let tab = $state(TABS.some((entry) => entry.id === lastTab) ? lastTab : 'tools')
  $effect(() => {
    lastTab = tab
  })

  const currentTab = $derived(TABS.find((entry) => entry.id === tab) ?? TABS[0])

  let query = $state('')

  /**
   * Rows rebound, cleared or reset during this search, which stay listed
   * until the search changes: a row that stopped matching the moment its key
   * changed would take with it the only sign the change was made.
   */
  let kept = $state(/** @type {string[]} */ ([]))

  /** @type {TextInput|undefined} */
  let searchField = $state()
  /** @type {HTMLButtonElement[]} */
  const tabButtons = $state([])

  /** @type {HTMLElement|undefined} */
  let root = $state()

  /** @param {string} value */
  function setQuery(value) {
    query = value
    kept = []
    recording = null
  }

  /**
   * Bring the top of the sheet back into view when the list under it is
   * replaced, so a short tab does not open part-way down a long one's scroll.
   * The scroller is whatever holds the sheet: the dialog's body, or Settings'.
   */
  function toTop() {
    let scroller = root?.parentElement
    while (scroller && !/(auto|scroll)/.test(getComputedStyle(scroller).overflowY)) scroller = scroller.parentElement
    if (!scroller || !root) return
    const above = root.getBoundingClientRect().top - scroller.getBoundingClientRect().top
    if (above < 0) scroller.scrollTop += above
  }

  /**
   * Text folded for matching: lower case and without accents, so `edicion`
   * finds Edición.
   *
   * @param {string} text
   */
  function fold(text) {
    return text.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase()
  }

  /**
   * The words a keycap also answers to, so `cmd`, `enter` and `left` find the
   * glyphs the sheet draws for them.
   */
  const CAP_WORDS = /** @type {Record<string, string>} */ ({
    '⌘': 'cmd command',
    '↵': 'enter return',
    '⌫': 'backspace delete',
    '←': 'left arrow',
    '→': 'right arrow',
    '↑': 'up arrow',
    '↓': 'down arrow',
    Esc: 'escape',
    Ctrl: 'control',
  })

  /**
   * Does a row answer the search? Every word typed must: a single character a
   * keyboard types as a key (ASCII) matches a keycap exactly, and any other
   * word - longer, or one character of Japanese or Korean, where that is a
   * whole word - is found anywhere in the row's name, its group's and tab's
   * names, or its keys.
   *
   * @param {string[]} words - already folded
   * @param {{label: string, keys: string[]}} row
   * @param {string[]} context - the group's and the tab's names
   */
  function matches(words, row, context) {
    const caps = row.keys.map((cap) => fold(cap))
    const text = fold([row.label, ...context, ...row.keys, ...row.keys.map((cap) => CAP_WORDS[cap] ?? '')].join(' '))
    return words.every((word) => (/^[\x21-\x7e]$/.test(word) ? caps.includes(word) : text.includes(word)))
  }

  /** @param {string} group */
  function tabLabelOf(group) {
    const entry = TABS.find((candidate) => candidate.groups.includes(group))
    return entry ? t(entry.labelKey) : ''
  }

  const words = $derived(fold(query).split(/\s+/).filter(Boolean))
  const searching = $derived(words.length > 0)

  /** Every key row of the table in force, by group, with what the sheet draws for it. */
  const sections = $derived.by(() => {
    // The bindings in force are `groupedShortcuts()`'s to know; this read is
    // what makes the sheet redraw when they change.
    void session.shortcuts
    // A beta tool's key is listed only while Settings > Beta offers the tool.
    const listed = (/** @type {{id: string}} */ shortcut) => session.betaTools || !BETA_SHORTCUTS.includes(shortcut.id)
    return groupedShortcuts().map((section) => ({
      group: section.group,
      label: t(`shortcuts.group.${section.group}`),
      rows: section.shortcuts.filter(listed).map((shortcut) => ({
        id: shortcut.id,
        label: t(shortcutLabelKey(shortcut, { readingDirection: readingDirection() })),
        keys: shortcutChord(shortcut, { apple }),
        fixed: shortcut.fixed === true,
        rebound: shortcut.rebound === true,
        unset: shortcut.keys.length === 0,
      })),
    }))
  })

  /** The pointer tab's rows, by the label the search reads. */
  const pointerRows = $derived([
    { id: 'clone', label: t('settings.cloneSource.label') },
    { id: 'preset', label: t('shortcuts.navigation.preset') },
    { id: 'wheel', label: t('shortcuts.navigation.wheel') },
    { id: 'middle', label: t('shortcuts.navigation.middle') },
    { id: 'empty', label: t('shortcuts.navigation.empty') },
  ])
  const pointerLabel = $derived(t('shortcuts.tab.pointer'))

  /** What the list shows: the open tab's groups, or every match of the search. */
  const shown = $derived.by(() => {
    const groups = searching ? sections : sections.filter((section) => currentTab.groups.includes(section.group))
    const keyed = groups
      .map((section) => ({
        ...section,
        rows: searching
          ? section.rows.filter((row) => kept.includes(row.id) || matches(words, row, [section.label, tabLabelOf(section.group)]))
          : section.rows,
      }))
      .filter((section) => section.rows.length > 0)
    const pointer = searching
      ? pointerRows.filter((row) => matches(words, { label: row.label, keys: [] }, [pointerLabel, t('shortcuts.group.pointer')])).map((row) => row.id)
      : tab === 'pointer' ? pointerRows.map((row) => row.id) : []
    return { keyed, pointer }
  })

  /** Matches per tab, while searching. */
  const counts = $derived.by(() => {
    /** @type {Record<string, number>} */
    const out = {}
    if (!searching) return out
    for (const entry of TABS) {
      out[entry.id] = entry.id === 'pointer'
        ? shown.pointer.length
        : shown.keyed.filter((section) => entry.groups.includes(section.group)).reduce((sum, section) => sum + section.rows.length, 0)
    }
    return out
  })

  /** Group headings are drawn where a list has more than one group, and for every result. */
  const headed = $derived(searching || currentTab.groups.length > 1)

  const nothingFound = $derived(searching && shown.keyed.length === 0 && shown.pointer.length === 0)

  const anyRebound = $derived(Object.keys(session.shortcuts).length > 0)

  /** @param {string} id */
  async function openTab(id) {
    tab = id
    setQuery('')
    await tick()
    toTop()
  }

  /**
   * The tab list's own keys, as a tab list has them: the arrows move and
   * select, Home and End go to the ends.
   *
   * @param {KeyboardEvent} event
   */
  async function ontabkeydown(event) {
    const at = TABS.findIndex((entry) => entry.id === tab)
    const to =
      event.key === 'ArrowRight' ? (at + 1) % TABS.length :
        event.key === 'ArrowLeft' ? (at + TABS.length - 1) % TABS.length :
          event.key === 'Home' ? 0 :
            event.key === 'End' ? TABS.length - 1 : -1
    if (to < 0) return
    event.preventDefault()
    await openTab(TABS[to].id)
    tabButtons[to]?.focus()
  }

  /**
   * Escape in a search with something typed empties it, and goes no further:
   * the dialog closes on the next one.
   *
   * @param {KeyboardEvent} event
   */
  function onsearchkeydown(event) {
    // An Escape that cancels an input method's composition is the IME's.
    if (event.key !== 'Escape' || query === '' || event.isComposing || event.keyCode === 229) return
    event.preventDefault()
    event.stopPropagation()
    setQuery('')
  }

  function clearSearch() {
    setQuery('')
    searchField?.focus()
  }

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

  /**
   * @param {import('../shortcuts.js').RebindResult} result
   * @param {string} id - the row it was asked of, kept in view in a search
   */
  function report(result, id) {
    if (result.ok) {
      message = null
      if (searching && !kept.includes(id)) kept = [...kept, id]
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
      message = null
      return
    }
    // Clearing, and the reason neither key can itself be bound: a recorder in
    // which Backspace sometimes means "clear" and sometimes means "bind
    // Backspace" has no way to tell the user which it just did.
    if (event.key === 'Backspace' || event.key === 'Delete') {
      recording = null
      report(clearShortcutBinding(id), id)
      return
    }
    take(id, chordFromEvent(event))
  }

  /**
   * An extra mouse button pressed while a row is listening is its chord, with
   * whatever modifiers are held. Any other press outside the listening chord
   * ends the recording: WebKit does not focus a clicked button, so the chord
   * never blurs there, and a click into the search would otherwise type into
   * the recorder.
   *
   * Cancelling the `pointerdown` keeps its `mousedown` from recording twice.
   * On macOS only the `mousedown` arrives (see `extraButtonPress`).
   *
   * @param {MouseEvent} event
   */
  function onpointerdown(event) {
    if (recording === null) return
    const press = extraButtonPress(event)
    if (!press) {
      if (!(event.target instanceof Element && event.target.closest('.chord.listening'))) recording = null
      return
    }
    if (!press.down) return
    event.preventDefault()
    event.stopPropagation()
    take(recording, chordFromEvent(press))
  }

  /**
   * @param {string} id
   * @param {{chord: import('../shortcuts.js').Chord|null, reasonKey: string|null}} recorded
   */
  function take(id, { chord, reasonKey }) {
    // A refusal keeps the row listening, so the "choose another" it says can
    // be done with the next press. Escape still cancels.
    if (reasonKey) {
      message = { key: reasonKey }
      return
    }
    // A modifier on its own is the first half of a chord: keep listening.
    if (!chord) return
    const result = setShortcutBinding(id, chord)
    if (result.ok) recording = null
    report(result, id)
  }

  /**
   * Focus moving anywhere but the listening chord ends the recording too: a
   * Tab onward, or a Settings section the sheet stays mounted behind.
   *
   * @param {FocusEvent} event
   */
  function onfocusin(event) {
    if (recording === null) return
    if (!(event.target instanceof Element && event.target.closest('.chord.listening'))) recording = null
  }

  function resetAll() {
    recording = null
    message = null
    resetAllShortcutBindings()
    push()
  }
</script>

<svelte:window
  onkeydowncapture={onkeydown}
  onpointerdowncapture={onpointerdown}
  onmousedowncapture={onpointerdown}
  onfocusincapture={onfocusin}
/>

<div class="sheet" bind:this={root}>
  <!-- Search and tabs stay put while the list scrolls under them. -->
  <div class="top">
    <div class="find">
      <div class="search">
        <TextInput
          bind:this={searchField}
          id={searchId}
          size="sm"
          icon="search"
          value={query}
          onchange={(value) => {
            setQuery(value)
            void tick().then(toTop)
          }}
          onkeydown={onsearchkeydown}
          placeholder={t('shortcuts.search.placeholder')}
          label={t('shortcuts.search.label')}
          autocomplete="off"
          spellcheck="false"
          aria-controls={panelId}
        />
        {#if query !== ''}
          <button type="button" class="clear" aria-label={t('shortcuts.search.clear')} title={t('shortcuts.search.clear')} onclick={clearSearch}>
            <Icon name="close" size={11} />
          </button>
        {/if}
      </div>
      <Button size="sm" disabled={!anyRebound} onclick={resetAll}>
        {t('shortcuts.rebind.resetAll')}
      </Button>
    </div>

    <!-- One tab stop for the list: the open tab. -->
    <div class="tabs" class:searching role="tablist" aria-label={t('shortcuts.tabs.label')}>
      {#each TABS as entry, index (entry.id)}
        <button
          bind:this={tabButtons[index]}
          type="button"
          role="tab"
          class="tab"
          class:on={entry.id === tab}
          class:none={searching && counts[entry.id] === 0}
          id="{uid}-tab-{entry.id}"
          aria-selected={!searching && entry.id === tab}
          aria-controls={panelId}
          tabindex={entry.id === tab ? 0 : -1}
          onclick={() => openTab(entry.id)}
          onkeydown={ontabkeydown}
        >
          {t(entry.labelKey)}
          {#if searching}<span class="count">{counts[entry.id]}</span>{/if}
        </button>
      {/each}
    </div>
  </div>

  <div
    class="panel"
    id={panelId}
    role={searching ? 'region' : 'tabpanel'}
    aria-label={searching ? t('shortcuts.search.label') : undefined}
    aria-labelledby={searching ? undefined : `${uid}-tab-${tab}`}
  >
    {#each shown.keyed as section (section.group)}
      <section class="group">
        {#if headed}
          <svelte:element this={headingLevel} class="heading">{section.label}</svelte:element>
        {/if}
        <ul class="rows">
          {#each section.rows as row (row.id)}
            <li class="row" class:rebound={row.rebound}>
              <span class="label">{row.label}</span>
              <span class="keys">
                {#if row.rebound}
                  <button
                    type="button"
                    class="revert"
                    title={t('shortcuts.rebind.reset', { name: row.label })}
                    aria-label={t('shortcuts.rebind.reset', { name: row.label })}
                    onclick={() => report(resetShortcutBinding(row.id), row.id)}
                  >
                    <Icon name="undo" size={12} />
                  </button>
                {/if}
                {#if row.fixed}
                  <span class="locked" title={t('shortcuts.rebind.fixedHint')}>
                    <KeyHint keys={row.keys} />
                  </span>
                {:else}
                  <button
                    type="button"
                    class="chord"
                    class:listening={recording === row.id}
                    class:unset={row.unset}
                    aria-label={t('shortcuts.rebind.change', { name: row.label })}
                    onclick={() => record(row.id)}
                    onblur={() => {
                      if (recording === row.id) recording = null
                    }}
                  >
                    {#if recording === row.id}
                      <span class="prompt">{t('shortcuts.rebind.listening')}</span>
                    {:else if row.unset}
                      <span class="prompt">{t('shortcuts.rebind.unbound')}</span>
                    {:else}
                      <KeyHint keys={row.keys} />
                    {/if}
                  </button>
                {/if}
              </span>
            </li>
          {/each}
        </ul>
      </section>
    {/each}

    <!-- The bindings that are a mouse rather than a key: a modifier held with
         a click, and how the wheel and the buttons move the canvas. -->
    {#if shown.pointer.length > 0}
      <section class="group">
        {#if searching}
          <svelte:element this={headingLevel} class="heading">{pointerLabel}</svelte:element>
        {/if}
        {#if shown.pointer.includes('clone')}
          <div class="pointer-row">
            <span class="label" id={cloneModifierLabelId}>{t('settings.cloneSource.label')}</span>
            <Segmented
              options={modifierOptions}
              value={session.cloneSourceModifier}
              labelledBy={cloneModifierLabelId}
              onchange={(value) => setCloneSourceModifier(value)}
            />
          </div>
        {/if}
        {#if shown.pointer.includes('preset')}
          <div class="pointer-row">
            <span class="label" id={presetLabelId}>{t('shortcuts.navigation.preset')}</span>
            <Segmented
              options={presetOptions}
              value={preset}
              labelledBy={presetLabelId}
              onchange={(value) => {
                if (value === 'trackpad' || value === 'mouse') setNavigation(NAVIGATION_PRESETS[value])
              }}
            />
          </div>
        {/if}
        {#if shown.pointer.includes('wheel')}
          <div class="pointer-row">
            <span class="label" id={wheelLabelId}>{t('shortcuts.navigation.wheel')}</span>
            <Segmented
              options={wheelOptions}
              value={session.navigation.wheel}
              labelledBy={wheelLabelId}
              onchange={(value) => setNavigation({ wheel: value === 'zoom' ? 'zoom' : 'scroll' })}
            />
          </div>
        {/if}
        {#if shown.pointer.includes('middle')}
          <div class="pointer-row">
            <span class="label" id={middleLabelId}>{t('shortcuts.navigation.middle')}</span>
            <Segmented
              options={panOptions}
              value={session.navigation.middlePan ? 'on' : 'off'}
              labelledBy={middleLabelId}
              onchange={(value) => setNavigation({ middlePan: value === 'on' })}
            />
          </div>
        {/if}
        {#if shown.pointer.includes('empty')}
          <div class="pointer-row">
            <span class="label" id={emptyLabelId}>{t('shortcuts.navigation.empty')}</span>
            <Segmented
              options={panOptions}
              value={session.navigation.emptyPan ? 'on' : 'off'}
              labelledBy={emptyLabelId}
              onchange={(value) => setNavigation({ emptyPan: value === 'on' })}
            />
          </div>
        {/if}
        {#if !searching}
          <p class="note">
            {t(
              session.navigation.wheel === 'zoom' ? 'shortcuts.navigation.hintZoom' : 'shortcuts.navigation.hintScroll',
              { accel: apple ? '⌘' : 'Ctrl', alt: modifierCap('alt', { apple }) },
            )}
          </p>
        {/if}
      </section>
    {/if}

    {#if nothingFound}
      <p class="empty">{t('shortcuts.search.empty', { query: query.trim() })}</p>
    {/if}
  </div>

  <p
    class="status"
    class:speaking={recording !== null || message !== null}
    role="status"
    aria-live="polite"
  >
    {#if message}
      {t(message.key, message.params)}
    {:else if recording !== null}
      {t('shortcuts.rebind.hint')}
    {/if}
  </p>
</div>

<style>
  /* The fill of whatever scrolls the sheet - the dialog's by default, the
     screen's inside Settings (`--sheet-fill`) - so rows scrolling under the
     bar do not show through it. */
  .top {
    position: sticky;
    top: 0;
    z-index: 1;
    margin-bottom: 6px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--line);
    background: var(--sheet-fill, var(--surface));
  }

  .find {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .search {
    position: relative;
    flex: 1;
    min-width: 0;
  }
  /* Room for the clear button. */
  .search :global(input) { padding-right: 18px }
  .clear {
    position: absolute;
    top: 50%;
    right: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: none;
    border-radius: var(--r-xs);
    background: none;
    color: var(--t3);
    transform: translateY(-50%);
    cursor: pointer;
  }
  .clear:hover { color: var(--text) }
  .clear:focus-visible { outline-offset: -1px }

  /* Chips, the shape of Settings' own tabs laid on their side. Weight never
     changes with selection, so a label never shifts under the pointer. */
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    margin-top: 8px;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 9px;
    border: none;
    border-radius: var(--r-chip);
    background: none;
    color: var(--t2);
    font: inherit;
    font-size: 11.5px;
    white-space: nowrap;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease);
  }
  .tab:hover { background: var(--accent-soft); color: var(--text) }
  .tab.on { background: var(--accent-soft); color: var(--text) }
  .tab:focus-visible { outline-offset: -2px }
  /* While searching no tab is open: the list is the results of all of them. */
  .tabs.searching .tab.on:not(:hover) { background: none; color: var(--t2) }
  .tab.none { color: var(--t3) }
  .count {
    font-size: 10.5px;
    color: var(--t3);
    font-variant-numeric: tabular-nums;
  }

  .panel { padding-top: 2px }

  .group + .group { margin-top: 12px }

  .heading {
    margin: 0 0 3px;
    padding: 0 6px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: .12em;
    text-transform: uppercase;
    color: var(--t3);
  }

  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  /* One row per shortcut, the name left and its keys right. The fill is the
     row answering the pointer, so it is clear which keys belong to which
     name across the gap. */
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 26px;
    padding: 0 3px 0 6px;
    border-radius: var(--r-sm);
    /* Room for the sticky search and tabs above and the status below, so a
       row the focus scrolls to is not left under either. */
    scroll-margin: 84px 0 36px;
  }
  .row:hover { background: var(--accent-soft) }

  .label {
    min-width: 0;
    font-size: 11.5px;
    line-height: 1.35;
    color: var(--t2);
  }
  .row:hover .label { color: var(--text) }

  .keys {
    display: flex;
    flex: none;
    align-items: center;
    gap: 2px;
  }

  /* The chord is a control, and reads as one only on approach: at rest it is
     the keycaps and nothing else, so a sheet that is being *read* is the sheet
     it always was. */
  .chord {
    display: inline-flex;
    align-items: center;
    min-height: 21px;
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
  /* No key yet: a dashed slot, which is what it is - a place to put one. */
  .chord.unset { padding: 1px 6px; border-style: dashed; border-color: var(--line2) }
  .chord.unset:hover { border-color: var(--tintline) }
  .chord.unset.listening, .chord.unset:focus-visible { border-style: solid; border-color: var(--accent) }

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
    width: 20px;
    height: 20px;
    padding: 0;
    border: none;
    border-radius: var(--r-xs);
    background: none;
    color: var(--t3);
    cursor: pointer;
  }
  .revert:hover { color: var(--text) }
  .revert:focus-visible {
    outline: none;
    color: var(--accent);
  }

  .locked { display: inline-flex; padding: 1px 4px }

  /* The pointer rows: the key rows' shape, with chips for keys. */
  .pointer-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 30px;
    padding: 0 3px 0 6px;
    scroll-margin: 84px 0 36px;
  }

  .note {
    margin: 8px 0 0;
    padding: 0 6px;
    font-size: 10.5px;
    line-height: 1.4;
    color: var(--t3);
  }

  .empty {
    margin: 24px 0 0;
    font-size: 11.5px;
    color: var(--t3);
    text-align: center;
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
    padding: 6px;
    background: var(--sheet-fill, var(--surface));
  }
</style>
