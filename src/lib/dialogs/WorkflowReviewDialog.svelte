<script>
  import { untrack } from 'svelte'
  import { Modal, Button } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { runDetection, workflowForDetectorModels } from '../model/pipelines.js'
  import { t } from '../i18n/index.js'
  import WorkflowAnalysis from './WorkflowAnalysis.svelte'

  let { spec } = $props()

  // The models a run would use now (`pipelines.js#runDetection`): the user's
  // own on this computer, the fixed best combination on the cloud GPU.
  const selection = untrack(() => runDetection(session))
  // Opened as the cloud entry, or with detection set to run on the cloud GPU
  // in Text cleanup: the review starts with its cloud choice made.
  // `WorkflowAnalysis` still refuses it where the cloud GPU is not usable.
  const initialCloud = untrack(() => spec.props.cloud === true || selection.target === 'cloud')
</script>

<Modal title={t('workflow.title.review')} width={1040} onclose={() => closeModal(null)}>
  <WorkflowAnalysis
    chapterId={spec.props.chapterId}
    initialPageIndex={spec.props.pageIndex ?? editor.pageIndex}
    initialWorkflow={workflowForDetectorModels(selection.detectorModels)}
    initialRtProfile={selection.detectorModels.includes('rtFull') ? 'full-halves' : 'small-whole'}
    {initialCloud}
  />

  {#snippet buttons()}
    <Button variant="ghost" onclick={() => closeModal('done')}>{t('shell.action.done')}</Button>
  {/snippet}
</Modal>
