import { describe, expect, it } from 'vitest'

import { takesGuidance } from './qwenflow.js'

describe('GUIDED_RECIPES', () => {
  it('names the FLUX 3 bf16 recipe, as render.rs GUIDED_RECIPES does', () => {
    expect(takesGuidance({ pinnedRecipeId: 'mc-flux3-klein-bf16-edit-v2' })).toBe(true)
  })
})
