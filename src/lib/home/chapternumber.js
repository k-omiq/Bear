/**
 * The chapter number as the user types it.
 *
 * A split chapter is numbered `12.5`, `12-1`, `12/2` or `12,5`: runs of digits
 * joined by single `.`, `,`, `/` or `-`. The number field used to take digits
 * only, so none of those could be typed. The same rule is in
 * `src-tauri/src/library.rs#chapter_label`; the backend stores the leading
 * whole number for numbering and keeps the label beside it, and the seam's
 * `chapter.number` is then the label, as typed.
 *
 * Nothing here names a file: a chapter's files are named by its id.
 */

/** The longest label the field takes, as `CHAPTER_LABEL_MAX` in Rust. */
export const CHAPTER_LABEL_MAX = 24

/** Every character the number field refuses as it is typed or pasted. */
export const REJECTED_CHARACTERS = /[^0-9.,/-]/g

const SEPARATOR = /[.,/-]/

/**
 * @typedef {Object} ChapterNumber
 * @property {number} whole - the leading whole number, what `max + 1` counts from
 * @property {string|null} label - the text as typed when it is more than `whole`
 * @property {number[]} parts - every run of digits, for comparing and sorting
 */

/**
 * Read a typed chapter number, or `null` when the rule refuses it.
 *
 * @param {string|number|null|undefined} raw
 * @returns {ChapterNumber|null}
 */
export function parseChapterNumber(raw) {
  if (typeof raw === 'number') {
    return Number.isInteger(raw) && raw >= 0 ? { whole: raw, label: null, parts: [raw] } : null
  }
  const text = String(raw ?? '').trim()
  if (text === '' || text.length > CHAPTER_LABEL_MAX) return null
  if (text.replace(REJECTED_CHARACTERS, '') !== text) return null
  const runs = text.split(SEPARATOR)
  if (runs.some((run) => run === '')) return null
  const parts = runs.map(Number)
  if (parts.some((part) => !Number.isSafeInteger(part))) return null
  return { whole: parts[0], label: runs.length > 1 ? text : null, parts }
}

/**
 * Whether the typed number can be given to a new chapter: the rule takes it,
 * and a plain number is at least 1 (a split one may lead with 0, `0.5`).
 *
 * @param {string} raw
 */
export function isChapterNumber(raw) {
  const parsed = parseChapterNumber(raw)
  return !!parsed && (parsed.whole >= 1 || parsed.label !== null)
}

/**
 * Whether two numbers name the same chapter. `12.5` and `12-5` do: they are
 * two spellings of one split, and a list that held both could not tell them
 * apart. `012` and `12` do too.
 *
 * @param {string|number} a
 * @param {string|number} b
 */
export function sameChapterNumber(a, b) {
  const left = parseChapterNumber(a)
  const right = parseChapterNumber(b)
  if (!left || !right) return String(a) === String(b)
  return left.parts.length === right.parts.length && left.parts.every((part, i) => part === right.parts[i])
}

/**
 * Natural order: 12, 12.5, 12-1 sort as their runs of digits, so 12.5 falls
 * between 12 and 13 and 12.10 after 12.9. A number the rule cannot read sorts
 * after every one it can, by its text.
 *
 * @param {string|number} a
 * @param {string|number} b
 * @returns {number}
 */
export function compareChapterNumbers(a, b) {
  const left = parseChapterNumber(a)
  const right = parseChapterNumber(b)
  if (!left || !right) {
    if (left) return -1
    if (right) return 1
    return String(a).localeCompare(String(b))
  }
  const length = Math.max(left.parts.length, right.parts.length)
  for (let i = 0; i < length; i += 1) {
    // A missing run is less than any run: 12 before 12.0 before 12.1.
    const l = left.parts[i] ?? -1
    const r = right.parts[i] ?? -1
    if (l !== r) return l - r
  }
  return String(a).localeCompare(String(b))
}
