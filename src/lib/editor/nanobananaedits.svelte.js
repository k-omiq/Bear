/**
 * The single-region Nano Banana requests in flight, each with the grant its
 * window made: Try again or Try again wider on a Nano Banana layer, Clean with
 * > Nano Banana, Clean anyway with Nano Banana.
 *
 * One can take many minutes and is billed, so the region's row shows a Stop
 * for it (`MaskRow.svelte`). Stop names that request's grant
 * (`nanoBananaStop`), so it ends that request only: a batch, which is a run
 * and stops as one, and another region's request go on. The quit guard counts
 * these natively (`jobs.rs#running`).
 */
import { SvelteMap } from 'svelte/reactivity'
import { getBackend } from '../api/backend.js'

/** The grant of the request in flight for each region, by region id. */
const out = new SvelteMap()

/**
 * Whether a Nano Banana request is in flight for this region.
 *
 * @param {string} regionId
 */
export function nanoBananaEditOut(regionId) {
  return out.has(regionId)
}

/**
 * `work`, counted as this region's Nano Banana request on `grant` while it
 * runs. With no grant, `work` alone: it does not reach Nano Banana.
 *
 * @template T
 * @param {string} regionId
 * @param {string|null|undefined} grant
 * @param {() => Promise<T>} work
 * @returns {Promise<T>}
 */
export async function whileNanoBananaEdit(regionId, grant, work) {
  if (!grant) return work()
  out.set(regionId, grant)
  try {
    return await work()
  } finally {
    if (out.get(regionId) === grant) out.delete(regionId)
  }
}

/**
 * Stop this region's request. Answers whether a Stop was sent. The request
 * itself answers as stopped, and the row says so as for any refusal.
 *
 * @param {string} regionId
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {Promise<boolean>}
 */
export async function stopNanoBananaEdit(regionId, backend = getBackend()) {
  const grant = out.get(regionId)
  if (!grant) return false
  try {
    await backend.nanoBananaStop({ grant })
  } catch (error) {
    console.error('a Nano Banana request could not be stopped', error)
    return false
  }
  return true
}
