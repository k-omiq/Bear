/**
 * The words for a running SAM-TS-L install, from its `onSamInstallProgress`
 * events. Setup's download list and the Settings model row say the same thing.
 */

const STAGE_KEYS = Object.freeze({
  python: 'settings.models.samSetup.python',
  environment: 'settings.models.samSetup.environment',
  dependencies: 'settings.models.samSetup.dependencies',
  weights: 'settings.models.samSetup.weights',
  source: 'settings.models.samSetup.source',
  export: 'settings.models.samSetup.export',
  verify: 'settings.models.samSetup.verify',
})

/**
 * The status line's key and params: the stage and the whole install's percent.
 *
 * @param {(key: string, params?: Object) => string} t
 * @param {{step: string, percent: number}} progress
 * @returns {{key: string, params: Object}}
 */
export function samSetupState(t, { step, percent }) {
  return {
    key: 'settings.models.samSetup.progress',
    params: { stage: t(STAGE_KEYS[step] ?? STAGE_KEYS.python), percent: Math.min(100, Math.max(0, Math.floor(percent))) },
  }
}
