/**
 * The consent in front of a streamed Detect and clean, mounted: it states the
 * pages, where, the GPU and its price, what a region and the start-up cost,
 * and a spending cap the person sets. Start answers only with both
 * statements checked and a cap from the fixed cost to the maximum; a standing
 * project skips the statements, never the cap. Escape is Cancel.
 */
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import CloudCleanStreamConsentDialog from './CloudCleanStreamConsentDialog.svelte'
import { app } from '../state/app.svelte.js'

const PROPOSAL = { proposalId: 'stream-prop', chapterId: 'c1', pages: 12, pageIndices: [...Array(12).keys()],
  execution: 'cloud', gpu: 'L4', gpuUsdPerHour: 0.8, fixedCostUsd: 0.16, regionCostUsd: 0.002,
  suggestedCapUsd: 0.4, maxCapUsd: 100, provider: 'modal', profileName: 'Studio A100',
  streamDigest: 'd'.repeat(64), expiresAtMs: Date.now() + 300000, standing: false }

function spec(proposal = PROPOSAL, onresolve = vi.fn()) {
  const modal = {
    id: 'm1', kind: 'cloudCleanStreamConsent', titleKey: 'cloud.clean.stream.title', blocking: true, dismissable: true,
    props: { proposal, endpoint: 'studio.modal.run' },
    actions: [
      { id: 'cancel', labelKey: 'cloud.analysis.consent.cancel' },
      { id: 'confirm', labelKey: 'cloud.clean.stream.confirm', variant: 'primary' },
    ],
    onresolve,
  }
  app.modals.push(/** @type {any} */ (modal))
  return modal
}

/** @param {string} name */
const fact = (name) => document.querySelector(`[data-fact="${name}"]`)?.textContent ?? ''
const capField = () => /** @type {HTMLInputElement} */ (document.querySelector('[data-fact="cap"]'))

afterEach(() => {
  cleanup()
  app.modals.length = 0
})

it('states the pages, where, the GPU and its price, and the cost of a region and of start-up', () => {
  render(CloudCleanStreamConsentDialog, { props: { spec: spec() } })
  expect(document.body.textContent).toContain(t('cloud.clean.stream.heading', { pages: t('cloud.analysis.pages', { count: 12 }) }))
  expect(fact('what')).toBe(t('cloud.clean.stream.whatValue'))
  expect(fact('where')).toContain('Studio A100')
  expect(fact('where')).toContain('studio.modal.run')
  expect(fact('gpu').trim()).toBe(t('cloud.clean.stream.price', { gpu: 'L4', price: 0.8 }))
  expect(fact('cost')).toContain(t('cloud.clean.stream.costValue', { region: 0.002, fixed: 0.16 }))
  expect(fact('plan')).toBe('d'.repeat(64))
  expect(Number(capField().value)).toBe(0.4)
  // Plainly: the cap is the app's estimate, and the provider bills actual use.
  expect(t('cloud.clean.stream.capHelp')).toMatch(/estimate/)
  expect(t('cloud.clean.stream.capHelp')).toMatch(/bills actual use/)
})

it('answers Start with both statements, the digest and the cap, and not before', async () => {
  const onresolve = vi.fn()
  const screen = render(CloudCleanStreamConsentDialog, { props: { spec: spec(PROPOSAL, onresolve) } })
  const start = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('cloud.clean.stream.confirm') }))
  expect(start.disabled).toBe(true)
  await fireEvent.click(screen.getByLabelText(t('cloud.analysis.rights')))
  await fireEvent.click(screen.getByLabelText(t('cloud.analysis.retention')))
  expect(start.disabled).toBe(false)
  await fireEvent.input(capField(), { target: { value: '2.5' } })
  await fireEvent.click(start)
  expect(onresolve).toHaveBeenCalledWith({ rightsAttested: true, retentionAcknowledged: true,
    streamDigest: 'd'.repeat(64), capUsd: 2.5 })
})

it('holds Start while the cap is below the fixed cost, above the maximum, or empty', async () => {
  const onresolve = vi.fn()
  const screen = render(CloudCleanStreamConsentDialog, { props: { spec: spec(PROPOSAL, onresolve) } })
  await fireEvent.click(screen.getByLabelText(t('cloud.analysis.rights')))
  await fireEvent.click(screen.getByLabelText(t('cloud.analysis.retention')))
  const start = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('cloud.clean.stream.confirm') }))
  for (const value of ['0.1', '100.01', '']) {
    await fireEvent.input(capField(), { target: { value } })
    expect(start.disabled, value).toBe(true)
    expect(document.body.textContent).toContain(t('cloud.clean.stream.capInvalid', { min: 0.16, max: 100 }))
  }
  await fireEvent.click(start)
  expect(onresolve).not.toHaveBeenCalled()
})

it('skips the statements for a standing project, never the cap', async () => {
  const onresolve = vi.fn()
  const screen = render(CloudCleanStreamConsentDialog, { props: { spec: spec({ ...PROPOSAL, standing: true }, onresolve) } })
  expect(screen.queryByLabelText(t('cloud.analysis.rights'))).toBeNull()
  expect(capField()).not.toBeNull()
  await fireEvent.click(screen.getByRole('button', { name: t('cloud.clean.stream.confirm') }))
  // No statement is answered for the user: the native side holds the project's.
  expect(onresolve).toHaveBeenCalledWith({ rightsAttested: false, retentionAcknowledged: false,
    streamDigest: 'd'.repeat(64), capUsd: 0.4 })
})

it('takes Escape as Cancel', async () => {
  const onresolve = vi.fn()
  render(CloudCleanStreamConsentDialog, { props: { spec: spec(PROPOSAL, onresolve) } })
  await fireEvent.keyDown(window, { key: 'Escape' })
  const [answer] = onresolve.mock.calls[0]
  expect(answer === null || typeof answer === 'string').toBe(true)
})
