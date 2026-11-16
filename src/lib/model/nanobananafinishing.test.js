/**
 * The Nano Banana finishing reader takes the names an older settings file or
 * record used, as native's serde aliases do.
 */
import { describe, expect, it } from 'vitest'

import { nanoBananaFinishing, nanoBananaFinishingRefusal, recordedNanoBananaFinishing } from './nanobananafinishing.js'

describe('Nano Banana finishing names', () => {
  it('reads an old name as the one it is now', () => {
    expect(nanoBananaFinishing({ nanoBananaFinishing: 'reference-poisson' }).finishing).toBe('poisson')
    expect(nanoBananaFinishing({ nanoBananaFinishing: 'reference-off' }).finishing).toBe('as-is')
    expect(nanoBananaFinishingRefusal({ nanoBananaFinishing: 'reference-raster' })).toBeNull()
    const provenance = { engine: 'nanobanana', params_snapshot: { postprocess: { finishing: 'reference-local' } } }
    expect(recordedNanoBananaFinishing(provenance)?.finishing).toBe('local-tone')
  })

  it('still refuses a name no build had', () => {
    expect(nanoBananaFinishingRefusal({ nanoBananaFinishing: 'blur' })).not.toBeNull()
    expect(nanoBananaFinishing({ nanoBananaFinishing: 'blur' }).finishing).toBe('tone-fill-dots')
  })
})
