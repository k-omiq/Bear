/**
 * The window in front of a Nano Banana request, mounted through the modal
 * host and asked by `confirmNanoBanana`: it states the model, the size, the
 * count and the cost, opens with the saved instruction, and Send answers with
 * what is in the box. Escape is Cancel and the backdrop is inert, and an
 * Escape that an input method takes (a Japanese or Korean candidate being
 * cancelled) leaves the window and the text alone.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import { app } from '../state/app.svelte.js'
import { confirmNanoBanana } from '../editor/nanobananaask.svelte.js'
import ModalHost from '../shell/ModalHost.svelte'

afterEach(() => {
  cleanup()
  app.modals.length = 0
  vi.restoreAllMocks()
})

/** A backend with a saved instruction, and the window asked for two regions. */
async function ask() {
  const backend = {
    readSettings: vi.fn(async () => ({ nanoBananaModel: 'gemini-3.1-flash-image', nanoBananaPrompt: 'keep the hatching' })),
    writeSettings: vi.fn(async () => ({})),
    grantNanoBanana: vi.fn(async () => ({ grant: 'g', model: 'gemini-3.1-flash-image', count: 2 })),
  }
  render(ModalHost)
  const asked = confirmNanoBanana(2, 'chapter', /** @type {any} */ (backend))
  const box = /** @type {HTMLTextAreaElement} */ (await screen.findByLabelText(t('nanoBanana.run.promptLabel')))
  return { backend, asked, box }
}

describe('the Nano Banana window', () => {
  it('states the model, the size, the count and the cost, and sends what is in the box', async () => {
    const { backend, asked, box } = await ask()
    expect(screen.getByText(t('nanoBanana.run.model', { model: t('nanoBanana.model.v2'), size: '512' }))).toBeTruthy()
    expect(screen.getByText(t('nanoBanana.run.regions', { count: 2 }))).toBeTruthy()
    expect(screen.getByText(t('nanoBanana.run.cost', { low: '$0.09', high: '$0.20' }))).toBeTruthy()
    expect(box.value).toBe('keep the hatching')
    await fireEvent.input(box, { target: { value: 'also the arrow' } })
    await fireEvent.click(screen.getByRole('button', { name: t('nanoBanana.run.confirm') }))
    expect(await asked).toBe('g')
    expect(backend.writeSettings).toHaveBeenCalledWith({ nanoBananaPrompt: 'also the arrow' })
    expect(app.modals).toEqual([])
  })

  it('names the count and the cost as its description, since focus lands in the box', async () => {
    const { asked } = await ask()
    const dialog = screen.getByRole('dialog')
    const ids = String(dialog.getAttribute('aria-describedby') ?? '').split(' ').filter(Boolean)
    expect(ids.map((id) => document.getElementById(id)?.textContent)).toEqual([
      t('nanoBanana.run.regions', { count: 2 }),
      t('nanoBanana.run.cost', { low: '$0.09', high: '$0.20' }),
    ])
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(await asked).toBeNull()
  })

  it('treats Escape as Cancel and sends nothing', async () => {
    const { backend, asked } = await ask()
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(await asked).toBeNull()
    expect(backend.writeSettings).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('does not close on the backdrop', async () => {
    const { asked } = await ask()
    const backdrop = /** @type {HTMLElement} */ (screen.getByRole('dialog').parentElement)
    await fireEvent.click(backdrop)
    expect(screen.getByRole('dialog')).toBeTruthy()
    await fireEvent.click(screen.getByRole('button', { name: t('nanoBanana.run.cancel') }))
    expect(await asked).toBeNull()
  })

  it('leaves an Escape the input method takes to the input method, and keeps the text', async () => {
    const { asked, box } = await ask()
    await fireEvent.input(box, { target: { value: '矢印も消して' } })
    await fireEvent.keyDown(box, { key: 'Escape', isComposing: true })
    // WebKit can end a composition with isComposing false and key code 229.
    await fireEvent.keyDown(box, { key: 'Escape', keyCode: 229 })
    expect(screen.getByRole('dialog')).toBeTruthy()
    expect(box.value).toBe('矢印も消して')
    expect(app.modals).toHaveLength(1)
    await fireEvent.keyDown(box, { key: 'Escape' })
    expect(await asked).toBeNull()
  })
})
