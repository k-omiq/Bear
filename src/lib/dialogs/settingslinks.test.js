/**
 * Where Settings opens: the seven sections, the ids callers used before the
 * restructure, and the anchor a missing-model notice sends the reader to.
 * The mounted half, focus landing on the anchor, is in
 * `SettingsDialog.tabs.dom.test.js`.
 */

import { describe, expect, it, vi } from 'vitest'

import { pushModal } from '../state/app.svelte.js'
import { SETTINGS_SECTIONS, openModelSettings, openSettings, resolveSettingsLink, settingsLinkForModel } from './settingslinks.js'

vi.mock('../state/app.svelte.js', () => ({ pushModal: vi.fn() }))

describe('the section a request resolves to', () => {
  it('offers seven sections, in the sidebar’s order', () => {
    expect(SETTINGS_SECTIONS).toEqual(['general', 'models', 'cloud', 'denoise', 'performance', 'shortcuts', 'about'])
    for (const id of SETTINGS_SECTIONS) expect(resolveSettingsLink(id)).toEqual({ section: id, anchor: null })
  })

  it.each([
    ['detection', { section: 'models', anchor: 'detection' }],
    ['cleaning', { section: 'models', anchor: 'cleaning' }],
    ['inference', { section: 'cloud', anchor: null }],
    ['acceleration', { section: 'performance', anchor: null }],
  ])('maps the old id %s to where its rows went', (id, expected) => {
    expect(resolveSettingsLink(id)).toEqual(expected)
  })

  it('reads anything unknown as General', () => {
    expect(resolveSettingsLink('billing')).toEqual({ section: 'general', anchor: null })
    expect(resolveSettingsLink(undefined)).toEqual({ section: 'general', anchor: null })
    expect(resolveSettingsLink({ tab: 'models' })).toEqual({ section: 'general', anchor: null })
  })

  it('keeps an anchor the section holds, and drops one it does not', () => {
    expect(resolveSettingsLink('models', 'samTs')).toEqual({ section: 'models', anchor: 'samTs' })
    expect(resolveSettingsLink('models', 'filtering')).toEqual({ section: 'models', anchor: 'filtering' })
    expect(resolveSettingsLink('models', 'access')).toEqual({ section: 'models', anchor: 'access' })
    expect(resolveSettingsLink('detection', 'mangaOcr')).toEqual({ section: 'models', anchor: 'mangaOcr' })
    expect(resolveSettingsLink('performance', 'runtime')).toEqual({ section: 'performance', anchor: 'runtime' })
    expect(resolveSettingsLink('inference', 'endpoints')).toEqual({ section: 'cloud', anchor: 'endpoints' })
    expect(resolveSettingsLink('models', 'runtime')).toEqual({ section: 'models', anchor: null })
    expect(resolveSettingsLink('general', 'samTs')).toEqual({ section: 'general', anchor: null })
  })
})

describe('the link that fixes a missing model', () => {
  it.each([
    ['samTs', { section: 'models', anchor: 'samTs' }],
    ['SAM-TS-L lettering mask', { section: 'models', anchor: 'samTs' }],
    ['Ogkalu comic text & bubble detector (Full)', { section: 'models', anchor: 'rtFull' }],
    ['rtFull', { section: 'models', anchor: 'rtFull' }],
    ['textDetector', { section: 'models', anchor: 'ctd' }],
    ['balloonDetector', { section: 'models', anchor: 'rtSmall' }],
    ['scriptGateLabels', { section: 'models', anchor: 'scriptGate' }],
    ['ocrVocab', { section: 'models', anchor: 'mangaOcr' }],
    ['inpainter', { section: 'models', anchor: 'lama' }],
    ['runtime', { section: 'performance', anchor: 'runtime' }],
    ['somethingNew', { section: 'models', anchor: 'other' }],
    [null, { section: 'models', anchor: null }],
  ])('sends %s to its row', (id, expected) => {
    expect(settingsLinkForModel(id)).toEqual(expected)
  })

  it('opens Settings there, with the anchor only when there is one', () => {
    openModelSettings('samTs')
    expect(pushModal).toHaveBeenLastCalledWith({ kind: 'settings', props: { tab: 'models', anchor: 'samTs' } })
    openSettings('inference')
    expect(pushModal).toHaveBeenLastCalledWith({ kind: 'settings', props: { tab: 'cloud' } })
  })
})
