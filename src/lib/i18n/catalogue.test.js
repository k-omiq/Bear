import { readdirSync, readFileSync, statSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, relative } from 'node:path'
import { describe, expect, it } from 'vitest'

import { catalogueKeys } from './index.js'
import { keyUses } from './keyscan.js'
import { SHORTCUT_GROUPS } from '../shortcuts.js'
import { ROW_ENGINES, engineChoiceLabel, reviewEntryActions } from '../model/masks.js'
import { RUNGS } from '../model/ladder.js'

/**
 * The catalogue and the source must agree in **both** directions:
 *
 * 1. every key the app renders has an entry - otherwise a screen shows `[key]`;
 * 2. every entry is rendered by something - otherwise the catalogue accumulates
 *    dead strings that a translator pays to translate and nobody ever reads.
 *
 * Direction 2 is the one that needs a source scan, and the scan can only see
 * *literal* keys. A key assembled at runtime has no literal to find, so it is
 * listed below - and **listed key by key, never as a wildcard prefix**. A
 * wildcard is exactly the hole a dead key survives through: allow
 * `progress.status.*` and `progress.status.abandoned` lives forever.
 *
 * Two of the families are computed from the code that owns the enumeration
 * rather than typed out, so they cannot drift from it at all.
 *
 * ## Both languages
 *
 * A use is a literal occurrence in any shipped `.js`, `.svelte` **or `.rs`**
 * file. The Rust core chooses a key for every outcome it reports, and until it
 * was scanned a key it named and the catalogue lacked rendered `[key]` at
 * runtime with this test green - the hole closed by widening the scan
 * rather than by maintaining a second list by hand.
 *
 * ## What green still does *not* mean
 *
 * The mock adapter is a shipped file, so an entry named only by
 * `src/lib/api/fixtures.js` or `src/lib/api/mock.js` counts as used even
 * though the only thing rendering it is fixture data. Today that is
 * `about.fact.*`, `time.relative.*` and three `notice.input.*` - the mock is
 * still the fallback backend, so they are alive; **delete the mock and every
 * one of them goes dead with this test green.** (`decline.reason.qualityMetric`
 * and `input.skipReason.*` left that list when the Rust scan began: the core
 * names them.)
 *
 * That is a real limit of scanning rather than a bug to engineer away - a
 * scanner cannot tell a fixture that names a key from a component that
 * renders one, and a rule excluding `src/lib/api` would be wrong the moment
 * the adapter legitimately reports a key. It is written down here so that
 * whoever finally removes the mock knows to re-check these three families by
 * hand at that point, which is the one moment it matters.
 */

const HERE = dirname(fileURLToPath(import.meta.url))
const SRC = join(HERE, '../../')

/** Keys built at runtime, with the line that builds each one. */
const DYNAMIC_KEYS = [
  // `src/lib/model/progress.js` - `progress.status.${status}`, over the closed
  // four-value union in the `Progress` typedef.
  'progress.status.notStarted',
  'progress.status.inProgress',
  'progress.status.review',
  'progress.status.completed',

  // `src/lib/editor/ClusterLeft.svelte` - `editor.direction.${readingDirection()}`.
  'editor.direction.rtl',
  'editor.direction.ltr',

  // `src/lib/state/app.svelte.js#normalizeModal` - `modal.title.${kind}` is the
  // default title, so every kind pushed *without* a `titleKey` needs one entry.
  // The kinds that pass an explicit `titleKey` appear as literals and are not
  // listed here.
  'modal.title.newProject', //    src/lib/home/actions.js, src/lib/shortcuts.js
  'modal.title.newChapter', //    src/lib/home/actions.js
  'modal.title.deleteChapter', // src/lib/home/actions.js
  'modal.title.renameProject', // src/lib/home/actions.js
  'modal.title.removeProject', // src/lib/home/actions.js
  'modal.title.settings', //      src/lib/home/HomeHeader.svelte, ClusterRight.svelte, shortcuts.js
  'modal.title.export', //        src/lib/editor/ClusterRight.svelte, shortcuts.js
  'modal.title.openProject', //   src/lib/shortcuts.js
  'modal.title.shortcuts', //     src/lib/shortcuts.js
]

/** `src/lib/dialogs/ShortcutSheet.svelte` - one heading per group. */
const GROUP_KEYS = SHORTCUT_GROUPS.map((group) => `shortcuts.group.${group}`)

/**
 * `src/lib/editor/maskrows.js#actionHint` - `masks.hint.${actionId}` over the
 * action ids `src/lib/model/masks.js` emits. Only an unmasked review entry
 * carries actions built this way; a detected region's actions and a mask's
 * controls name their own keys as literals.
 */
const HINT_KEYS = reviewEntryActions({ outcome: 'gate-skipped' }).map(
  (action) => `masks.hint.${action.id}`,
)

/**
 * `src/lib/model/masks.js#engineChoiceLabel` - the Layers row's engine picker.
 * Over every rung, not only `ROW_ENGINES`: a mask that ran on a rung the picker
 * does not offer still names its own entry, which is the `cloud` case.
 */
const ENGINE_CHOICE_KEYS = [...new Set([...RUNGS, ...ROW_ENGINES, 'cloud'])].map(engineChoiceLabel)

const RUNTIME_KEYS = [...DYNAMIC_KEYS, ...GROUP_KEYS, ...HINT_KEYS, ...ENGINE_CHOICE_KEYS]

/** Every source file the app ships, in either language. The catalogue's own directory and JS tests excluded. */
function sourceFiles(dir, out = []) {
  for (const name of readdirSync(dir)) {
    if (name === 'target' || name === 'node_modules' || name === 'gen') continue
    const path = join(dir, name)
    if (statSync(path).isDirectory()) sourceFiles(path, out)
    else if (/\.(js|svelte|rs)$/.test(name) && !name.endsWith('.test.js')) out.push(path)
  }
  return out
}

/**
 * The Rust core and the Tauri adapter are shipped source too, and they are
 * where every `reasonKey`, `skipReason`, rung name and execution provider is
 * chosen. Scanning them is what makes direction 1 hold for keys the backend
 * emits - until this, a key a Rust string named and the catalogue lacked would
 * render `[key]` at runtime with this test green.
 */
const RUST_ROOTS = [join(SRC, '../crates'), join(SRC, '../src-tauri/src')]

const FILES = [SRC, ...RUST_ROOTS]
  .flatMap((root) => sourceFiles(root))
  .filter((path) => !path.startsWith(join(SRC, 'lib/i18n')))
  .map((path) => ({ path: relative(SRC, path), source: readFileSync(path, 'utf8') }))

const USES = keyUses(FILES)
// `cloud_job_wire::JobKind::capability` names an HTTP capability, not UI copy.
// Its namespace overlaps the denoise catalogue; exclude this exact wire identifier.
const WIRE_IDENTIFIERS = new Set(['denoise.jobs'])
const USED = new Set([...USES.keys(), ...RUNTIME_KEYS].filter((key) => !WIRE_IDENTIFIERS.has(key)))
const CATALOGUE = new Set(catalogueKeys())

describe('the catalogue is complete', () => {
  it('reads every source file', () => {
    // A silent zero here would make both assertions below pass vacuously.
    expect(FILES.length).toBeGreaterThan(60)
    expect(USES.size).toBeGreaterThan(300)
  })

  // The Rust half specifically: a moved crate or a renamed directory would
  // drop it, and every assertion here would go on passing - with the backend's
  // keys unchecked again.
  it('reads the Rust source too, and finds keys in it', () => {
    const rust = FILES.filter((file) => file.path.endsWith('.rs'))
    expect(rust.length).toBeGreaterThan(15)
    const fromRust = [...USES].filter(([, paths]) => paths.some((p) => p.endsWith('.rs')))
    expect(fromRust.length).toBeGreaterThan(20)
  })

  it('has an entry for every key the source uses', () => {
    const missing = [...USED].filter((key) => !CATALOGUE.has(key)).sort()
    expect(missing, `used but not in the catalogue:\n${format(missing, USES)}`).toEqual([])
  })

  it('has no entry the source never uses', () => {
    const dead = [...CATALOGUE].filter((key) => !USED.has(key)).sort()
    expect(dead, `in the catalogue but never rendered:\n${dead.join('\n')}`).toEqual([])
  })

  it('lists every runtime-built key individually, never as a prefix', () => {
    for (const key of RUNTIME_KEYS) {
      expect(key, `"${key}" is not a whole key`).toMatch(/^[a-z][a-zA-Z0-9]*(\.[a-zA-Z0-9]+)+$/)
      expect(key).not.toContain('*')
    }
    // The whole runtime list stays small enough to read. If it grows, the app
    // has started assembling keys where it should be choosing between them.
    expect(RUNTIME_KEYS.length).toBeLessThan(30)
  })
})

/**
 * @param {string[]} keys
 * @param {Map<string, string[]>} uses
 */
function format(keys, uses) {
  return keys.map((key) => `  ${key}   ${(uses.get(key) ?? ['(runtime)']).join(', ')}`).join('\n')
}
