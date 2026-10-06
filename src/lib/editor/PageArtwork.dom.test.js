import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import PageArtwork from './PageArtwork.svelte'

beforeEach(() => { globalThis.__TAURI_INTERNALS__ = { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` } })
afterEach(() => { cleanup(); delete globalThis.__TAURI_INTERNALS__; vi.restoreAllMocks() })
const page = (appearance = 'one', id = 'p1') => ({ id, chapterId: 'c1', index: id === 'p1' ? 0 : 1,
  width: 800, height: 1000, sourceSha: id, appearance, regions: [] })

it('draws native-composited clean tiles and retains the prior pixels until the replacement loads', async () => {
  const view = render(PageArtwork, { page: page(), variant: 'cleaned' })
  const first = view.container.querySelector('img[data-version]')
  expect(first.src).toContain('/cleaned/0?')
  await fireEvent.load(first)
  const prior = first.src
  expect(first.dataset.loadedVersion).toBe(first.dataset.version)
  await view.rerender({ page: page('two'), variant: 'cleaned' })
  const next = view.container.querySelector('img[data-version]')
  expect(next.src).not.toBe(prior)
  expect(next.dataset.loadedVersion).toBeUndefined()
  expect(next.style.visibility).toBe('hidden')
  expect(view.container.querySelector('.held')).toBe(first)
  expect(first.src).toBe(prior)
  await fireEvent.load(next)
  expect(next.style.visibility).toBe('')
  expect(next.dataset.loadedVersion).toBe(next.dataset.version)
  expect(view.container.querySelector('.held')).toBeNull()
})

it('never carries a loaded tile from another page into the new page', async () => {
  const view = render(PageArtwork, { page: page(), variant: 'cleaned' })
  await fireEvent.load(view.container.querySelector('img'))
  await view.rerender({ page: page('one', 'p2'), variant: 'cleaned' })
  expect(view.container.querySelector('.held')).toBeNull()
  expect(view.container.querySelector('img').dataset.loadedVersion).toBeUndefined()
  expect(view.container.querySelector('img').src).toContain('/1/cleaned/0?')
})

it('keeps displayed pixels through rapid edits and failed replacements', async () => {
  const view = render(PageArtwork, { page: page(), variant: 'cleaned' })
  const first = view.container.querySelector('img[data-version]')
  await fireEvent.load(first)
  await view.rerender({ page: page('two'), variant: 'cleaned' })
  const obsolete = view.container.querySelector('img[data-version]')
  await view.rerender({ page: page('three'), variant: 'cleaned' })
  const newest = view.container.querySelector('img[data-version]')
  await fireEvent.load(obsolete)
  await fireEvent.error(newest)
  expect(view.container.querySelector('.held')).toBe(first)
  expect(newest.style.visibility).toBe('hidden')
  await fireEvent.load(newest)
  expect(view.container.querySelector('img')).toBe(newest)
})

it('waits for decoding before removing the displayed tile', async () => {
  const view = render(PageArtwork, { page: page(), variant: 'cleaned' })
  const first = view.container.querySelector('img[data-version]')
  await fireEvent.load(first)
  await view.rerender({ page: page('two'), variant: 'cleaned' })
  const next = view.container.querySelector('img[data-version]')
  let decoded
  next.decode = vi.fn(() => new Promise(resolve => { decoded = resolve }))
  await fireEvent.load(next)
  expect(view.container.querySelector('.held')).toBe(first)
  expect(next.dataset.loadedVersion).toBeUndefined()
  decoded()
  await waitFor(() => expect(view.container.querySelector('img')).toBe(next))
  expect(next.dataset.loadedVersion).toBe(next.dataset.version)
})
