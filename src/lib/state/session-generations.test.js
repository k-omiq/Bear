/**
 * Generations per region in the session: 1 to 8, 1 when unset or anything
 * else, read back from the stored record and from settings.json, and written
 * into the patch the native run reads (`variations.rs#GENERATIONS_KEY`).
 */

import { afterEach, describe, expect, it, vi } from 'vitest'

vi.mock('./persist.js', async (importOriginal) => {
  const actual = await importOriginal()
  return { ...actual, writeRecord: () => true }
})
import {
  adoptBackendSettings,
  backendSettingsPatch,
  sanitizeSession,
  session,
  setGenerationsPerRegion,
} from './session.svelte.js'

afterEach(() => {
  setGenerationsPerRegion(1)
})

describe('Generations per region', () => {
  it('starts at 1 and keeps a whole number from 1 to 8', () => {
    expect(sanitizeSession({}).generationsPerRegion).toBe(1)
    expect(sanitizeSession({ generationsPerRegion: 5 }).generationsPerRegion).toBe(5)
    for (const bad of [0, 9, 2.5, '3', null, -1]) {
      expect(sanitizeSession({ generationsPerRegion: bad }).generationsPerRegion, String(bad)).toBe(1)
    }
  })

  it('ignores a count out of range, and saves one in range into the native patch', () => {
    setGenerationsPerRegion(4)
    expect(session.generationsPerRegion).toBe(4)
    for (const bad of [0, 9, 1.5, '2']) setGenerationsPerRegion(/** @type {any} */ (bad))
    expect(session.generationsPerRegion).toBe(4)
    expect(backendSettingsPatch()).toMatchObject({ generationsPerRegion: 4 })
  })

  it('is read back from settings.json', () => {
    adoptBackendSettings({ generationsPerRegion: 8 })
    expect(session.generationsPerRegion).toBe(8)
    adoptBackendSettings({ generationsPerRegion: 12 })
    expect(session.generationsPerRegion).toBe(1)
  })
})
