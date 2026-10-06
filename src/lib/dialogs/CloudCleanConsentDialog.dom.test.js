/**
 * The consent in front of a cloud clean, mounted: it states the region count,
 * the pages, whether anything is cleaned on this computer, the batches, the
 * regions held back, the endpoint, the GPU, the GPU time and cost range, and
 * the plan it is given for. Confirm answers only with both statements
 * checked. Escape is Cancel.
 */
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import CloudCleanConsentDialog from './CloudCleanConsentDialog.svelte'
import { app } from '../state/app.svelte.js'

const PROPOSAL = { proposalId: 'clean-prop', chapterId: 'c1', regionIds: ['a', 'b', 'c'], regions: 3, pages: 2,
  totalCropPixels: 184320, totalWorkPixels: 1769472, localCleaned: 0, execution: 'cloud', localCandidates: 0,
  chunkRegions: 256, chunks: 1, unresolvedIds: [], estimatedGpuSeconds: { low: 90, high: 745 },
  estimatedCostUsd: { low: 0.03, high: 0.06 }, gpu: 'L4', provider: 'modal', profileName: 'Studio A100',
  planDigest: 'b'.repeat(64), expiresAtMs: Date.now() + 300000 }

function spec(proposal = PROPOSAL, onresolve = vi.fn()) {
  const modal = {
    id: 'm1', kind: 'cloudCleanConsent', titleKey: 'cloud.clean.title', blocking: true, dismissable: true,
    props: { proposal, endpoint: 'studio.modal.run' },
    actions: [
      { id: 'cancel', labelKey: 'cloud.analysis.consent.cancel' },
      { id: 'confirm', labelKey: 'cloud.clean.confirm', variant: 'primary' },
    ],
    onresolve,
  }
  app.modals.push(/** @type {any} */ (modal))
  return modal
}

/** @param {string} name */
const fact = (name) => document.querySelector(`[data-fact="${name}"]`)?.textContent ?? ''

afterEach(() => {
  cleanup()
  app.modals.length = 0
})

it('states the regions, pages, that nothing is cleaned here, where, on which GPU and at what cost', () => {
  render(CloudCleanConsentDialog, { props: { spec: spec() } })
  expect(fact('what')).toContain(t('cloud.clean.regions', { count: 3 }))
  expect(fact('what')).toContain(t('cloud.analysis.pages', { count: 2 }))
  const execution = /** @type {HTMLElement} */ (document.querySelector('[data-fact="execution"]'))
  expect(execution.dataset.execution).toBe('cloud')
  expect(execution.textContent?.trim()).toBe(t('cloud.clean.executionCloud'))
  expect(fact('batches')).toContain(t('cloud.clean.batchCount', { count: 1 }))
  expect(document.querySelector('[data-fact="held"]')).toBeNull()
  expect(fact('gpu-time')).toBe(t('cloud.clean.gpuTime', { low: 2, high: 12 }))
  expect(fact('plan')).toBe('b'.repeat(64))
  expect(fact('where')).toContain('Studio A100')
  expect(fact('where')).toContain('studio.modal.run')
  expect(fact('gpu')).toBe('L4')
  const cost = /** @type {HTMLElement} */ (document.querySelector('[data-cost]'))
  expect(cost.dataset.cost).toBe('estimate')
  expect(cost.textContent?.trim()).toBe(t('cloud.clean.costRange', { low: 0.03, high: 0.06 }))
})

it('states one consent for a plan of many batches, and the regions held back', () => {
  const ids = Array.from({ length: 1025 }, (_, n) => `r${n}`)
  render(CloudCleanConsentDialog, { props: { spec: spec({ ...PROPOSAL, regionIds: ids, regions: 1025, pages: 41,
    chunks: 5, unresolvedIds: ['x', 'y'] }) } })
  expect(fact('what')).toContain(t('cloud.clean.regions', { count: 1025 }))
  expect(fact('batches')).toContain(t('cloud.clean.batchesValue', {
    batches: t('cloud.clean.batchCount', { count: 5 }), size: '256' }))
  expect(fact('batches')).toContain(t('cloud.clean.batchesNote'))
  expect(fact('held')).toBe(t('cloud.clean.heldBackValue', { count: 2 }))
  expect(document.querySelector('[data-fact="tooLarge"]')).toBeNull()
})

it('names the regions left out as too large for the render service', () => {
  render(CloudCleanConsentDialog, { props: { spec: spec({ ...PROPOSAL, tooLargeIds: ['big'] }) } })
  expect(fact('tooLarge')).toBe(t('cloud.clean.tooLargeValue', { count: 1 }))
  expect(t('cloud.clean.tooLargeValue', { count: 1 })).toContain('too large for your cloud GPU')
})

it('names mixed execution when it was asked for, and what it tries here after the start', () => {
  render(CloudCleanConsentDialog, { props: { spec: spec({ ...PROPOSAL, execution: 'mixed', localCandidates: 2 }) } })
  const execution = /** @type {HTMLElement} */ (document.querySelector('[data-fact="execution"]'))
  expect(execution.dataset.execution).toBe('mixed')
  expect(execution.textContent).toContain(t('cloud.clean.mixed.label'))
  expect(execution.textContent).toContain(t('cloud.clean.executionMixed', { count: 2 }))
  // Plainly: the consent still covers every region as sent.
  expect(t('cloud.clean.executionMixed', { count: 2 })).toMatch(/cost range .* every region as sent/)
  expect(t('cloud.clean.executionMixed', { count: 2 })).toContain('up to that full amount')
})

it('says the price and the GPU are unknown rather than guessing', () => {
  render(CloudCleanConsentDialog, { props: { spec: spec({ ...PROPOSAL, estimatedCostUsd: null, gpu: null }) } })
  const cost = /** @type {HTMLElement} */ (document.querySelector('[data-cost]'))
  expect(cost.dataset.cost).toBe('unknown')
  expect(cost.textContent?.trim()).toBe(t('cloud.analysis.costUnknown'))
  expect(fact('gpu')).toBe(t('cloud.clean.gpuUnknown'))
})

it('answers Confirm with both statements, and not before', async () => {
  const onresolve = vi.fn()
  const screen = render(CloudCleanConsentDialog, { props: { spec: spec(PROPOSAL, onresolve) } })
  const confirm = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('cloud.clean.confirm') }))
  expect(confirm.disabled).toBe(true)
  await fireEvent.click(screen.getByLabelText(t('cloud.analysis.rights')))
  expect(confirm.disabled).toBe(true)
  await fireEvent.click(confirm)
  expect(onresolve).not.toHaveBeenCalled()
  await fireEvent.click(screen.getByLabelText(t('cloud.analysis.retention')))
  expect(confirm.disabled).toBe(false)
  await fireEvent.click(confirm)
  // With the plan it showed: the grant is minted only for that plan.
  expect(onresolve).toHaveBeenCalledWith({ rightsAttested: true, retentionAcknowledged: true, planDigest: 'b'.repeat(64) })
})

it('takes Escape as Cancel, and answers nothing a send could be made of', async () => {
  const onresolve = vi.fn()
  render(CloudCleanConsentDialog, { props: { spec: spec(PROPOSAL, onresolve) } })
  await fireEvent.keyDown(window, { key: 'Escape' })
  expect(onresolve).toHaveBeenCalledTimes(1)
  const [answer] = onresolve.mock.calls[0]
  expect(answer === null || typeof answer === 'string').toBe(true)
})
