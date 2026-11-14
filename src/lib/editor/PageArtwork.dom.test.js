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

it('keeps the tile it is waiting for when a header\'s regions arrive', async () => {
  const view = render(PageArtwork, { page: { ...page(), resident: false }, variant: 'cleaned' })
  const pending = view.container.querySelector('img[data-version]')
  const asked = pending.src
  // `loadPages` answers for the same saved state: a new object, same digest.
  const regions = [{ id: 'r1', mask: { id: 'm1', appearance: 'layer-one' } }]
  await view.rerender({ page: { ...page(), resident: true, regions }, variant: 'cleaned' })
  expect(view.container.querySelectorAll('img')).toHaveLength(1)
  expect(view.container.querySelector('img[data-version]')).toBe(pending)
  expect(pending.src).toBe(asked)
  await fireEvent.load(pending)
  expect(pending.dataset.loadedVersion).toBe(pending.dataset.version)
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

it('lays the page at its own resolution over a scaled proxy while it is drawn larger, and only then', async () => {
  // 1600 wide: the proxy is 1024, scaled down from the page.
  const big = { ...page(), width: 1600, height: 2400 }
  const view = render(PageArtwork, { page: big, variant: 'original' })
  const tiles = [...view.container.querySelectorAll('img[data-version]')]
  expect(tiles.length).toBeGreaterThan(0)
  for (const tile of tiles) expect(tile.hasAttribute('data-proxy')).toBe(true)
  expect(view.container.querySelector('img.full')).toBeNull()

  await view.rerender({ page: big, variant: 'original', full: true })
  const whole = /** @type {HTMLImageElement} */ (view.container.querySelector('img.full'))
  // The URL with no tile segment: the page at its own resolution.
  expect(new URL(whole.src).pathname).toBe('/c1/0/source')
  // Over the tiles, and not one of them: nothing waits for it or samples it.
  expect(whole.hasAttribute('data-version')).toBe(false)
  expect(whole.hasAttribute('data-paint-key')).toBe(false)
  expect(whole.hasAttribute('data-proxy')).toBe(false)
  expect(whole.compareDocumentPosition(tiles.at(-1)) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy()

  await view.rerender({ page: big, variant: 'original', full: false })
  expect(view.container.querySelector('img.full')).toBeNull()
})

it('asks for no second image where the proxy is the page itself', async () => {
  const view = render(PageArtwork, { page: page(), variant: 'cleaned', full: true })
  expect(view.container.querySelector('img.full')).toBeNull()
  expect(view.container.querySelector('img[data-proxy]')).toBeNull()
})
