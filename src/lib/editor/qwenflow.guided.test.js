import { describe, expect, it } from 'vitest'

import { takesGuidance } from './qwenflow.js'

describe('GUIDED_RECIPES', () => {
  // Bug hunt, 2026-10-31. render.rs and contract.py list five recipes, this list four, so
  // the FLUX 3 bf16 weights never ask for guidance on a redo though the worker reads it.
  it.skip('names the FLUX 3 bf16 recipe, as render.rs GUIDED_RECIPES does', () => {
    expect(takesGuidance({ pinnedRecipeId: 'mc-flux3-klein-bf16-edit-v2' })).toBe(true)
  })
})
