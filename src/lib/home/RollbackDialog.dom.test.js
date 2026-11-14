/**
 * The rollback dialog against a stub backend: the list, the warnings, the
 * install with its progress, and the skip it leaves behind.
 */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'

import { setBackend } from '../api/backend.js'
import { t } from '../i18n/index.js'
import { jobs } from '../state/jobs.svelte.js'
import { skippedVersion } from '../rollback.js'
import RollbackDialog from './RollbackDialog.svelte'
import UpdateDialog from './UpdateDialog.svelte'

const page = (v) => `https://github.com/k-omiq/Bear/releases/tag/v${v}`

const CATALOG = {
  currentVersion: '1.0.0-beta.21',
  bundle: 'app',
  inPlace: true,
  floor: '1.0.0-beta.3',
  manifestFormat: 6,
  versions: [
    { version: '1.0.0-beta.22', date: null, notes: 'Newer.', pageUrl: page('1.0.0-beta.22'), readsFormat: 6, honoursSkip: true },
    { version: '1.0.0-beta.21', date: '2026-11-14T00:00:00Z', notes: 'This one.', pageUrl: page('1.0.0-beta.21'), readsFormat: 6, honoursSkip: true },
    { version: '1.0.0-beta.20', date: '2026-11-05T01:54:06Z', notes: 'SAM-TS-L now sets up on a clean 16 GB PC.', pageUrl: page('1.0.0-beta.20'), readsFormat: 5, honoursSkip: false },
    { version: '1.0.0-beta.2', date: '2026-09-17T20:35:04Z', notes: 'First beta.', pageUrl: page('1.0.0-beta.2'), readsFormat: 2, honoursSkip: false },
  ],
}

/** @param {Partial<{prepare: (spec: any) => any}>} [over] */
function stub(over = {}) {
  const backend = {
    rollbackVersions: vi.fn(async () => structuredClone(CATALOG)),
    rollbackPrepare: vi.fn(
      over.prepare ??
        (async ({ version }) => ({
          currentVersion: CATALOG.currentVersion,
          version,
          body: '',
          downloadAndInstall: async (onEvent) => {
            onEvent({ event: 'Started', data: { contentLength: 100 } })
            onEvent({ event: 'Progress', data: { chunkLength: 100 } })
            onEvent({ event: 'Finished' })
          },
        })),
    ),
  }
  setBackend(/** @type {any} */ (backend))
  return backend
}

/** @param {ReturnType<typeof render>} r @param {string} v */
const row = (r, v) => /** @type {HTMLElement} */ (r.baseElement.querySelector(`[data-version="${v}"]`))

/** @param {ReturnType<typeof render>} r @param {string} label */
const button = (r, label) =>
  /** @type {HTMLButtonElement} */ ([...r.baseElement.querySelectorAll('button')].find((b) => b.textContent?.trim() === label))

/** This jsdom has no `localStorage`; the skip lives in one of these. */
function memoryStorage() {
  const map = new Map()
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => void map.set(k, String(v)),
    removeItem: (k) => void map.delete(k),
    clear: () => map.clear(),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  jobs.list = []
})

afterEach(() => {
  cleanup()
  setBackend(null)
  jobs.list = []
  vi.unstubAllGlobals()
})

it('lists the installed version and older releases, newest first, and keeps below-floor ones unchosen', async () => {
  stub()
  const r = render(RollbackDialog, { open: true })
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())

  const versions = [...r.baseElement.querySelectorAll('[data-version]')].map((el) => el.getAttribute('data-version'))
  expect(versions).toEqual(['1.0.0-beta.21', '1.0.0-beta.20', '1.0.0-beta.2'])
  expect(row(r, '1.0.0-beta.21').textContent).toContain(t('update.rollback.installed'))
  expect(row(r, '1.0.0-beta.21').querySelector('button')).toBeNull()
  expect(row(r, '1.0.0-beta.20').textContent).toContain('SAM-TS-L now sets up')
  expect(row(r, '1.0.0-beta.2').textContent).toContain(t('update.rollback.belowFloor'))
  expect(row(r, '1.0.0-beta.2').querySelector('button')).toBeNull()
})

it('warns before going back, installs with progress, and remembers the version left', async () => {
  const backend = stub()
  const r = render(RollbackDialog, { open: true })
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())
  await fireEvent.click(/** @type {HTMLElement} */ (row(r, '1.0.0-beta.20').querySelector('button')))

  const warnings = r.baseElement.querySelector('[data-testid="rollback-warnings"]')?.textContent ?? ''
  expect(warnings).toContain(t('update.rollback.warn.formatLoss', { version: 'v1.0.0-beta.20' }))
  expect(warnings).toContain(t('update.rollback.warn.library'))
  expect(warnings).toContain(t('update.rollback.warn.noSkip', { version: 'v1.0.0-beta.20', current: 'v1.0.0-beta.21' }))
  expect(warnings).not.toContain(t('update.rollback.warn.jobs', { count: 1 }))

  await fireEvent.click(button(r, t('update.rollback.action.confirm', { version: 'v1.0.0-beta.20' })))
  await waitFor(() => expect(button(r, t('update.action.restart'))).toBeTruthy())
  expect(backend.rollbackPrepare).toHaveBeenCalledWith({ version: '1.0.0-beta.20' })
  expect(skippedVersion()).toBe('1.0.0-beta.21')
})

it('says how many jobs are running before anything is downloaded', async () => {
  stub()
  jobs.list = /** @type {any} */ ([{ id: 'a', status: 'running' }, { id: 'b', status: 'running' }, { id: 'c', status: 'done' }])
  const r = render(RollbackDialog, { open: true })
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())
  await fireEvent.click(/** @type {HTMLElement} */ (row(r, '1.0.0-beta.20').querySelector('button')))
  const warnings = r.baseElement.querySelector('[data-testid="rollback-warnings"]')?.textContent ?? ''
  expect(warnings).toContain(t('update.rollback.warn.jobs', { count: 2 }))
})

it('offers the download page when the version has no manifest, and leaves no skip', async () => {
  stub({
    prepare: async () => {
      throw new Error('rollback_unreachable: no manifest for 1.0.0-beta.20')
    },
  })
  const r = render(RollbackDialog, { open: true })
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())
  await fireEvent.click(/** @type {HTMLElement} */ (row(r, '1.0.0-beta.20').querySelector('button')))
  await fireEvent.click(button(r, t('update.rollback.action.confirm', { version: 'v1.0.0-beta.20' })))
  await waitFor(() => expect(r.baseElement.textContent).toContain(t('update.rollback.error.unreachable')))
  expect(button(r, t('update.rollback.action.page'))).toBeTruthy()
  expect(skippedVersion()).toBe('')
})

it('refuses a wrongly signed build and puts the skip back', async () => {
  globalThis.localStorage.setItem('mangaCleaner.updateSkip.v1', JSON.stringify({ version: '1.0.0-beta.19' }))
  stub({
    prepare: async ({ version }) => ({
      version,
      downloadAndInstall: async () => {
        throw new Error('signature verification failed')
      },
    }),
  })
  const r = render(RollbackDialog, { open: true })
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())
  await fireEvent.click(/** @type {HTMLElement} */ (row(r, '1.0.0-beta.20').querySelector('button')))
  await fireEvent.click(button(r, t('update.rollback.action.confirm', { version: 'v1.0.0-beta.20' })))
  await waitFor(() => expect(r.baseElement.textContent).toContain(t('update.rollback.error.signature')))
  expect(button(r, t('update.action.restart'))).toBeFalsy()
  expect(button(r, t('update.rollback.action.page'))).toBeFalsy()
  expect(skippedVersion()).toBe('1.0.0-beta.19')
})

it('sends a copy that cannot be replaced in place to each download page', async () => {
  const backend = stub()
  backend.rollbackVersions.mockResolvedValue({ ...structuredClone(CATALOG), inPlace: false, bundle: 'deb' })
  const open = vi.spyOn(globalThis, 'open').mockImplementation(() => null)
  const r = render(RollbackDialog, { open: true })
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())
  expect(r.baseElement.textContent).toContain(t('update.rollback.inPlaceNote', { bundle: 'deb' }))
  await fireEvent.click(/** @type {HTMLElement} */ (row(r, '1.0.0-beta.20').querySelector('button')))
  expect(open).toHaveBeenCalledWith(page('1.0.0-beta.20'), '_blank', 'noopener,noreferrer')
  expect(backend.rollbackPrepare).not.toHaveBeenCalled()
  open.mockRestore()
})

it('is reached from the update dialog, which then closes', async () => {
  stub()
  const update = /** @type {any} */ ({ currentVersion: '1.0.0-beta.21', version: '1.0.0-beta.22', body: 'Newer.' })
  const r = render(UpdateDialog, { open: true, update })
  await fireEvent.click(button(r, t('update.rollback.openFromUpdate')))
  await waitFor(() => expect(row(r, '1.0.0-beta.20')).toBeTruthy())
  expect(r.baseElement.textContent).toContain(t('update.rollback.title'))
  expect(r.baseElement.textContent).not.toContain(t('update.title'))
})
