import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import LoadedModels from './LoadedModels.svelte'

afterEach(cleanup)

const local = { id: 1, name: 'LaMa Manga', size: '210 MB', device: 'CPU', unloading: false, unloadLabel: 'Free the memory LaMa Manga is using' }
const cloudRow = {
  id: 'cloud-render', name: 'Cloud GPU · L4', size: '~$0.80/h', device: 'List price estimate',
  detail: 'Idle, stops in ~1:30', unloading: false, unloadingLabel: 'Stopping…', unloadLabel: 'Stop cloud GPU',
}

it('draws nothing when nothing is loaded or up', () => {
  const { container } = render(LoadedModels, { models: [], title: 'Using memory now', unloadingLabel: 'Freeing…', onunload: () => {} })
  expect(container.querySelector('section')).toBeNull()
})

it('renders a cloud GPU row on its own, with its state line and a stop button', async () => {
  const onunload = vi.fn()
  render(LoadedModels, { models: [cloudRow], title: 'Running in the cloud', icon: 'cloud', unloadingLabel: 'Freeing…', onunload })
  expect(screen.getByRole('region', { name: 'Running in the cloud' })).toBeTruthy()
  expect(screen.getByText('Cloud GPU · L4').getAttribute('title')).toBe('List price estimate')
  expect(screen.getByText('Idle, stops in ~1:30')).toBeTruthy()
  expect(screen.getByText('~$0.80/h')).toBeTruthy()
  await fireEvent.click(screen.getByRole('button', { name: 'Stop cloud GPU' }))
  expect(onunload).toHaveBeenCalledWith('cloud-render')
})

it('says each row is on its way out in its own words', () => {
  render(LoadedModels, {
    models: [{ ...local, unloading: true }, { ...cloudRow, unloading: true }],
    title: 'Using memory and a cloud GPU', unloadingLabel: 'Freeing…', onunload: () => {},
  })
  expect(screen.getByText('Freeing…')).toBeTruthy()
  expect(screen.getByText('Stopping…')).toBeTruthy()
  expect(screen.queryByRole('button')).toBeNull()
})
