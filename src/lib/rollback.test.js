import { describe, expect, it } from 'vitest'

import {
  ROLLBACK_ERROR_KEYS,
  clearSkipped,
  compareVersions,
  offersPage,
  parseVersion,
  rememberSkipped,
  rollbackErrorCode,
  rollbackRows,
  shortNotes,
  shouldOfferUpdate,
  skippedVersion,
} from './rollback.js'

/** A `Storage` with nothing behind it, for the node environment. */
function memoryStore() {
  const map = new Map()
  return /** @type {Storage} */ (/** @type {unknown} */ ({
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, String(v)),
    removeItem: (k) => map.delete(k),
  }))
}

const entry = (version, extra = {}) => ({
  version,
  date: null,
  notes: null,
  pageUrl: `https://github.com/k-omiq/Bear/releases/tag/v${version}`,
  readsFormat: 5,
  honoursSkip: false,
  ...extra,
})

const catalog = (extra = {}) => ({
  currentVersion: '1.0.0-beta.21',
  bundle: 'app',
  inPlace: true,
  floor: '1.0.0-beta.3',
  manifestFormat: 6,
  versions: [
    entry('1.0.0-beta.22', { readsFormat: 6, honoursSkip: true }),
    entry('1.0.0-beta.9'),
    entry('1.0.0-beta.21', { readsFormat: 6, honoursSkip: true }),
    entry('1.0.0-beta.2', { readsFormat: 2 }),
    entry('1.0.0-beta.10'),
    entry('1.0.0-beta.3'),
    entry('1.0.0-beta.10'),
    entry('nightly'),
  ],
  ...extra,
})

describe('version order', () => {
  it('orders prerelease numbers as numbers and a release after its betas', () => {
    expect(compareVersions('1.0.0-beta.9', '1.0.0-beta.10')).toBe(-1)
    expect(compareVersions('1.0.0-beta.10', '1.0.0-beta.9')).toBe(1)
    expect(compareVersions('1.0.0', '1.0.0-beta.20')).toBe(1)
    expect(compareVersions('v1.0.0-beta.3', '1.0.0-beta.3')).toBe(0)
    expect(compareVersions('1.0.0-alpha', '1.0.0-beta')).toBe(-1)
    expect(compareVersions('1.0.0-beta', '1.0.0-beta.1')).toBe(-1)
    expect(compareVersions('0.9.10', '0.10.0')).toBe(-1)
  })

  it('reads only versions', () => {
    expect(parseVersion('1.0.0-beta.3')).toEqual({ core: [1, 0, 0], pre: ['beta', '3'] })
    expect(parseVersion('1.0')).toBeNull()
    expect(parseVersion('../1.0.0')).toBeNull()
    expect(parseVersion(undefined)).toBeNull()
  })
})

describe('the rollback list', () => {
  it('lists the installed version and older ones, newest first, each once', () => {
    const rows = rollbackRows(catalog())
    expect(rows.map((r) => r.version)).toEqual([
      '1.0.0-beta.21',
      '1.0.0-beta.10',
      '1.0.0-beta.9',
      '1.0.0-beta.3',
      '1.0.0-beta.2',
    ])
  })

  it('marks the installed version and refuses one below the floor', () => {
    const rows = rollbackRows(catalog())
    const state = Object.fromEntries(rows.map((r) => [r.version, r.state]))
    expect(state['1.0.0-beta.21']).toBe('installed')
    expect(state['1.0.0-beta.3']).toBe('available')
    expect(state['1.0.0-beta.2']).toBe('belowFloor')
  })

  it('says when a version cannot open the chapter format this build writes', () => {
    const rows = rollbackRows(catalog())
    expect(rows.find((r) => r.version === '1.0.0-beta.10')?.loses).toBe(true)
    expect(rows.find((r) => r.version === '1.0.0-beta.21')?.loses).toBe(false)
    const same = rollbackRows(catalog({ manifestFormat: 5 }))
    expect(same.find((r) => r.version === '1.0.0-beta.10')?.loses).toBe(false)
  })

  it('sends a copy that cannot be replaced in place to the download page', () => {
    const rows = rollbackRows(catalog({ inPlace: false, bundle: 'deb' }))
    expect(rows.find((r) => r.version === '1.0.0-beta.9')?.state).toBe('pageOnly')
    expect(rows.find((r) => r.version === '1.0.0-beta.2')?.state).toBe('belowFloor')
    expect(rows.find((r) => r.version === '1.0.0-beta.21')?.state).toBe('installed')
  })

  it('still marks the installed version when no release lists it', () => {
    const rows = rollbackRows(catalog({ currentVersion: '1.0.0-beta.30' }))
    expect(rows[0]).toMatchObject({ version: '1.0.0-beta.30', state: 'installed' })
  })

  it('shortens long notes at a word', () => {
    expect(shortNotes('  one\n two  ')).toBe('one two')
    const long = 'word '.repeat(60)
    const short = shortNotes(long, 40)
    expect(short.length).toBeLessThanOrEqual(41)
    expect(short.endsWith('…')).toBe(true)
    expect(shortNotes(null)).toBe('')
  })
})

describe('the skipped version', () => {
  it('hides the badge for the version left, and shows a newer release', () => {
    const store = memoryStore()
    expect(shouldOfferUpdate('1.0.0-beta.21', '1.0.0-beta.20', store)).toBe(true)
    expect(rememberSkipped('1.0.0-beta.21', store)).toBe('')
    expect(skippedVersion(store)).toBe('1.0.0-beta.21')
    expect(shouldOfferUpdate('1.0.0-beta.21', '1.0.0-beta.20', store)).toBe(false)
    expect(shouldOfferUpdate('1.0.0-beta.20', '1.0.0-beta.19', store)).toBe(false)
    expect(shouldOfferUpdate('1.0.0-beta.22', '1.0.0-beta.20', store)).toBe(true)
    expect(skippedVersion(store)).toBe('1.0.0-beta.21')
  })

  it('forgets the skip once the installed version reaches it', () => {
    const store = memoryStore()
    rememberSkipped('1.0.0-beta.21', store)
    expect(shouldOfferUpdate('1.0.0-beta.22', '1.0.0-beta.21', store)).toBe(true)
    expect(skippedVersion(store)).toBe('')
  })

  it('puts back the earlier value after a failed install', () => {
    const store = memoryStore()
    rememberSkipped('1.0.0-beta.19', store)
    const before = rememberSkipped('1.0.0-beta.21', store)
    expect(before).toBe('1.0.0-beta.19')
    rememberSkipped(before, store)
    expect(skippedVersion(store)).toBe('1.0.0-beta.19')
    clearSkipped(store)
    expect(skippedVersion(store)).toBe('')
  })

  it('treats a missing or broken record as no skip', () => {
    const store = memoryStore()
    store.setItem('mangaCleaner.updateSkip.v1', '{"version": 7}')
    expect(skippedVersion(store)).toBe('')
    store.setItem('mangaCleaner.updateSkip.v1', 'not json')
    expect(shouldOfferUpdate('1.0.0', '0.9.0', store)).toBe(true)
    expect(shouldOfferUpdate(null, '0.9.0', store)).toBe(false)
    expect(shouldOfferUpdate('1.0.0', '0.9.0', null)).toBe(true)
  })
})

describe('refusals', () => {
  it('reads the code from the backend refusal and the plugin signature error', () => {
    expect(rollbackErrorCode(new Error('rollback_unreachable: no manifest for 1.0.0'))).toBe('unreachable')
    expect(rollbackErrorCode('rollback_in_place_unsupported: deb')).toBe('in_place_unsupported')
    expect(rollbackErrorCode(new Error('Failed to install update: signature verification failed'))).toBe('signature')
    expect(rollbackErrorCode(new Error('disk full'))).toBe('')
  })

  it('has a message for every code and a page only where it helps', () => {
    for (const key of Object.values(ROLLBACK_ERROR_KEYS)) expect(key).toMatch(/^update\.rollback\.error\./)
    expect(offersPage('unreachable')).toBe(true)
    expect(offersPage('platform')).toBe(true)
    expect(offersPage('signature')).toBe(false)
    expect(offersPage('network')).toBe(false)
  })
})
