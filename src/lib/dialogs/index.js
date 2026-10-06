/**
 * The dialog registry: modal `kind` → the component that draws it.
 *
 * `src/lib/shell/ModalHost.svelte` asks `dialogFor(spec.kind)` and mounts what
 * it gets; a kind with no entry falls through to the host's generic dialog -
 * a title, a line of copy (`props.bodyKey`) and the spec's action buttons -
 * which is all a confirmation or a refusal needs. So there is no `{#if}` chain
 * to grow: a new dialog is a file and a line in this table.
 *
 * Home's four were registered in `src/lib/home/dialogs/index.js` before this
 * module existed and stay there, next to the screen whose state they mutate.
 * They are folded in here so the host has **one** registry to consult rather
 * than two to merge.
 *
 * Kinds that deliberately have no component, and why:
 *
 * - `overwriteRefusal` - a statement and one named action. The generic dialog
 *   renders it exactly, and `dismissable: false` is what makes it a refusal
 *   rather than a warning.
 * - `removeProject` - the same shape, from `src/lib/home/actions.js`.
 */

import * as homeDialogs from '../home/dialogs/index.js'
import QwenPromptDialog from './QwenPromptDialog.svelte'
import QwenReviewDialog from './QwenReviewDialog.svelte'
import SettingsDialog from './SettingsDialog.svelte'
import ShortcutsDialog from './ShortcutsDialog.svelte'
import ExportDialog from './ExportDialog.svelte'
import FormatConversionDialog from './FormatConversionDialog.svelte'
import CloudConsentDialog from './CloudConsentDialog.svelte'
import CloudRunConsentDialog from './CloudRunConsentDialog.svelte'
import CloudCleanConsentDialog from './CloudCleanConsentDialog.svelte'
import WorkflowReviewDialog from './WorkflowReviewDialog.svelte'
import DenoiseDialog from './DenoiseDialog.svelte'
import DenoiseCompareDialog from './DenoiseCompareDialog.svelte'

/**
 * Null-prototype on purpose. `kind` is a plain string that reaches this table
 * from a `pushModal` call site, and a plain object would answer `'constructor'`
 * or `'toString'` with something that is not a component.
 */
const DIALOGS = Object.assign(Object.create(null), homeDialogs, {
  qwenPrompt: QwenPromptDialog,
  qwenReview: QwenReviewDialog,
  settings: SettingsDialog,
  shortcuts: ShortcutsDialog,
  export: ExportDialog,
  formatConversion: FormatConversionDialog,
  cloudConsent: CloudConsentDialog,
  cloudRunConsent: CloudRunConsentDialog,
  cloudCleanConsent: CloudCleanConsentDialog,
  workflowReview: WorkflowReviewDialog,
  denoise: DenoiseDialog,
  denoiseCompare: DenoiseCompareDialog,
})

/**
 * @param {string} kind
 * @returns {any} the component for this kind, or null for the generic dialog
 */
export function dialogFor(kind) {
  return (typeof kind === 'string' && DIALOGS[kind]) || null
}
