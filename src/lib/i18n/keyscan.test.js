import { describe, expect, it } from 'vitest'

import { keysIn, stripComments, stripIds } from './keyscan.js'

describe('finding keys in source', () => {
  it('finds a literal key in any of the three quotes', () => {
    const found = keysIn(`t('a.b'); t("masks.origin.hand"); t(\`pages.status.cleaned\`)`)
    expect([...found].sort()).toEqual(['masks.origin.hand', 'pages.status.cleaned'])
  })

  it('ignores a key named in a comment', () => {
    expect(keysIn(`// t('masks.origin.hand')`).size).toBe(0)
    expect(keysIn(`/* 'masks.origin.hand' */`).size).toBe(0)
  })

  /** Rust's `///` and `//!` are line comments, which is why one scanner reads both languages. */
  it('ignores a key named in a Rust doc comment', () => {
    expect(keysIn(`//! see "masks.origin.hand"\n/// and "pages.status.cleaned"`).size).toBe(0)
    expect(keysIn(`let key = "masks.origin.hand";`).size).toBe(1)
  })

  it('drops dotted ids in the one file that has them', () => {
    const source = `{ id: 'app.export', labelKey: 'editor.action.export' }`
    expect([...keysIn(source, 'lib/shortcuts.js')]).toEqual(['editor.action.export'])
    expect([...keysIn(source, 'lib/other.js')].sort()).toEqual(['app.export', 'editor.action.export'])
  })

  /**
   * A file name that begins with a namespace. The general rule this replaced -
   * "a last segment that is a file extension is not a key" - dropped
   * `export.format.png`, which is a key, and a dropped use makes a live
   * catalogue entry look dead.
   */
  it('a file name that looks like a key is listed rather than pattern-matched', () => {
    expect(keysIn(`const FILE: &str = "settings.json";`).size).toBe(0)
    expect(keysIn(`saved.join("input.png")`).size).toBe(0)
    expect([...keysIn(`t('export.format.png')`)]).toEqual(['export.format.png'])
    expect([...keysIn(`t('settings.models.status.installed')`)]).toEqual([
      'settings.models.status.installed',
    ])
  })

  it('leaves a URL inside a string alone', () => {
    expect(stripComments(`const u = 'https://example.invalid/x'`)).toContain('https://example')
  })

  it('strips only id values, not the keys beside them', () => {
    expect(stripIds(`{ id: 'a.b', labelKey: 'c.d' }`)).toContain('c.d')
    expect(stripIds(`{ id: 'a.b', labelKey: 'c.d' }`)).not.toContain('a.b')
  })
})
