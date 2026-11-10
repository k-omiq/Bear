/**
 * The crop window: with "Show each crop before it is sent" on
 * (`nanoBananaShowCrop`), the native side stops right before each Nano
 * Banana request, single region or batch, every retry crop too, and sends a
 * `nano-banana-crop` event with the crop picture. This module opens the
 * window for each one (`dialogs/NanoBananaCropDialog.svelte`) and sends what
 * the person picked: Send, Skip or Stop (`answerNanoBananaCrop`).
 *
 * Started once from `App.svelte`, as `startJobs()` is, since a batch outlives
 * the editor that started it.
 *
 * The native side may stop waiting on its own (Stop in the Layers row, Stop
 * on the batch, cloud engines turned off). It says nothing then, so the
 * answer to a window it gave up on is refused, and that is only the window
 * closing late: there is nothing left to do.
 *
 * A window that goes away unanswered (a route change dismisses the whole
 * modal stack) answers Skip, as Escape does: nothing is sent and a batch
 * goes on.
 */
import { pushModal } from '../state/app.svelte.js'
import { getBackend } from '../api/backend.js'

/** The answers the native side takes. */
export const CROP_ANSWERS = Object.freeze(['send', 'skip', 'stop'])

/** @type {import('../api/backend.js').Backend|null} */
let started = null
/** @type {(() => void)|null} */
let detach = null

/**
 * Follow the event channel and open a window for each crop. Once per
 * backend; answers the teardown.
 *
 * @param {import('../api/backend.js').Backend} [backend]
 * @returns {() => void}
 */
export function startNanoBananaCrops(backend = getBackend()) {
  if (started === backend) return stopNanoBananaCrops
  stopNanoBananaCrops()
  started = backend
  const off = backend.subscribe?.((event) => {
    if (event?.type !== 'nano-banana-crop' || typeof event.id !== 'string') return
    void showCrop(/** @type {import('../api/backend.js').NanoBananaCropEvent} */ (event), backend)
  })
  detach = typeof off === 'function' ? off : null
  return stopNanoBananaCrops
}

/** Stop following the event channel. A window already open stays. */
export function stopNanoBananaCrops() {
  detach?.()
  detach = null
  started = null
}

/**
 * Open the window for one crop and send the answer.
 *
 * @param {import('../api/backend.js').NanoBananaCropEvent} event
 * @param {import('../api/backend.js').Backend} backend
 */
export async function showCrop(event, backend = getBackend()) {
  const picked = /** @type {{ answer?: string, keep?: number[][]|null }|null} */ (await new Promise((resolve) => {
    pushModal({
      kind: 'nanoBananaCrop',
      titleKey: 'nanoBanana.crop.title',
      blocking: true,
      props: { crop: event },
      // The dialog draws its own buttons; these are the way out the modal
      // stack asks of every dialog.
      actions: [
        { id: 'skip', labelKey: 'nanoBanana.crop.skip' },
        { id: 'stop', labelKey: 'nanoBanana.crop.stop' },
        { id: 'send', labelKey: 'nanoBanana.crop.send', variant: 'primary' },
      ],
      onresolve: /** @type {any} */ (resolve),
    })
  }))
  const answer = CROP_ANSWERS.includes(String(picked?.answer)) ? String(picked?.answer) : 'skip'
  const keep = answer === 'send' && event.restrictive && Array.isArray(picked?.keep) && picked.keep.length
    ? picked.keep
    : null
  try {
    await /** @type {any} */ (backend).answerNanoBananaCrop({ id: event.id, answer, keep })
  } catch {
    // The request already ended (stopped elsewhere): the window is closed
    // and nothing waits on this answer.
  }
}
