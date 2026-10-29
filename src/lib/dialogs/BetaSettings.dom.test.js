/**
 * Settings > Beta and what it gates: the Patch tool on the rail and in the
 * shortcut sheet, saved at once with the session and written to the
 * settings file, off by default. The engine pickers' half (the Screentone
 * fill) is held by `model/masks.test.js`, `MaskRow.dom.test.js` and the
 * editor's tool tests.
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { t } from '../i18n/index.js'
import { editor } from '../state/editor.svelte.js'
import { adoptBackendSettings, backendSettingsPatch, session, setBetaTools } from '../state/session.svelte.js'
import BetaSettings from './BetaSettings.svelte'
import ShortcutSheet from './ShortcutSheet.svelte'
import ToolRail from '../editor/ToolRail.svelte'

/** Local storage for the test: the node running the suite has none. */
function memoryStorage() {
  /** @type {Map<string, string>} */
  const values = new Map()
  return {
    get length() {
      return values.size
    },
    key: (/** @type {number} */ index) => [...values.keys()][index] ?? null,
    getItem: (/** @type {string} */ key) => values.get(key) ?? null,
    setItem: (/** @type {string} */ key, /** @type {string} */ value) => void values.set(key, String(value)),
    removeItem: (/** @type {string} */ key) => void values.delete(key),
    clear: () => values.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  setBetaTools(false)
})

afterEach(() => {
  cleanup()
  setBetaTools(false)
  editor.tool = 'autoClean'
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

const toggle = () => /** @type {HTMLInputElement} */ (screen.getByLabelText(t('settings.beta.tools')))

describe('Settings > Beta', () => {
  it('offers the Patch tool and the Screentone fill as one beta switch, off by default', () => {
    const backend = { writeSettings: vi.fn(async () => ({})) }
    render(BetaSettings, { props: { backend: /** @type {any} */ (backend) } })
    expect(toggle().checked).toBe(false)
    expect(screen.getByText(t('settings.beta.tag'))).toBeTruthy()
    expect(screen.getByText(t('settings.beta.toolsHint'))).toBeTruthy()
  })

  it('saves the choice with the session at once and writes it to the settings file', async () => {
    const backend = { writeSettings: vi.fn(async () => ({})) }
    render(BetaSettings, { props: { backend: /** @type {any} */ (backend) } })
    await fireEvent.click(toggle())
    expect(session.betaTools).toBe(true)
    await vi.waitFor(() => expect(backend.writeSettings).toHaveBeenCalledWith({ betaTools: true }))
    expect(JSON.parse(localStorage.getItem('mangaCleaner.session.v1') ?? '{}').betaTools).toBe(true)
    expect(backendSettingsPatch().betaTools).toBe(true)
    await fireEvent.click(toggle())
    expect(session.betaTools).toBe(false)
  })

  it('keeps the session\'s choice when the settings file cannot be written', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const backend = { writeSettings: vi.fn(async () => { throw new Error('disk full') }) }
    render(BetaSettings, { props: { backend: /** @type {any} */ (backend) } })
    await fireEvent.click(toggle())
    await vi.waitFor(() => expect(backend.writeSettings).toHaveBeenCalled())
    expect(session.betaTools).toBe(true)
  })

  it('takes the choice from the settings file like the other shared preferences', () => {
    adoptBackendSettings({ betaTools: true })
    expect(session.betaTools).toBe(true)
    adoptBackendSettings({ betaTools: 'yes' })
    expect(session.betaTools).toBe(false)
  })
})

describe('what Settings > Beta gates', () => {
  it('puts the Patch tool on the rail only while it is on, and keeps the Gradient either way', async () => {
    render(ToolRail)
    expect(screen.queryByRole('radio', { name: new RegExp(t('tools.name.patch')) })).toBeNull()
    expect(screen.getByRole('radio', { name: new RegExp(t('tools.name.gradient')) })).toBeTruthy()
    setBetaTools(true)
    await vi.waitFor(() => expect(screen.getByRole('radio', { name: new RegExp(t('tools.name.patch')) })).toBeTruthy())
  })

  it('lists the Patch tool\'s key in the shortcut sheet only while it is on', async () => {
    render(ShortcutSheet)
    expect(screen.queryByText(t('tools.name.patch'))).toBeNull()
    expect(screen.getByText(t('tools.name.gradient'))).toBeTruthy()
    setBetaTools(true)
    await vi.waitFor(() => expect(screen.getByText(t('tools.name.patch'))).toBeTruthy())
  })
})
