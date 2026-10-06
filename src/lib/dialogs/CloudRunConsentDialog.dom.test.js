/**
 * The consent in front of a cloud-detection Auto clean, mounted: it says how
 * many pages go, as how many tiles and pixels, and Send answers only with both
 * statements checked.
 */
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render } from '@testing-library/svelte'
import { t } from '../i18n/index.js'
import CloudRunConsentDialog from './CloudRunConsentDialog.svelte'
import { app } from '../state/app.svelte.js'

const SAM = 'text_mask_sam_ts@1'
const RT = 'text_regions_rt@1'

function spec(scope, proposal, onresolve = vi.fn(), cleanFollows = false) {
  const modal = {
    id: 'm1', kind: 'cloudRunConsent', titleKey: 'cloud.analysis.run.title', blocking: true, dismissable: true,
    props: { scope, proposal, cleanFollows },
    actions: [
      { id: 'cancel', labelKey: 'cloud.analysis.consent.cancel' },
      { id: 'confirm', labelKey: 'cloud.analysis.run.confirm', variant: 'primary' },
    ],
    onresolve,
  }
  app.modals.push(/** @type {any} */ (modal))
  return modal
}

const CHAPTER = { proposalId: 'run-prop', chapterId: 'c1', pageIndices: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
  provider: 'modal', profileId: 'p1', profileName: 'Studio A100', capabilities: [SAM, RT], pages: 12,
  totalTiles: 48, totalTilePixels: 46080000, costEstimateUsd: null, expiresAtMs: Date.now() + 300000,
  models: [
    { capability: SAM, graphSha256s: ['a'.repeat(64)], modelRevision: 'c'.repeat(40) },
    { capability: RT, graphSha256s: ['b'.repeat(64)], modelRevision: 'd'.repeat(40) },
  ] }

afterEach(() => {
  cleanup()
  app.modals.length = 0
})

it('states a chapter run as its page count, tiles and pixels, and what happens to a failed page', () => {
  const screen = render(CloudRunConsentDialog, { props: { spec: spec('chapter', CHAPTER) } })
  const text = document.body.textContent ?? ''
  expect(text).toContain(t('cloud.analysis.run.headingChapter', { pages: t('cloud.analysis.pages', { count: 12 }) }))
  expect(text).toContain('12 pages sent in full for detection, split into 48 tiles.')
  expect(text).toContain('46,080,000 uploaded pixels, including any overlap.')
  expect(text).toContain(t('cloud.analysis.run.scopeChapter'))
  expect(text).toContain(`☁ SAM-TS-L lettering mask, revision ${'c'.repeat(12)}…`)
  expect(text).toContain(`☁ Ogkalu comic text & bubble detector (Full), revision ${'d'.repeat(12)}…`)
  expect(text).toContain(t('cloud.analysis.costUnknown'))
  expect(screen.queryByText(t('cloud.analysis.run.scope'))).toBeNull()
})

it('says a cloud clean asks for its own consent after detection, and covers none here', () => {
  render(CloudRunConsentDialog, { props: { spec: spec('chapter', CHAPTER, vi.fn(), true) } })
  expect(document.querySelector('[data-fact="clean-follows"]')?.textContent).toBe(t('cloud.clean.asksAfterDetection'))
  expect(document.body.textContent).not.toContain(t('cloud.analysis.run.resultValue'))
  cleanup()
  render(CloudRunConsentDialog, { props: { spec: spec('chapter', CHAPTER) } })
  expect(document.querySelector('[data-fact="clean-follows"]')).toBeNull()
  expect(document.body.textContent).toContain(t('cloud.analysis.run.resultValue'))
})

it('answers Send with both statements, and not before', async () => {
  const onresolve = vi.fn()
  const screen = render(CloudRunConsentDialog, { props: { spec: spec('page', { ...CHAPTER, pageIndices: [0], pages: 1 }, onresolve) } })
  expect(document.body.textContent).toContain(t('cloud.analysis.run.heading', { page: 1 }))
  const send = /** @type {HTMLButtonElement} */ (screen.getByRole('button', { name: t('cloud.analysis.run.confirm') }))
  expect(send.disabled).toBe(true)
  await fireEvent.click(screen.getByLabelText(/I have the rights/))
  expect(send.disabled).toBe(true)
  await fireEvent.click(screen.getByLabelText(/I have reviewed and accept/))
  expect(send.disabled).toBe(false)
  await fireEvent.click(send)
  expect(onresolve).toHaveBeenCalledWith({ rightsAttested: true, retentionAcknowledged: true })
})
