import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { tick } from 'svelte'
import { setBackend } from '../api/backend.js'
import { editor } from '../state/editor.svelte.js'
import { app, pushModal } from '../state/app.svelte.js'
import { session } from '../state/session.svelte.js'
import ExportDialog from './ExportDialog.svelte'

let backend
beforeEach(() => {
  editor.project = { id: 'p1', mode: 'single' }
  editor.chapter = { id: 'c1', number: 1, review: [], pages: [{ id: 'page1', index: 0, width: 2, height: 2, regions: [], resident: true }] }
  backend = {
    planExportChapter: vi.fn(async () => ({ status: 'planned', revision: 'revision-1', fileCount: 1, actualFormats: ['TIFF'], declared: [{ key: 'export.declared.formatChanged', params: { page: 1, requested: 'PNG', used: 'TIFF', mode: 'Cmyk' } }], outputs: [] })),
    exportChapter: vi.fn(async () => ({ status: 'exported' })),
  }
  setBackend(backend)
})
afterEach(() => { cleanup(); setBackend(null); editor.chapter = null; editor.project = null; app.modals.length = 0 })
function open() { pushModal({ kind: 'export', titleKey: 'modal.title.export', props: {} }); return render(ExportDialog, { props: { spec: app.modals.at(-1) } }) }
it('shows actual format before export and binds execution to the reviewed revision', async () => {
  const view = open()
  await waitFor(() => expect(view.getByText('1 TIFF file')).toBeTruthy())
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
  await waitFor(() => expect(view.getByText('1 TIFF file')).toBeTruthy())
  resolveFirst({ status: 'planned', revision: 'stale', fileCount: 1, actualFormats: ['JPEG'], declared: [] })
  await tick()
  expect(view.queryByText('1 JPEG file')).toBeNull()
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
it('exports the page on screen alone when asked', async () => {
  editor.chapter.pages.push({ id: 'page2', index: 1, width: 2, height: 2, regions: [], resident: true })
  editor.pageIndex = 1
  const view = open()
  await waitFor(() => expect(backend.planExportChapter).toHaveBeenCalledWith(expect.objectContaining({ pages: undefined })))
  await fireEvent.click(view.getByRole('radio', { name: 'This page (2)' }))
  await waitFor(() => expect(backend.planExportChapter).toHaveBeenLastCalledWith(expect.objectContaining({ pages: [1] })))
  await waitFor(() => expect(view.getByRole('button', { name: 'Export PNG' }).disabled).toBe(false))
  await fireEvent.click(view.getByRole('button', { name: 'Export PNG' }))
  expect(backend.exportChapter).toHaveBeenCalledWith(expect.objectContaining({ pages: [1], planRevision: 'revision-1' }))
  editor.pageIndex = 0
})
it('counts the flags and files of what it writes, and never a part of a CBZ', async () => {
  editor.chapter.pages.push({ id: 'page2', index: 1, width: 2, height: 2, regions: [], resident: true })
  editor.chapter.review = [{ pageIndex: 0 }, { pageIndex: 0 }, { pageIndex: 1 }]
  editor.pageIndex = 1
  backend.planExportChapter.mockImplementation(async (options) => ({ status: 'planned', revision: 'r', fileCount: options.pages?.length ?? 2, actualFormats: ['PNG'], declared: [], outputs: [] }))
  const view = open()
  await waitFor(() => expect(view.getByText('2 PNG files')).toBeTruthy())
  expect(view.getByText(/3 regions are still flagged/)).toBeTruthy()
  await fireEvent.click(view.getByRole('radio', { name: 'This page (2)' }))
  await waitFor(() => expect(view.getByText(/1 region is still flagged/)).toBeTruthy())
  await fireEvent.click(view.getByRole('radio', { name: 'Separate layer' }))
  await waitFor(() => expect(view.getByText('Plus 1 PNG mask file')).toBeTruthy())
  await fireEvent.click(view.getByRole('radio', { name: 'CBZ' }))
  await waitFor(() => expect(view.getByText('1 CBZ file')).toBeTruthy())
  expect(view.getByRole('radio', { name: 'This page (2)' }).disabled).toBe(true)
  expect(backend.planExportChapter).toHaveBeenLastCalledWith(expect.objectContaining({ format: 'CBZ', pages: undefined }))
  expect(view.queryByText(/mask file/)).toBeNull()
  editor.pageIndex = 0
})
it('sends the colour choice with the plan and the export', async () => {
  const view = open()
  await waitFor(() => expect(backend.planExportChapter).toHaveBeenCalledWith(expect.objectContaining({ grayscale: false })))
  expect(view.getByText(/Every page keeps its own color type/)).toBeTruthy()
  await fireEvent.click(view.getByRole('radio', { name: 'Gray pages as grayscale' }))
  await waitFor(() => expect(backend.planExportChapter).toHaveBeenLastCalledWith(expect.objectContaining({ grayscale: true })))
  expect(view.getByText(/is written as grayscale/)).toBeTruthy()
  await waitFor(() => expect(view.getByRole('button', { name: 'Export PNG' }).disabled).toBe(false))
  await fireEvent.click(view.getByRole('button', { name: 'Export PNG' }))
  expect(backend.exportChapter).toHaveBeenCalledWith(expect.objectContaining({ grayscale: true, planRevision: 'revision-1' }))
})
it('starts on the colour choice Settings names', async () => {
  session.exportTone = 'grayscale'
  try {
    const view = open()
    await waitFor(() => expect(backend.planExportChapter).toHaveBeenCalledWith(expect.objectContaining({ grayscale: true })))
    expect(view.getByRole('radio', { name: 'Gray pages as grayscale' }).getAttribute('aria-checked')).toBe('true')
  } finally {
    session.exportTone = 'source'
  }
})
it('names each format card by its format and describes it by its use', async () => {
  const view = open()
  const psd = view.getByRole('radio', { name: 'PSD' })
  expect(psd.getAttribute('aria-describedby')).toBeTruthy()
  expect(document.getElementById(psd.getAttribute('aria-describedby')).textContent).toBe('Layers for Photoshop')
  await fireEvent.click(psd)
  await waitFor(() => expect(view.getByRole('button', { name: 'Export PSD' })).toBeTruthy())
})
