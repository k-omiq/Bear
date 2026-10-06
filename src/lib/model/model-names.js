/**
 * The four detection choices by their persisted ids, as every screen names
 * them. The ids are stored in sessions and settings and never change; only
 * these names do. Full and Small are the two published profiles of one
 * upstream model, `ogkalu/comic-text-and-bubble-detector`, and a selection
 * holds at most one of them (`pipelines.js#toggleDetectorModel`).
 */
export const DETECTOR_MODEL_NAMES = Object.freeze({
  ctd: 'Comic Text Detector (CTD)',
  rtFull: 'Ogkalu comic text & bubble detector (Full)',
  rtSmall: 'Ogkalu comic text & bubble detector (Small)',
  samTs: 'SAM-TS-L lettering mask',
})

/**
 * The detection model behind each cloud analysis capability. Only these two
 * have a cloud twin today; CTD and the Small profile run on this computer.
 */
export const ANALYSIS_CAPABILITY_MODELS = Object.freeze({
  'text_regions_rt@1': 'rtFull',
  'text_mask_sam_ts@1': 'samTs',
})

/** Product names shared by memory reporting, cloud provenance and engine selectors. */
const MODEL_NAMES = Object.freeze({
    'flux2-klein-4b': 'FLUX.2 Klein 4B',
    'flux2-klein-9b': 'FLUX.2 Klein 9B',
    'qwen-image-edit-2511': 'Qwen-Image-Edit-2511',
    'flux1-schnell': 'FLUX.1 [schnell]',
    'flux1-dev': 'FLUX.1 [dev]',
    'lama-manga': 'LaMa Manga',
    'sam-ts-l': DETECTOR_MODEL_NAMES.samTs,
    'ogkalu/comic-text-and-bubble-detector': DETECTOR_MODEL_NAMES.rtFull,
    'Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic': 'FLUX.2 Klein 4B',
    'Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32': 'FLUX.2 Klein 9B',
    'Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32': 'Qwen-Image-Edit-2511',
  })

/**
 * A detection model's visible name by its persisted id, or null for an id this build does not know.
 *
 * @param {unknown} id
 * @returns {string|null}
 */
export function detectorModelName(id) {
  const names = /** @type {Record<string, string>} */ (DETECTOR_MODEL_NAMES)
  return typeof id === 'string' && Object.hasOwn(names, id) ? names[id] : null
}

/**
 * The visible name of a cloud analysis capability (`text_regions_rt@1`), or
 * null for one this build does not know. A helper or gateway may send its own
 * label beside the id; the app names the model the same way everywhere.
 *
 * @param {unknown} capability
 * @returns {string|null}
 */
export function analysisCapabilityName(capability) {
  const models = /** @type {Record<string, string>} */ (ANALYSIS_CAPABILITY_MODELS)
  return typeof capability === 'string' && Object.hasOwn(models, capability) ? detectorModelName(models[capability]) : null
}

/** Unknown IDs do not get a guessed product name in a user-facing engine choice. */
export function knownModelName(id) {
  return typeof id === 'string' ? MODEL_NAMES[id] ?? null : null
}

export function displayModelName(id) {
  return knownModelName(id) ?? id
}

/** The current endpoint's verified ID, if the endpoint or a prior consent supplied one. */
export function currentCloudModelId(cloud) {
  const readiness = cloud?.readiness
  const profile = readiness?.profile
  const explicit = profile?.modelId ?? profile?.model_id
  if (typeof explicit === 'string' && explicit) return explicit
  const target = readiness?.target
  const remembered = cloud?.model
  return remembered && target?.type === remembered.provider && target?.profile_id === remembered.profileId &&
    (profile?.updatedAtMs ?? null) === (remembered.updatedAtMs ?? null)
    ? remembered.id
    : null
}

/**
 * One cloud profile's entry among the engines, when there is more than one:
 * its model, and the profile that runs it. An unknown model gets no guessed
 * name; the profile alone names the entry.
 *
 * @param {unknown} modelId
 * @param {string} profileName
 */
export function cloudProfileLabel(modelId, profileName) {
  const name = knownModelName(modelId)
  return name ? `☁ ${name} · ${profileName}` : `☁ ${profileName}`
}

/** Put the model among local engines while making its remote execution visible. */
export function engineModelLabel(engine, modelId, fallback) {
  const name = knownModelName(modelId)
  if (engine === 'cloud') return name ? `☁ ${name} · Cloud` : `☁ ${fallback}`
  if (engine === 'flux') return name ? `${name} · Local` : fallback
  return fallback
}
