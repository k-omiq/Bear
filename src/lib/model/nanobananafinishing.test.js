/**
 * A Nano Banana layer's record is read under the names it has now, as
 * native's serde aliases read it, and only a Nano Banana layer has one.
 */
import { describe, expect, it } from 'vitest'

import { recordedNanoBananaFinishing } from './nanobananafinishing.js'

describe('Nano Banana recorded finishing', () => {
  it('reads an old name as the one it is now', () => {
    const provenance = { engine: 'nanobanana', params_snapshot: { postprocess: { finishing: 'reference-local' } } }
    expect(recordedNanoBananaFinishing(provenance)?.finishing).toBe('local-tone')
  })

  it('keeps the stray choice a layer was made with', () => {
    const provenance = { engine: 'nanobanana', params_snapshot: { postprocess: { finishing: 'copy', grayscale: true, strays: true } } }
    expect(recordedNanoBananaFinishing(provenance)).toEqual({ finishing: 'copy', grayscale: true, strays: true })
  })

  it('is none for another engine or a layer with no record', () => {
    expect(recordedNanoBananaFinishing({ engine: 'lama', params_snapshot: { postprocess: { finishing: 'copy' } } })).toBeNull()
    expect(recordedNanoBananaFinishing({ engine: 'nanobanana', params_snapshot: {} })).toBeNull()
  })
})
