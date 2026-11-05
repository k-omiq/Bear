/**
 * The Nano Banana models this app can send to: what each answers at, and what
 * it costs. The native list is `cleaner-core/src/engines/nano_banana.rs`
 * (`Model::ALL`), which is the one that decides what a request carries; this
 * copy only draws the pickers and the cost, and a test keeps the two in step.
 *
 * Read from Google's image generation and pricing pages on 2026-11-03. Only
 * `gemini-3.1-flash-image` answers at 512.
 */

/** The longest instruction sent, in characters (`MAX_INSTRUCTION_CHARS` native). */
export const MAX_INSTRUCTION_CHARS = 600

/** The most regions one window may grant (`nano_banana_consent.rs#MAX_REGIONS`). */
export const MAX_GRANT_REGIONS = 2000

/**
 * Dollars per image at Google's standard price, by size.
 *
 * @typedef {{ id: string, labelKey: string, lowest: string, usd: Record<string, number> }} NanoBananaModel
 * @type {ReadonlyArray<NanoBananaModel>}
 */
export const NANO_BANANA_MODELS = Object.freeze([
  { id: 'gemini-nano-banana-2.1', labelKey: 'nanoBanana.model.v21', lowest: '1K', usd: { '1K': 0.0336, '2K': 0.0504 } },
  { id: 'gemini-3.1-flash-image', labelKey: 'nanoBanana.model.v2', lowest: '512', usd: { '512': 0.045, '1K': 0.067, '2K': 0.101 } },
  { id: 'gemini-3-pro-image', labelKey: 'nanoBanana.model.pro', lowest: '1K', usd: { '1K': 0.134, '2K': 0.134 } },
])

/** The model a stored setting that is not one of ours means. */
export const DEFAULT_NANO_BANANA_MODEL = 'gemini-3.1-flash-image'

/** @param {unknown} id */
export function nanoBananaModel(id) {
  return NANO_BANANA_MODELS.find((model) => model.id === id)
    ?? /** @type {NanoBananaModel} */ (NANO_BANANA_MODELS.find((model) => model.id === DEFAULT_NANO_BANANA_MODEL))
}

/**
 * The size crops start at: the smallest the model answers at. It is never a
 * setting. Native picks, for each region, the smallest chart size that holds it
 * with page around it, and a region too big for that size moves up.
 *
 * @param {unknown} modelId
 */
export function nanoBananaStartSize(modelId) {
  return nanoBananaModel(modelId).lowest
}

/**
 * The lowest and highest cost of `count` regions: at the model's smallest size,
 * and at its largest, since a region too big for a size moves up.
 *
 * @param {number} count
 * @param {unknown} modelId
 */
export function nanoBananaCost(count, modelId) {
  const model = nanoBananaModel(modelId)
  return {
    low: count * model.usd[model.lowest],
    high: count * Math.max(...Object.values(model.usd)),
  }
}
