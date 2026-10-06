/**
 * Application-level shell state: where we are, what is blocking, what is being
 * announced. Nothing project-specific lives here - the editor owns that.
 *
 * Route model
 * -----------
 * Home is a two-level hierarchy, Projects → Chapters (constraints.md ruling 1),
 * so the brief's `home | editor` is expressed as three route names:
 *
 *   library   the project grid, the root
 *   chapters  one project's chapter list
 *   editor    one chapter open in the editor
 *
 * `back()` walks that hierarchy: the editor returns to *its project's* chapter
 * list, not to the library root. (The `H` shortcut is the separate "go all the
 * way home" affordance.)
 *
 * Modal stack
 * -----------
 * A stack, not a slot, because a dialog can raise another and has to be found
 * again underneath it: the Export dialog raises the overwrite refusal, and
 * answering the refusal reveals Export with its destination put right. Task 4's
 * `Modal` handles Escape on `window` and is mount-driven, so **only the top of
 * the stack is ever mounted** - see `src/lib/shell/ModalHost.svelte`.
 */

import { createIdFactory } from '../model/ids.js'

const nextModalId = createIdFactory('modal')
const nextNoticeId = createIdFactory('notice')

export const ROUTES = /** @type {const} */ (['library', 'chapters', 'editor'])

/** More than this on screen at once is noise, not information. Oldest goes. */
export const MAX_NOTICES = 4

/**
 * @typedef {Object} Route
 * @property {'library'|'chapters'|'editor'} name
 * @property {string|null} projectId - set on `chapters` and `editor`
 * @property {string|null} chapterId - set on `editor`
 */

/**
 * One entry on the modal stack. `kind` selects the dialog; Task 11 owns the
 * bodies. `actions` are the buttons - selecting one resolves the modal with
 * that action's id.
 *
 * **Invariant: a dialog always has a way out.** Either `dismissable` is true
 * (Escape and the backdrop close it) or `actions` is non-empty. A
 * non-dismissable dialog with no buttons would be a dead end with no keyboard
 * route out of it, so `pushModal`/`replaceModal` reject that combination
 * rather than mounting it - see `normalizeModal`.
 *
 * @typedef {Object} ModalSpec
 * @property {string} id - minted by `pushModal`
 * @property {string} kind
 * @property {string} titleKey
 * @property {Record<string, unknown>} props - whatever the dialog body needs
 * @property {Array<{id: string, labelKey: string, variant?: 'primary'|'ghost'}>} actions
 * @property {boolean} blocking - backdrop clicks are ignored (cloud cost, format conversion)
 * @property {boolean} dismissable - false makes Escape and the backdrop inert (overwrite refusal)
 * @property {((result: string|null) => void)|null} onresolve
 */

/**
 * A queued notice, still in i18n form - `key` + `params`, never English.
 * `App.svelte` translates at render time.
 *
 * @typedef {Object} NoticeSpec
 * @property {string} id
 * @property {string} key
 * @property {Record<string, unknown>} params
 * @property {'info'|'warn'} tone
 * @property {string} [icon]
 * @property {number} [duration]
 */

export const app = $state({
  /** @type {Route} */
  route: { name: 'library', projectId: null, chapterId: null },
  /** @type {ModalSpec[]} */
  modals: [],
  /** @type {NoticeSpec[]} */
  notices: [],
})

/* ------------------------------------------------------------------ */
/* Routing                                                             */
/* ------------------------------------------------------------------ */

/**
 * The one place the route is assigned.
 *
 * A dialog belongs to the screen that opened it, so **a route change dismisses
 * the whole modal stack**. Without this a dialog pushed on one screen can
 * outlive it and end up mounted over an unrelated one - reachable whenever a
 * dialog's own action navigates instead of merely closing itself, which is
 * exactly what Task 11's New project and resume-job flows do.
 *
 * The route is written *before* the stack is dismissed, so an `onresolve` that
 * navigates in turn finds the route already settled and stops here rather than
 * recursing.
 *
 * @param {Route} next
 */
function setRoute(next) {
  const current = app.route
  if (
    current.name === next.name &&
    current.projectId === next.projectId &&
    current.chapterId === next.chapterId
  ) {
    return
  }
  app.route = next
  closeAllModals()
}

export function goLibrary() {
  setRoute({ name: 'library', projectId: null, chapterId: null })
}

/** @param {string} projectId */
export function openProject(projectId) {
  if (!projectId) return
  setRoute({ name: 'chapters', projectId, chapterId: null })
}

/**
 * @param {string} projectId
 * @param {string} chapterId
 */
export function openChapter(projectId, chapterId) {
  if (!projectId || !chapterId) return
  setRoute({ name: 'editor', projectId, chapterId })
}

/**
 * One step up the hierarchy. From the editor that is the project's chapter
 * list - the back path a scanlator working through a project expects, and not
 * the library root.
 */
export function back() {
  if (app.route.name === 'editor' && app.route.projectId) {
    openProject(app.route.projectId)
    return
  }
  goLibrary()
}

/* ------------------------------------------------------------------ */
/* Modal stack                                                         */
/* ------------------------------------------------------------------ */

/**
 * Widths from `constraints.md`'s metrics table. Everything else is 400.
 *
 * `about` is gone: About is a section inside Settings, not a kind of its own,
 * and an entry no `pushModal` can reach is a value that outlives its meaning.
 *
 * `shortcuts` at 460 is a departure from the table's "else 400", recorded in
 * `constraints.md` beside the table: the design file has no standalone
 * shortcut dialog to take a number from - it draws the sheet only inside
 * Settings, at 520 - and the sheet is a two-column `dl` whose descriptions
 * wrap onto three lines at 400.
 */
const MODAL_WIDTHS = {
  // No `settings`: Settings is a full-window `Screen`, not a `Modal`.
  export: 460,
  shortcuts: 460,
  newProject: 440,
}

/**
 * @param {string} kind
 * @returns {number}
 */
export function modalWidth(kind) {
  return MODAL_WIDTHS[kind] ?? 400
}

/**
 * Fill a spec out, enforcing the way-out invariant.
 *
 * A dismissable dialog with no `actions` gets a single Close button. A
 * **non-dismissable** one must supply its own, and this throws if it does not:
 * the only shape that reaches that branch is the overwrite refusal, whose
 * whole point is that the way out is a specific, named choice -
 * "Choose another folder…" - and not a generic dismissal. Substituting a
 * Close button would silently turn a refusal into the dismissable dialog
 * that rule forbids, so there is no safe default to fall back to.
 *
 * It throws rather than warning because the combination cannot arise from user
 * input or backend data - only from a bad call site - and because the failure
 * mode of throwing (the dialog does not open, the console says why) is far
 * better than the failure mode of mounting it (the user is trapped in a dialog
 * with no keyboard route out).
 *
 * @param {Partial<ModalSpec> & {kind: string}} spec
 * @returns {ModalSpec}
 * @throws {TypeError} when `dismissable` is false and `actions` is empty
 */
function normalizeModal(spec) {
  const kind = spec.kind
  const dismissable = spec.dismissable ?? true
  const actions = spec.actions ?? (dismissable ? [{ id: 'close', labelKey: 'shell.action.close' }] : [])

  if (!dismissable && actions.length === 0) {
    throw new TypeError(
      `Modal "${kind}" is not dismissable and has no actions - it would have no way out. ` +
        'Give it at least one action, or make it dismissable.'
    )
  }

  return {
    id: nextModalId(),
    kind,
    titleKey: spec.titleKey ?? `modal.title.${kind}`,
    props: spec.props ?? {},
    actions,
    blocking: spec.blocking ?? false,
    dismissable,
    onresolve: spec.onresolve ?? null,
  }
}

/**
 * Push a dialog. The pushed dialog becomes the mounted one; anything beneath it
 * stays on the stack and reappears when this one closes.
 *
 * @param {Partial<ModalSpec> & {kind: string}} spec
 * @returns {string} the modal's id
 */
export function pushModal(spec) {
  const modal = normalizeModal(spec)
  app.modals.push(modal)
  return modal.id
}

/**
 * Swap the top of the stack for another dialog. The replaced dialog does *not*
 * resolve; whatever raised it carries on in the new one.
 *
 * **Not for a flow that waits on an answer.** `closeModal` pops the stack
 * before the resolved promise continues, so by the time a flow reads one
 * dialog's answer that dialog is already gone, and a replace would overwrite
 * whatever was underneath. Such a flow pushes its next dialog instead. The
 * cloud asks one question per action, the consent dialog, pushed by
 * `src/lib/editor/cloudflow.svelte.js#requestCloudConsent`. A seamless swap
 * would need `closeModal` to resolve *after* the replacement, which is a
 * change to this module's semantics that nothing has yet needed.
 *
 * @param {Partial<ModalSpec> & {kind: string}} spec
 * @returns {string} the new modal's id
 */
export function replaceModal(spec) {
  const modal = normalizeModal(spec)
  if (app.modals.length === 0) app.modals.push(modal)
  else app.modals[app.modals.length - 1] = modal
  return modal.id
}

/**
 * Resolve and pop the top dialog.
 *
 * @param {string|null} [result] - the id of the action taken, or null for a dismissal
 */
export function closeModal(result = null) {
  const modal = app.modals.pop()
  modal?.onresolve?.(result)
}

/**
 * Dismiss the whole stack. Every dropped dialog resolves with `null` - the
 * same value Escape and a backdrop click produce - so a caller awaiting an
 * answer is told the dialog went away unanswered rather than being left
 * waiting forever.
 *
 * The array is detached before anything is resolved, so an `onresolve` that
 * pushes a new dialog keeps it.
 *
 * Called by `setRoute` on every route change, and available to Tasks 7 to 11 for
 * anything else that invalidates the whole stack.
 */
export function closeAllModals() {
  if (app.modals.length === 0) return
  const dropped = app.modals.slice()
  app.modals.length = 0
  for (let i = dropped.length - 1; i >= 0; i -= 1) dropped[i].onresolve?.(null)
}

/** @returns {ModalSpec|null} the only dialog that may be mounted */
export function activeModal() {
  return app.modals.length ? app.modals[app.modals.length - 1] : null
}

/** @returns {boolean} */
export function isModalOpen() {
  return app.modals.length > 0
}

/* ------------------------------------------------------------------ */
/* Notice queue                                                        */
/* ------------------------------------------------------------------ */

/**
 * Queue a notice. Takes i18n data, never text - the backend's `notice` events
 * are forwarded here verbatim.
 *
 * @param {{key: string, params?: Record<string, unknown>, tone?: 'info'|'warn', icon?: string, duration?: number}} spec
 * @returns {string} the notice's id
 */
export function notify(spec) {
  const notice = {
    id: nextNoticeId(),
    key: spec.key,
    params: spec.params ?? {},
    tone: spec.tone ?? 'info',
    icon: spec.icon,
    duration: spec.duration,
  }
  app.notices.push(notice)
  while (app.notices.length > MAX_NOTICES) app.notices.shift()
  return notice.id
}

/** @param {string} id */
export function dismissNotice(id) {
  const index = app.notices.findIndex((notice) => notice.id === id)
  if (index >= 0) app.notices.splice(index, 1)
}

export function clearNotices() {
  app.notices.length = 0
}
