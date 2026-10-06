import { describe, expect, it } from 'vitest'

import { CATALOGUES, LOCALES, flatten } from './index.js'
import { en } from './en.js'

/**
 * Every translated catalogue says the same things as English, in its own words.
 *
 * English is the source. A translation must carry exactly its keys, so a string
 * added to `en.js` is a string owed in every language, and a key English
 * deleted is not kept alive elsewhere. Within an entry the placeholders are the
 * contract the call site relies on: `{count}` renamed or dropped renders the
 * literal braces, and `{cost:currency}` without its format prints a bare
 * number where money belongs.
 */

const PLACEHOLDER = /\{\w+(?::\w+)?\}/g
const DASH = /[—–]/

/** @param {string} text */
function placeholders(text) {
  return new Set(text.match(PLACEHOLDER) ?? [])
}

/** @param {string|Record<string, string>} entry */
function forms(entry) {
  if (typeof entry === 'string') return { other: entry }
  return Object.fromEntries(Object.entries(entry).filter(([name]) => name !== 'select'))
}

/** @param {string|Record<string, string>} entry */
function allPlaceholders(entry) {
  const out = new Set()
  for (const text of Object.values(forms(entry))) for (const token of placeholders(text)) out.add(token)
  return out
}

const SOURCE = flatten(en)
const TRANSLATED = Object.entries(CATALOGUES).filter(([tag]) => tag !== 'en')

it('lists every catalogue once, English first', () => {
  expect(LOCALES.map((locale) => locale.tag)).toEqual(Object.keys(CATALOGUES))
  expect(LOCALES[0].tag).toBe('en')
})

describe.each(TRANSLATED)('the %s catalogue', (tag, catalogue) => {
  const flat = flatten(catalogue)

  it('has exactly the English keys', () => {
    const missing = Object.keys(SOURCE).filter((key) => !Object.hasOwn(flat, key))
    const extra = Object.keys(flat).filter((key) => !Object.hasOwn(SOURCE, key))
    expect(missing, `missing from ${tag}.js:\n  ${missing.join('\n  ')}`).toEqual([])
    expect(extra, `not in en.js:\n  ${extra.join('\n  ')}`).toEqual([])
  })

  it('keeps every placeholder, format included', () => {
    const offenders = []
    for (const [key, source] of Object.entries(SOURCE)) {
      const entry = flat[key]
      if (entry === undefined) continue
      if (typeof source === 'string') {
        const want = [...placeholders(source)].sort().join(' ')
        const got = typeof entry === 'string' ? [...placeholders(entry)].sort().join(' ') : 'plural forms'
        if (want !== got) offenders.push(`${key}: want ${want || 'none'}, got ${got || 'none'}`)
        continue
      }
      if (typeof entry === 'string') {
        offenders.push(`${key}: English has plural forms, ${tag} has a string`)
        continue
      }
      if ((entry.select ?? 'count') !== (source.select ?? 'count')) offenders.push(`${key}: select changed`)
      if (typeof entry.other !== 'string') offenders.push(`${key}: no other form`)
      if (typeof source.zero === 'string' && typeof entry.zero !== 'string') offenders.push(`${key}: no zero form`)
      const allowed = allPlaceholders(source)
      for (const token of allPlaceholders(entry)) {
        if (!allowed.has(token)) offenders.push(`${key}: unknown ${token}`)
      }
      for (const token of placeholders(source.other)) {
        if (!placeholders(entry.other ?? '').has(token)) offenders.push(`${key}: other form lost ${token}`)
      }
    }
    expect(offenders, offenders.join('\n')).toEqual([])
  })

  it('carries no em-dash or en-dash', () => {
    const offenders = Object.entries(flat)
      .flatMap(([key, entry]) => Object.values(forms(entry)).map((text) => [key, text]))
      .filter(([, text]) => DASH.test(text))
      .map(([key, text]) => `  ${key}   ${text}`)
    expect(offenders, offenders.join('\n')).toEqual([])
  })
})
