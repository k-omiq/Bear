/**
 * The Nano Banana window: a yes saves the instruction, then takes a grant for
 * that many regions on the model the window showed. A caller sees only the
 * boolean; the backend keeps the grant and sends it with each render.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'

const stores = vi.hoisted(() => ({ notify: vi.fn(), modals: /** @type {any[]} */ ([]) }))
vi.mock('../state/app.svelte.js', () => ({
  notify: stores.notify,
  pushModal: (/** @type {any} */ spec) => stores.modals.push(spec),
}))

import { confirmNanoBanana } from './nanobananaask.svelte.js'

function backend() {
  const calls = /** @type {string[]} */ ([])
  return {
    calls,
    readSettings: vi.fn(async () => ({ nanoBananaModel: 'gemini-3-pro-image', nanoBananaPrompt: 'old' })),
    writeSettings: vi.fn(async () => { calls.push('write'); return {} }),
    grantNanoBanana: vi.fn(async () => { calls.push('grant'); return { grant: 'g', model: 'gemini-3-pro-image', count: 3 } }),
  }
}

/** @param {string} id @param {string} [prompt] */
async function answer(id, prompt = 'keep the dots') {
  await vi.waitFor(() => expect(stores.modals.length).toBe(1))
  stores.modals.pop().onresolve({ id, prompt })
}

beforeEach(() => {
  vi.clearAllMocks()
  stores.modals.length = 0
})

describe('confirmNanoBanana', () => {
  it('saves the instruction, then takes the grant for the model shown', async () => {
    const api = backend()
    const asked = confirmNanoBanana(3, /** @type {any} */ (api))
    await answer('confirm')
    expect(await asked).toBe(true)
    expect(api.writeSettings).toHaveBeenCalledWith(expect.objectContaining({ nanoBananaPrompt: 'keep the dots' }))
    expect(api.grantNanoBanana).toHaveBeenCalledWith({ model: 'gemini-3-pro-image', count: 3 })
    expect(api.calls).toEqual(['write', 'grant'])
  })

  it('takes no grant when the answer is no', async () => {
    const api = backend()
    const asked = confirmNanoBanana(1, /** @type {any} */ (api))
    await answer('cancel')
    expect(await asked).toBe(false)
    expect(api.grantNanoBanana).not.toHaveBeenCalled()
  })

  it('sends nothing when no grant was given, and says why', async () => {
    const api = backend()
    api.grantNanoBanana.mockRejectedValueOnce(new Error('decline.reason.nanoBananaOff'))
    const asked = confirmNanoBanana(1, /** @type {any} */ (api))
    await answer('confirm')
    expect(await asked).toBe(false)
    expect(stores.notify).toHaveBeenCalledWith({ key: 'decline.reason.nanoBananaOff', tone: 'warn' })
  })
})
