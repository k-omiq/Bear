// A gap found by trying to break the dev log section, now fixed.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import DevLogSection from './DevLogSection.svelte'

const button = (/** @type {string} */ key) => /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t(key) }))

/** @type {{ readDevLog: ReturnType<typeof vi.fn>, clearDevLog: ReturnType<typeof vi.fn> }} */
let backend

beforeEach(() => {
  backend = {
    readDevLog: vi.fn(async () => 'first line\nsecond line\n'),
    clearDevLog: vi.fn(async () => {}),
  }
  setBackend(/** @type {any} */ (backend))
  vi.stubGlobal('navigator', { ...globalThis.navigator, clipboard: { writeText: vi.fn(async () => {}) } })
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('DevLogSection gaps', () => {
  // A person reading an old line during a flood keeps their place on every
  // Refresh and every return.
  it('keeps the place of someone reading further up', async () => {
    render(DevLogSection, { visible: true })
    const view = /** @type {HTMLElement} */ (await screen.findByText(/first line/))
    Object.defineProperty(view, 'scrollHeight', { configurable: true, value: 5000 })
    Object.defineProperty(view, 'clientHeight', { configurable: true, value: 400 })
    view.scrollTop = 100
    backend.readDevLog.mockResolvedValue('first line\nsecond line\na newer line\n')
    await fireEvent.click(button('settings.devlog.refresh'))
    await screen.findByText(/a newer line/)
    await new Promise((resolve) => setTimeout(resolve))
    expect(view.scrollTop).toBe(100)
  })
})
