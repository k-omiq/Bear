/**
 * The finishing a layer's record names, as the Layers row and the
 * Post-processing window show it: the recorded one, or the tone fix for a
 * result whose record keeps a tone alignment and no finishing.
 */
import { describe, expect, it } from 'vitest'
import { recordedFinishing } from './maskactions.svelte.js'

/** @param {Record<string, unknown>} snapshot */
const layer = (snapshot) => /** @type {any} */ ({ mask: { provenance: { engine: 'lama', params_snapshot: snapshot } } })

describe('recordedFinishing', () => {
  it('reads the recorded finishing first', () => {
    expect(recordedFinishing(layer({ postprocess: { finishing: 'poisson', grayscale: true }, tone: {} })))
      .toEqual({ finishing: 'poisson', grayscale: true })
  })

  it('reads a tone alignment with no finishing as the tone fix', () => {
    expect(recordedFinishing(layer({ tone: { samples: 7 } }))).toEqual({ finishing: 'tone-fix', grayscale: false })
  })

  it('names none for a record with neither', () => {
    expect(recordedFinishing(layer({}))).toBeNull()
    expect(recordedFinishing(null)).toBeNull()
  })
})
