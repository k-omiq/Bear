/**
 * Finding every i18n key the source uses.
 *
 * This exists for one consumer, `catalogue.test.js`, which asserts the
 * catalogue and the source agree in **both** directions: no key used without an
 * entry, no entry without a use. That second direction is the one that needs a
 * scanner rather than a runtime check - a dead string is invisible at runtime.
 *
 * Pure, string-in / set-out, so it is testable without a filesystem.
 *
 * **The source is two languages.** The Rust core chooses a key for every
 * outcome it reports - every `reasonKey`, `skipReason`, rung and execution
 * provider - and it writes them as string literals exactly as the JavaScript
 * does. Line and block comments are the same syntax in both, and `///` and
 * `//!` fall under the line rule, so the same scanner reads both without a
 * special case. This is what closes the hole
 * from the other side: a Rust string *is* a literal occurrence now.
 *
 * Two deliberate limits, both of which the test compensates for by *listing*
 * what they miss rather than waving it through:
 *
 * 1. Comments are stripped first. A key named in a doc comment is documentation,
 *    not a use, and letting one count would keep a dead key alive forever.
 * 2. Only *literal* keys are found. A key assembled at runtime
 *    (`progress.status.${status}`) has no literal to find, so the test
 *    enumerates those families key by key. See `DYNAMIC_KEYS` there.
 */

/** The first segment of every key. A string that starts with anything else is not a key. */
export const NAMESPACES = Object.freeze([
  'about',
  'accel',
  'app',
  'canvas',
  'cloud',
  'decline',
  'denoise',
  'diagnostics',
  'editor',
  'export',
  'home',
  'input',
  'jobs',
  'ladder',
  'masks',
  'modal',
  'models',
  'notice',
  'onboarding',
  'pipelines',
  'pages',
  'paging',
  'progress',
  'project',
  'qwen',
  'review',
  'settings',
  'shell',
  'shortcuts',
  'time',
  'tools',
  'update',
  'workflow',
])

const NAMESPACE_SET = new Set(NAMESPACES)

/**
 * Strip comments so a key mentioned in prose does not count as a use.
 *
 * `//` is only treated as a line comment when it is not preceded by `:` - which
 * is what keeps `https://…` inside a string literal intact. That is a heuristic,
 * not a parser, and it is the right trade here: the failure mode is *dropping*
 * text, which can only ever hide a use, and a hidden use fails the test loudly
 * (a key with no entry) rather than passing it quietly.
 *
 * @param {string} source
 * @returns {string}
 */
export function stripComments(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, ' ')
    .replace(/<!--[\s\S]*?-->/g, ' ')
    .replace(/(^|[^:])\/\/[^\n]*/g, '$1')
}

/**
 * The one file whose `id: '…'` values look like keys and are not.
 *
 * `src/lib/shortcuts.js` names its entries `app.export`, `review.next` and so
 * on - dotted, lower-camel, and starting with a namespace this scanner knows.
 * They are identifiers; every one of them carries a `labelKey` alongside that
 * *is* the key.
 *
 * The strip is scoped to that file rather than applied namespace-blind,
 * because `id` is a common property name and a genuine key written as an `id`
 * value anywhere else would have vanished with it - which is the one failure
 * this scanner must never have, since a vanished use makes a dead entry look
 * alive. If another file ever adopts dotted ids, the test fails loudly with a
 * list of entries it cannot find, and this predicate is where to widen.
 */
const STRIPS_IDS = /(^|[\\/])shortcuts\.js$/

/**
 * Drop `id: '…'` before scanning. Only called for `STRIPS_IDS` paths.
 *
 * @param {string} source
 * @returns {string}
 */
export function stripIds(source) {
  return source.replace(/\bid:\s*(['"`])[^'"`]*\1/g, ' ')
}

/**
 * A key: lower-camel segments, at least two of them, in any of the three
 * string quotes. Single quotes are this codebase's style and every key in it
 * is written that way today - but nothing enforces that, and a scanner that
 * cannot see `t("a.b.c")` would let a missing catalogue entry pass green and
 * render `[a.b.c]` at runtime. A backtick template that interpolates is not
 * matched by construction: `${` holds characters this pattern excludes.
 */
const KEY = /(['"`])([a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9]+)+)\1/g

/**
 * Dotted strings that are file names rather than keys, **listed one by one**.
 *
 * `NAMESPACES` rejects `desktop.ini`, `notes.txt` and `onnxruntime.dll` for
 * free, because none of them begins with a namespace. `settings.json` does
 * begin with one, and it is the real name of a real file
 * (`src-tauri/src/settings.rs`), so it has to be named here.
 *
 * A general rule was tried first - "a last segment that is a file extension is
 * not a key" - and it is **wrong**: `export.format.png` is a key, the scan
 * dropped it, and a dropped use makes a live entry look dead. The test said so
 * on the first run. Hence a literal list, for the same reason the test
 * enumerates its runtime keys rather than allowing a prefix: the failure this
 * scanner must never have is a vanished use, and a pattern is how one vanishes.
 */
const NOT_KEYS = new Set([
  'settings.json',
  'input.png',
  // The gateway's operation names (`src-tauri/src/inference/http.rs`). They begin
  // with `jobs`, the Jobs list's namespace, and are not keys.
  'jobs.submit',
  'jobs.status',
  'jobs.result',
  'jobs.cancel',
])

/**
 * Every literal i18n key in one file's source.
 *
 * @param {string} source
 * @param {string} [path] - the file's path; selects the `id:` strip
 * @returns {Set<string>}
 */
export function keysIn(source, path = '') {
  const found = new Set()
  const stripped = stripComments(source)
  const text = STRIPS_IDS.test(path) ? stripIds(stripped) : stripped
  for (const match of text.matchAll(KEY)) {
    const key = match[2]
    if (!NAMESPACE_SET.has(key.slice(0, key.indexOf('.')))) continue
    if (NOT_KEYS.has(key)) continue
    found.add(key)
  }
  return found
}

/**
 * Every literal i18n key across a set of files.
 *
 * @param {Iterable<{path: string, source: string}>} files
 * @returns {Map<string, string[]>} key → the paths that use it
 */
export function keyUses(files) {
  /** @type {Map<string, string[]>} */
  const uses = new Map()
  for (const file of files) {
    for (const key of keysIn(file.source, file.path)) {
      const paths = uses.get(key)
      if (paths) paths.push(file.path)
      else uses.set(key, [file.path])
    }
  }
  return uses
}
