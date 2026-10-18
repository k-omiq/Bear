import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { tick } from 'svelte'
import { setBackend } from '../api/backend.js'
import { editor } from '../state/editor.svelte.js'
import { app, pushModal } from '../state/app.svelte.js'
import ExportDialog from './ExportDialog.svelte'

let backend
beforeEach(() => {
  editor.project = { id: 'p1', mode: 'single' }
  editor.chapter = { id: 'c1', number: 1, review: [], pages: [{ id: 'page1', index: 0, width: 2, height: 2, regions: [], resident: true }] }
  backend = {
    planExportChapter: vi.fn(async () => ({ status: 'planned', revision: 'revision-1', actualFormats: ['TIFF'], declared: [{ key: 'export.declared.formatChanged', params: { page: 1, requested: 'PNG', used: 'TIFF', mode: 'Cmyk' } }], outputs: [] })),
    exportChapter: vi.fn(async () => ({ status: 'exported' })),
  }
  setBackend(backend)
})
afterEach(() => { cleanup(); setBackend(null); editor.chapter = null; editor.project = null; app.modals.length = 0 })
function open() { pushModal({ kind: 'export', titleKey: 'modal.title.export', props: {} }); return render(ExportDialog, { props: { spec: app.modals.at(-1) } }) }
it('shows actual format before export and binds execution to the reviewed revision', async () => {
  const view = open()
  await waitFor(() => expect(view.getByText('Output files: TIFF.')).toBeTruthy())
  expect(view.getByText(/Page 1: PNG will be saved as lossless TIFF/)).toBeTruthy()
  await fireEvent.click(view.getByRole('button', { name: 'Export PNG' }))
  expect(backend.exportChapter).toHaveBeenCalledWith(expect.objectContaining({ planRevision: 'revision-1', format: 'PNG' }))
})
it('discards stale plan responses when the requested format changes', async () => {
  let resolveFirst
  backend.planExportChapter.mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve }))
  const view = open()
  await tick()
  await fireEvent.click(view.getByRole('radio', { name: 'TIFF' }))
  await waitFor(() => expect(view.getByText('Output files: TIFF.')).toBeTruthy())
  resolveFirst({ status: 'planned', revision: 'stale', actualFormats: ['JPEG'], declared: [] })
  await tick()
  expect(view.queryByText('Output files: JPEG.')).toBeNull()
})
it('keeps the dialog open and surfaces validation failure', async () => {
  backend.planExportChapter.mockResolvedValue({ status: 'refused', reasonKey: 'notice.export.invalidSource', detail: 'page 2: incompatible ICC profile', actualFormats: [], declared: [] })
  const view = open()
  await waitFor(() => expect(view.getByRole('alert').textContent).toContain('page 2: incompatible ICC profile'))
  expect(view.getByRole('button', { name: 'Export PNG' }).disabled).toBe(true)
  expect(backend.exportChapter).not.toHaveBeenCalled()
})
it('names a destination that is not there and offers the folder picker', async () => {
  const missing = '/Volumes/Portable/scans/ch1_cleaned'
  backend.planExportChapter.mockImplementation(async ({ destination }) => destination === 'new-folder'
    ? { status: 'refused', reasonKey: 'notice.export.destinationUnavailable', path: missing, actualFormats: [], declared: [] }
    : { status: 'planned', revision: 'revision-2', actualFormats: ['PNG'], declared: [], outputs: [] })
  const view = open()
  await waitFor(() => expect(view.getByRole('alert').textContent).toContain(missing))
  expect(view.getByRole('button', { name: 'Export PNG' }).disabled).toBe(true)
  await fireEvent.click(view.getByRole('button', { name: 'Choose another folder…' }))
  // No chooser outside a Tauri window: the path field is the way in.
  const field = await waitFor(() => view.getByRole('textbox'))
  await fireEvent.input(field, { target: { value: '/Users/me/out' } })
  await waitFor(() => expect(view.getByRole('button', { name: 'Export PNG' }).disabled).toBe(false))
  await fireEvent.click(view.getByRole('button', { name: 'Export PNG' }))
  expect(backend.exportChapter).toHaveBeenCalledWith(expect.objectContaining({ destination: '/Users/me/out', planRevision: 'revision-2' }))
})
