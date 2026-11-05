/**
 * The engine ladder: fill → screentone → manga-LaMa → FLUX, ordered
 * weakest/cheapest to strongest/most expensive. Screentone is arithmetic like
 * fill, made for lettering on a halftone dot screen; picked by hand only. `stronger`/`simpler` step along it and clamp at
 * both ends - there is no engine below fill or above FLUX.
 *
 * `decline` is a terminal pipeline outcome, not a
 * rung to escalate to or from; it lives on `Region.outcome`, not here.
 */

/** @type {ReadonlyArray<string>} */
export const RUNGS = Object.freeze(['fill', 'screentone', 'lama', 'flux'])

const LABEL_KEYS = Object.freeze({
  fill: 'ladder.rung.fill',
  screentone: 'ladder.rung.screentone',
  lama: 'ladder.rung.lama',
  flux: 'ladder.rung.flux',
  cloud: 'ladder.rung.cloud',
  nanobanana: 'ladder.rung.nanobanana',
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
 * The strongest rung this machine runs: FLUX where its sidecar can be
 * offered (`available.flux`, `capabilities.engines`), LaMa otherwise. The
 * native side's `region.rs#strongest_here`, which reads the same answer. With
 * no map, FLUX is not assumed: the sidecar is absent on nearly every machine
 * (`masks.js#UNANSWERED`).
 *
 * @param {Record<string, boolean>} [available]
 * @returns {string}
 */
export function strongestHere(available) {
  return available?.flux === true ? 'flux' : 'lama'
}

/**
 * @param {string} rung
 * @param {Record<string, boolean>} [available] - `capabilities.engines`; read only for a Nano Banana layer
 * @returns {string} the rung one step stronger, clamped at the top ('flux')
 */
export function stronger(rung, available) {
  if (rung === 'cloud') return 'cloud'
  // Never onto Nano Banana and never staying on it: each of its renders is a
  // paid request a person asks for by name. It steps to the strongest rung
  // this machine runs, not to a FLUX it does not have (`region.rs#step`).
  if (rung === 'nanobanana') return strongestHere(available)
  const i = RUNGS.indexOf(currentRung(rung))
  if (i === -1) return rung
  return RUNGS[Math.min(i + 1, RUNGS.length - 1)]
}

/**
 * @param {string} rung
 * @param {Record<string, boolean>} [available] - `capabilities.engines`; read only for a Nano Banana layer
 * @returns {string} the rung one step simpler, clamped at the bottom ('fill')
 */
export function simpler(rung, available) {
  if (rung === 'cloud') return 'lama'
  if (rung === 'nanobanana') return strongestHere(available)
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
