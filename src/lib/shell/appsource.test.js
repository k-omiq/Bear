/**
 * A one-line `@type` comment in `App.svelte` types the declaration right
 * under it. One with another comment or declaration between them types that
 * one instead and leaves its own declaration untyped, which no check reports:
 * `NOTICE_ACTIONS` lost its type to `openSummaryOf` that way.
 */
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const source = readFileSync(new URL('../../App.svelte', import.meta.url), 'utf8')

describe('App.svelte', () => {
  it('puts each one-line type comment right above its declaration', () => {
    const lines = source.split('\n')
    const misplaced = lines.flatMap((line, at) => {
      if (!/^\s*\/\*\* @type \{.*\} \*\/\s*$/.test(line)) return []
      const next = lines.slice(at + 1).find((candidate) => candidate.trim() !== '') ?? ''
      return /^\s*(export\s+)?(const|let)\s/.test(next) ? [] : [`${at + 1}: ${next.trim()}`]
    })
    expect(misplaced).toEqual([])
    const typed = lines.findIndex((line) => line.includes('const NOTICE_ACTIONS'))
    expect(lines[typed - 1]).toMatch(/@type \{Record<string, \{labelKey: string/)
  })
})
