/**
 * Providers this version does not connect to: shown as Paused and never
 * chosen. The native side refuses them too (`inference::provider_paused`), so
 * this only keeps the interface from offering what would be refused.
 */
export const PAUSED_PROVIDERS = Object.freeze(['beam'])

/** @param {unknown} provider */
export function providerPaused(provider) {
  return PAUSED_PROVIDERS.includes(/** @type {string} */ (provider))
}
