/**
 * The engine ladder: fill → manga-LaMa → FLUX, ordered weakest/cheapest to
 * strongest/most expensive. `stronger`/`simpler` step along it and clamp at
 * both ends - there is no engine below fill or above FLUX.
 *
 * `decline` is a terminal pipeline outcome, not a
 * rung to escalate to or from; it lives on `Region.outcome`, not here.
 */

/** @type {ReadonlyArray<string>} */
export const RUNGS = Object.freeze(['fill', 'lama', 'flux'])

const LABEL_KEYS = Object.freeze({
  fill: 'ladder.rung.fill',
  lama: 'ladder.rung.lama',
  flux: 'ladder.rung.flux',
  cloud: 'ladder.rung.cloud',
  paint: 'ladder.rung.paint',
  clone: 'ladder.rung.clone',
})

/**
 * A rung name as it was stored or sent, in today's ladder.
 *
 * `denoise` was rung 1, Denoise fill, and is gone: pages are denoised whole
 * now. A patch, a detection's pick or a ceiling saved while it existed still
 * names it, and reads as `fill`, the way the native side reads it
 * (`patch.rs#Engine`). Every other value is returned as it came.
 *
 * @param {string} rung
 * @returns {string}
 */
export function currentRung(rung) {
  return rung === 'denoise' ? 'fill' : rung
}

/**
 * @param {string} rung
 * @returns {string} the rung one step stronger, clamped at the top ('flux')
 */
export function stronger(rung) {
  if (rung === 'cloud') return 'cloud'
  const i = RUNGS.indexOf(currentRung(rung))
  if (i === -1) return rung
  return RUNGS[Math.min(i + 1, RUNGS.length - 1)]
}

/**
 * @param {string} rung
 * @returns {string} the rung one step simpler, clamped at the bottom ('fill')
 */
export function simpler(rung) {
  if (rung === 'cloud') return 'lama'
  const i = RUNGS.indexOf(currentRung(rung))
  if (i === -1) return rung
  return RUNGS[Math.max(i - 1, 0)]
}

/**
 * @param {string} rung
 * @returns {string} i18n key for the rung's display name
 */
export function rungLabel(rung) {
  return LABEL_KEYS[currentRung(rung)] ?? 'ladder.rung.unknown'
}
