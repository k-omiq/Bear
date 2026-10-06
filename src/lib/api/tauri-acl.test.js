import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const tauri = new URL('../../../src-tauri/', import.meta.url)
const read = (path) => readFileSync(new URL(path, tauri), 'utf8')
const quotedNames = (source) => [...source.matchAll(/"([a-z][a-z0-9_]*)"/g)].map((match) => match[1])

/** Tauri checks both the build manifest and a window capability before dispatching. */
describe('Tauri command ACL', () => {
  const handlers = read('src/lib.rs').match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1]
  const registered = [...(handlers ?? '').matchAll(/\b\w+(?:::\w+)*::([a-z][a-z0-9_]*)\b/g)]
    .map((match) => match[1])
  const manifest = read('build.rs').match(/\.commands\(&\[([\s\S]*?)\]\)/)?.[1]
  const built = quotedNames(manifest ?? '')
  const permissions = read('permissions/cloud.toml')
  const allowed = [...permissions.matchAll(/commands\.allow\s*=\s*\[([\s\S]*?)\]/g)]
    .flatMap((match) => quotedNames(match[1]))

  it('lists every registered command in the build manifest', () => {
    expect(registered.length).toBeGreaterThan(0)
    expect(built.sort()).toEqual([...registered].sort())
  })

  it('allows every registered command in exactly one permission', () => {
    expect(allowed.sort()).toEqual([...registered].sort())
  })

  it('grants those permissions to the main window', () => {
    const permissionIds = [...permissions.matchAll(/\[\[permission\]\]\s*identifier\s*=\s*"([^"]+)"/g)]
      .map((match) => match[1])
    const capabilities = ['default', 'cloud'].map((name) => JSON.parse(read(`capabilities/${name}.json`)))
    const granted = new Set(capabilities.filter((capability) => capability.windows.includes('main'))
      .flatMap((capability) => capability.permissions)
      .filter((entry) => typeof entry === 'string'))
    expect(permissionIds.every((id) => granted.has(id))).toBe(true)
  })
})
