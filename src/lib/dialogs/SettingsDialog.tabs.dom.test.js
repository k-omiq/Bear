/**
 * The Settings screen's sidebar, and the one binding in it that is not a key.
 *
 * Settings is a full-window `Screen` with a vertical tab list down the left.
 * What is asserted here is what makes that list a real WAI-ARIA tab list
 * rather than seven buttons that look like one: one tab stop, Up / Down /
 * Home / End, selection that follows focus, every tab wired to a mounted
 * panel, and a heading outline that survives (h1 screen, h2 section, h3
 * below). Also here: the ids older callers still open it with, the anchor a
 * deep link lands on, the one place the community invitation lives, and the
 * theme picker on General.
 *
 * The second half of the file is the pointer modifier - `session
 * .cloneSourceModifier`, the setting that exists because *Alt* is not a key on
 * an Apple keyboard. What is asserted is the round trip the user actually
 * makes: press a chip in Settings › Shortcuts, and the tool window's own hint
 * says the new modifier. The arithmetic underneath it - which modifier a
 * pointer event satisfies - is pure and lives in `shortcuts.test.js`.
 *
 * The seam is stubbed the way the two files beside this one stub it: the
 * screen opens several calls on mount and none of them is what this file is
 * about, so each answers the emptiest true thing.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { editor, pageByArrow, readingDirection } from '../state/editor.svelte.js'
import { t } from '../i18n/index.js'
import { DEFAULT_POINTER_MODIFIER, isApplePlatform, modifierCap } from '../shortcuts.js'
import { session, setCloneSourceModifier, setTheme } from '../state/session.svelte.js'
import { closeModal } from '../state/app.svelte.js'
import SettingsDialog from './SettingsDialog.svelte'

vi.mock('../state/app.svelte.js', async (importOriginal) => ({
  ...(await importOriginal()),
  closeModal: vi.fn(),
}))

/** The spec `pushModal({kind: 'settings'})` would have handed the screen. */
const SPEC = {
  id: 'modal-1',
  kind: 'settings',
  titleKey: 'modal.title.settings',
  props: {},
  actions: [{ id: 'close', labelKey: 'shell.action.close' }],
  blocking: false,
  dismissable: true,
  onresolve: null,
}

/** The tabs, in the order the list offers them, with the key each is named by. */
const LABELS = {
  general: 'settings.section.general',
  models: 'settings.section.models',
  cloud: 'settings.section.cloud',
  denoise: 'settings.section.denoise',
  performance: 'settings.section.performance',
  shortcuts: 'settings.section.shortcuts',
  about: 'settings.section.about',
}
const TABS = Object.keys(LABELS)

/**
 * The panels whose tail holds nothing focusable, so the scroller is the stop.
 * Performance is the one here because this file lists no per-model backends,
 * so the panel ends in prose. Models ends in the token's buttons and About in
 * the community invitation's link.
 */
const FOCUSABLE_SCROLLERS = new Set(['performance'])

/** @param {string} id */
const tabName = (id) => t(LABELS[id])

/** @type {ReturnType<typeof vi.fn>} */
let writeSettings

beforeEach(() => {
  writeSettings = vi.fn(async () => ({}))
  setBackend(
    /** @type {any} */ ({
      listModels: vi.fn(async () => null),
      listAccelerators: vi.fn(async () => ({ providers: [], models: [] })),
      listSidecarModels: vi.fn(async () => []),
      sidecarAvailable: vi.fn(async () => ({ available: false, reasonKey: null })),
      about: vi.fn(async () => ({ appVersion: '0.0.0-test', facts: [] })),
      subscribe: vi.fn(() => () => {}),
      writeSettings: (/** @type {any} */ patch) => writeSettings(patch),
      readInferenceConfig: vi.fn(async () => ({ schemaVersion: 1, selectedTarget: { type: 'local' }, beamProfiles: {}, modalProfiles: {} })),
    }),
  )
})

afterEach(() => {
  cleanup()
  editor.project = null
  editor.chapter = null
  session.readingDirection = 'rtl'
  setBackend(null)
  // The session is a module singleton, so a value this file chose would
  // otherwise be the value the next file mounts with.
  setCloneSourceModifier(DEFAULT_POINTER_MODIFIER)
  vi.clearAllMocks()
})

/** @param {Record<string, unknown>} [props] */
function open(props = {}) {
  const rendered = render(SettingsDialog, { props: { spec: { ...SPEC, props } } })
  /** @param {string} id */
  const tab = (id) => rendered.getByRole('tab', { name: tabName(id) })
  /** @param {string} id */
  const panel = (id) =>
    /** @type {HTMLElement} */ (
      rendered.container.querySelector(`#${tab(id).getAttribute('aria-controls')}`)
    )
  const selected = () => TABS.find((id) => tab(id).getAttribute('aria-selected') === 'true')
  return { rendered, tab, panel, selected }
}

describe('the tab list', () => {
  it('changes existing RTL projects to LTR through Settings and saves the preference', async () => {
    editor.project = { id: 'p1', mode: 'single', readingDirection: 'rtl' }
    editor.chapter = { id: 'c1', review: [], pages: [
      { id: 'page1', index: 0, regions: [], resident: true },
      { id: 'page2', index: 1, regions: [], resident: true },
    ] }
    editor.pageIndex = 0
    session.readingDirection = 'rtl'
    const { rendered } = open()
    await fireEvent.click(rendered.getByRole('radio', { name: t('settings.direction.ltr') }))
    expect(readingDirection()).toBe('ltr')
    await waitFor(() => expect(writeSettings).toHaveBeenCalledWith(expect.objectContaining({ readingDirection: 'ltr' })))
    pageByArrow('right')
    expect(editor.pageIndex).toBe(1)
    await fireEvent.click(rendered.getByRole('radio', { name: t('settings.direction.rtl') }))
    expect(readingDirection()).toBe('rtl')
    pageByArrow('right')
    expect(editor.pageIndex).toBe(0)
  })
  it('opens on General', () => {
    const { rendered, tab, panel } = open()

    expect(tab('general').getAttribute('aria-selected')).toBe('true')
    expect(panel('general').hasAttribute('hidden')).toBe(false)
    expect(panel('general').textContent).toContain(t('settings.theme.label'))
    expect(panel('general').textContent).toContain(t('settings.language.label'))

    for (const id of TABS.slice(1)) {
      expect(tab(id).getAttribute('aria-selected')).toBe('false')
      expect(panel(id).hasAttribute('hidden')).toBe(true)
    }
    const list = rendered.getByRole('tablist')
    expect(list.getAttribute('aria-label')).toBe(t('settings.tabs.label'))
    expect(list.getAttribute('aria-orientation')).toBe('vertical')
  })

  it('offers the sections in order, each with its label on screen', () => {
    const { rendered } = open()
    const names = rendered.getAllByRole('tab').map((tab) => tab.textContent?.trim())
    expect(names).toEqual(TABS.map(tabName))
  })

  it('is a full-window screen, not a modal', () => {
    const { rendered } = open()
    const screen = rendered.getByRole('dialog')
    expect(screen.classList.contains('screen')).toBe(true)
    expect(screen.getAttribute('aria-label')).toBe(t('modal.title.settings'))
  })

  it('wires every tab to a panel that exists and names it back', () => {
    const { tab, panel } = open()
    for (const id of TABS) {
      const controls = tab(id).getAttribute('aria-controls')
      expect(controls).toBeTruthy()
      // Mounted, not conditional: `aria-controls` pointing at nothing is a
      // promise the markup does not keep.
      expect(panel(id)).not.toBeNull()
      expect(panel(id).getAttribute('role')).toBe('tabpanel')
      expect(panel(id).getAttribute('aria-labelledby')).toBe(tab(id).id)
    }
  })

  it('holds exactly one tab stop, and it is the selected tab', async () => {
    const { tab } = open()
    const stops = () => TABS.filter((id) => tab(id).getAttribute('tabindex') === '0')

    expect(stops()).toEqual(['general'])
    await fireEvent.click(tab('shortcuts'))
    expect(stops()).toEqual(['shortcuts'])
  })

  it('moves and selects on Up and Down, wrapping at both ends', async () => {
    const { tab, panel, selected } = open()

    await fireEvent.keyDown(tab('general'), { key: 'ArrowDown' })
    expect(selected()).toBe('models')
    expect(document.activeElement).toBe(tab('models'))
    expect(panel('models').hasAttribute('hidden')).toBe(false)
    expect(panel('general').hasAttribute('hidden')).toBe(true)

    await fireEvent.keyDown(tab('models'), { key: 'ArrowUp' })
    expect(selected()).toBe('general')

    // Up from the first is the last, and down from the last is the first.
    await fireEvent.keyDown(tab('general'), { key: 'ArrowUp' })
    expect(selected()).toBe('about')
    await fireEvent.keyDown(tab('about'), { key: 'ArrowDown' })
    expect(selected()).toBe('general')
  })

  it('leaves Left and Right alone: the list is vertical', async () => {
    const { tab, selected } = open()
    for (const key of ['ArrowLeft', 'ArrowRight']) {
      const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })
      tab('general').dispatchEvent(event)
      expect(event.defaultPrevented).toBe(false)
    }
    expect(selected()).toBe('general')
  })

  it('goes to the ends on Home and End', async () => {
    const { tab, panel } = open()

    await fireEvent.keyDown(tab('general'), { key: 'End' })
    expect(tab('about').getAttribute('aria-selected')).toBe('true')
    expect(panel('about').hasAttribute('hidden')).toBe(false)
    expect(document.activeElement).toBe(tab('about'))

    await fireEvent.keyDown(tab('about'), { key: 'Home' })
    expect(tab('general').getAttribute('aria-selected')).toBe('true')
    expect(panel('about').hasAttribute('hidden')).toBe(true)
    expect(document.activeElement).toBe(tab('general'))
  })

  it('stops the keys it answers, so the editor underneath does not page', async () => {
    const { tab } = open()
    const event = new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true })
    const underneath = vi.fn()
    window.addEventListener('keydown', underneath)
    tab('general').dispatchEvent(event)
    window.removeEventListener('keydown', underneath)
    expect(event.defaultPrevented).toBe(true)
    expect(underneath).not.toHaveBeenCalled()
  })

  it('leaves a key it does not answer to whatever is under the screen', async () => {
    const { tab } = open()
    const event = new KeyboardEvent('keydown', { key: 'k', bubbles: true, cancelable: true })
    tab('general').dispatchEvent(event)
    expect(event.defaultPrevented).toBe(false)
    expect(tab('general').getAttribute('aria-selected')).toBe('true')
  })

  it('keeps a real heading outline: h1 title, h2 section, h3 below', () => {
    const { rendered, panel } = open()

    const titles = rendered.container.querySelectorAll('h1')
    expect(titles).toHaveLength(1)
    expect(titles[0].textContent?.trim()).toBe(t('modal.title.settings'))
    for (const id of TABS) {
      const headings = panel(id).querySelectorAll('h2')
      expect(headings).toHaveLength(1)
      expect(headings[0].textContent?.trim()).toBe(tabName(id))
    }
    // The sheet's own group headings fall one level below the section's.
    expect(panel('shortcuts').querySelectorAll('h3').length).toBeGreaterThan(0)
  })

  it('closes on the back control, which is named Done', async () => {
    const { rendered } = open()
    const back = rendered.getByRole('button', { name: t('shell.action.done') })
    await fireEvent.click(back)
    expect(closeModal).toHaveBeenCalledWith('done')
  })

  it('closes on Escape', async () => {
    open()
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(closeModal).toHaveBeenCalledWith(null)
  })

  it('has no cloud pointer in General: Cloud is a section of its own', () => {
    const { panel } = open()
    expect(panel('general').querySelector('button')?.textContent ?? '').not.toContain(t('settings.section.cloud'))
    expect(panel('general').textContent).not.toContain(t('settings.inference.permission.label'))
  })

  it('offers the community invitation in About, and nowhere else', () => {
    const { rendered, panel } = open()
    // Every panel is mounted, so the hidden ones count too.
    const invites = rendered.getAllByRole('link', { name: t('onboarding.community.join'), hidden: true })
    expect(invites).toHaveLength(1)
    expect(panel('about').contains(invites[0])).toBe(true)
  })

  it('makes a panel focusable when, and only when, its tail is not', () => {
    const { panel } = open()
    for (const id of TABS) {
      expect([id, panel(id).getAttribute('tabindex')]).toEqual([
        id,
        FOCUSABLE_SCROLLERS.has(id) ? '0' : null,
      ])
    }
  })
})

describe('the section a caller asks for', () => {
  it.each([
    ['models', 'models'],
    ['cloud', 'cloud'],
    ['performance', 'performance'],
    // The ids from before the restructure still land where their rows went.
    ['detection', 'models'],
    ['cleaning', 'models'],
    ['inference', 'cloud'],
    ['acceleration', 'performance'],
    // Anything else is General.
    ['billing', 'general'],
    [undefined, 'general'],
  ])('opens %s on %s', (requested, expected) => {
    const { selected, panel } = open(requested === undefined ? {} : { tab: requested })
    expect(selected()).toBe(expected)
    expect(panel(expected).hasAttribute('hidden')).toBe(false)
  })

  it.each([
    ['detection', 'pipelines.detection'],
    ['cleaning', 'pipelines.cleaning'],
  ])('lands on the %s group inside Models, with focus on its heading', async (requested, heading) => {
    const { rendered, panel } = open({ tab: requested })
    const target = rendered.getByRole('heading', { name: t(heading) })
    expect(panel('models').contains(target)).toBe(true)
    await waitFor(() => expect(document.activeElement).toBe(target))
  })

  it('lands on the download token from its anchor', async () => {
    const { rendered } = open({ tab: 'models', anchor: 'access' })
    await waitFor(() =>
      expect(document.activeElement).toBe(rendered.getByRole('heading', { name: t('settings.models.access.heading') })),
    )
  })

  it('opens the collapsed language filtering when a link asks for one of its models', async () => {
    const { rendered } = open({ tab: 'models', anchor: 'scriptGate' })
    const summary = rendered.getByRole('button', { name: new RegExp(t('settings.detection.capability.japanese')) })
    await waitFor(() => expect(summary.getAttribute('aria-expanded')).toBe('true'))
  })

  it('drops an anchor the section does not hold, and still opens the section', () => {
    const { selected } = open({ tab: 'cloud', anchor: 'samTs' })
    expect(selected()).toBe('cloud')
  })
})

describe('the theme picker', () => {
  afterEach(() => setTheme('system'))

  it('offers every theme, and marks the one in force', () => {
    const { rendered } = open()
    const group = rendered.getByRole('radiogroup', { name: t('settings.theme.label') })
    const names = [...group.querySelectorAll('[role="radio"]')].map((radio) => radio.textContent?.trim())
    expect(names).toEqual(['system', 'light', 'dark', 'sakura', 'jade', 'ocean'].map((id) => t(`settings.theme.${id}`)))
    expect(rendered.getByRole('radio', { name: t('settings.theme.system') }).getAttribute('aria-checked')).toBe('true')
  })

  it('writes the theme to the session and the backend on a press', async () => {
    const { rendered } = open()
    await fireEvent.click(rendered.getByRole('radio', { name: t('settings.theme.sakura') }))

    expect(session.theme).toBe('sakura')
    expect(rendered.getByRole('radio', { name: t('settings.theme.sakura') }).getAttribute('aria-checked')).toBe('true')
    await waitFor(() => expect(writeSettings).toHaveBeenCalledWith(expect.objectContaining({ theme: 'sakura' })))
  })

  it('moves and selects on the arrows', async () => {
    const { rendered } = open()
    await fireEvent.keyDown(rendered.getByRole('radio', { name: t('settings.theme.system') }), { key: 'ArrowRight' })
    expect(session.theme).toBe('light')
  })
})

describe('the modifier that picks Clone / heal’s source', () => {
  /**
   * The four chips, found by the spelt-out names rather than by their keycaps
   * - `⌥` is not a word, and a role query resolves the name the same way a
   * screen reader would. It is a *role* query, so it will not find a chip in a
   * panel that is still `hidden`: the press has to have happened.
   */
  function chip(rendered, id) {
    return rendered.getByRole('radio', { name: t(`settings.cloneSource.name.${id}`) })
  }

  it('starts on the modifier the gesture has always used', async () => {
    const { rendered, tab, panel } = open()
    await fireEvent.click(tab('shortcuts'))

    expect(session.cloneSourceModifier).toBe('alt')
    expect(chip(rendered, 'alt').getAttribute('aria-checked')).toBe('true')
    // And it is drawn as the key the platform prints, not as the word `Alt`.
    expect(panel('shortcuts').textContent).toContain(
      modifierCap('alt', { apple: isApplePlatform() }),
    )
  })

  it('is changed by a press, and the tool window’s hint says the new one', async () => {
    const { rendered, tab } = open()
    await fireEvent.click(tab('shortcuts'))

    // The string the tool window renders, before and after. It names the
    // modifier through a context param, because `ToolWindow` calls
    // `t(spec.hintKey)` and has no preference to pass.
    const apple = isApplePlatform()
    expect(t('tools.hint.cloneHeal')).toContain(modifierCap('alt', { apple }))

    await fireEvent.click(chip(rendered, 'meta'))

    expect(session.cloneSourceModifier).toBe('meta')
    expect(chip(rendered, 'meta').getAttribute('aria-checked')).toBe('true')
    expect(chip(rendered, 'alt').getAttribute('aria-checked')).toBe('false')
    expect(t('tools.hint.cloneHeal')).toContain(modifierCap('meta', { apple }))
    expect(t('tools.hint.cloneHeal')).not.toContain(modifierCap('alt', { apple }))
  })

  it('offers all four, each with a name a screen reader can say', async () => {
    const { rendered, tab } = open()
    await fireEvent.click(tab('shortcuts'))
    for (const id of ['alt', 'meta', 'control', 'shift']) {
      expect(chip(rendered, id)).toBeTruthy()
    }
  })
})
