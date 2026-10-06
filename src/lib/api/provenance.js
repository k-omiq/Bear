/**
 * Engine metadata and mask/provenance construction, shared by the fixtures
 * and by the mock engine's live results so a fixture mask and a mask made
 * during a session are indistinguishable in shape.
 *
 * Engine names, versions and the cloud provider are taken from
 * documented engine vocabulary rather than from the prototype's invented model
 * names. `params_snapshot` carries the constants actually used by the
 * pipeline.
 */

import { RUNGS, currentRung } from '../model/ladder.js'

/**
 * @typedef {Object} EngineInfo
 * @property {string} version - `Provenance.engine_version`
 * @property {string|null} modelSha - `Provenance.model_sha256`; null for the model-free ones
 * @property {string} provider - `Provenance.execution_provider`
 * @property {[number, number]} elapsed - inclusive millisecond range for the simulated run
 * @property {'match-surround'|'reconstruct'|'solid'} fillMode - the mode this rung produces by default
 */

/** @type {Readonly<Record<string, EngineInfo>>} */
export const ENGINE_INFO = Object.freeze({
  fill: {
    version: 'flat-fill 4',
    modelSha: null,
    provider: 'cpu',
    elapsed: [2, 14],
    fillMode: 'match-surround',
  },
  lama: {
    version: 'lama-manga onnx opset 17',
    modelSha: 'lama',
    provider: 'cpu',
    elapsed: [1000, 3000],
    fillMode: 'reconstruct',
  },
  // Rung 3a, the optional FLUX sidecar. Never on the automatic path and
  // never bundled; a mask made by one still has to be able
  // to name what made it, and the row's picker can name it on a machine that
  // has the sidecar installed.
  flux: {
    version: 'flux2-klein-4b',
    modelSha: null,
    provider: 'sidecar',
    elapsed: [10000, 60000],
    fillMode: 'reconstruct',
  },
  // FLUX.2 Klein on the user's own Modal or Beam endpoint: rung 3a's recipe
  // (IC-6), run on a GPU in their account, one consent per request. A render
  // committed today is recorded as `flux` with a `provenance.cloud` record
  // that names the provider, the endpoint and the model
  // (`mock.js#cloudRecordFor`), the way the native side records it; this
  // entry describes a mask whose engine is `cloud` itself.
  cloud: {
    version: 'flux2-klein-4b sdnq-4bit',
    modelSha: null,
    provider: 'cloud',
    elapsed: [3000, 15000],
    fillMode: 'reconstruct',
  },
  paint: {
    version: 'brush-paint 1',
    modelSha: null,
    provider: 'cpu',
    elapsed: [2, 14],
    fillMode: 'solid',
  },
  clone: {
    version: 'clone-heal 1',
    modelSha: null,
    provider: 'cpu',
    elapsed: [5, 20],
    fillMode: 'match-surround',
  },
})

/** Constants the pipeline actually ran with. */
const PARAMS_SNAPSHOT = Object.freeze({
  min_mask_thickness: 4,
  mask_growth_step: 2,
  annulus_offset: [1, 5],
  mask_deviation_max: 8,
  isolation_radius: 5,
  edit_margin: 6,
})

/**
 * The rung a region routes to, given a ceiling. Deterministic in the region
 * id, so the same region always comes back on the same rung - a re-run is a
 * re-run, not a dice roll.
 *
 * Automatic routing never escalates to FLUX or cloud: the automatic ceiling
 * is pinned at LaMa ('lama'). A ceiling saved as the retired `denoise` rung
 * is a fill ceiling (`ladder.js#currentRung`).
 *
 * @param {number} hash - `hashString(regionId)`
 * @param {string} ceiling - highest rung permitted, a member of `RUNGS` or legacy 'cloud'
 * @returns {string} rung id
 */
export function routeRung(hash, ceiling) {
  const lama = RUNGS.indexOf('lama')
  const resolvedCeiling = ceiling === 'cloud' || ceiling === 'flux' ? 'lama' : currentRung(ceiling)
  const ceilingIndex = Math.min(lama, Math.max(0, RUNGS.indexOf(resolvedCeiling)))
  // Most regions are flat paper and belong on rung 0.
  // Only a few reach the inpainter, because every one of those is a review
  // entry and the flag rate is capped at 5% of boxes.
  const index = hash % 32 >= 29 ? lama : 0
  return RUNGS[Math.min(index, ceilingIndex)]
}

/**
 * Caps a requested rung at a ceiling. Never raises it: a ceiling is an upper
 * bound on what a run may reach, not an instruction to reach it. Either one
 * given as the retired `denoise` rung is read as `fill`.
 *
 * @param {string} rawRequested
 * @param {string} rawCeiling
 * @returns {string} rung id, the lower of the two
 */
export function capRung(rawRequested, rawCeiling) {
  const requested = currentRung(rawRequested)
  const ceiling = currentRung(rawCeiling)
  if (requested === ceiling) return requested
  if (requested === 'cloud') return ceiling === 'cloud' ? 'cloud' : capRung('lama', ceiling)
  if (ceiling === 'cloud') return requested
  const requestedIndex = RUNGS.indexOf(requested)
  const ceilingIndex = RUNGS.indexOf(ceiling)
  if (requestedIndex === -1 && ceilingIndex === -1) return 'fill'
  if (requestedIndex === -1) return ceiling
  if (ceilingIndex === -1) return requested
  return RUNGS[Math.min(requestedIndex, ceilingIndex)]
}

/**
 * @typedef {Object} MaskSpec
 * @property {string} regionId
 * @property {number} sequence - monotonic revision number
 * @property {string} engine - rung id the mask's provenance records
 * @property {import('./rng.js').Rng} rng - source of the simulated hashes and elapsed time
 * @property {string} created - ISO 8601 timestamp
 * @property {string} sourceSha - the page's `source_sha256`
 * @property {'match-surround'|'reconstruct'|'solid'} [fillMode]
 * @property {number} [elapsedMs] - overrides the engine's simulated range
 * @property {boolean} [fittingReconstructed]
 */

/**
 * Builds a `Mask` with a complete `Provenance` record.
 *
 * @param {MaskSpec} spec
 * @returns {import('../model/types.js').Mask}
 */
export function buildMask(spec) {
  const engine = currentRung(spec.engine)
  const info = ENGINE_INFO[engine] ?? ENGINE_INFO.fill
  const elapsedMs = spec.elapsedMs ?? spec.rng.int(info.elapsed[0], info.elapsed[1])
  const params_snapshot = { ...PARAMS_SNAPSHOT }
  if (spec.tool) {
    params_snapshot.tool = spec.tool
  }
  if (spec.source) {
    params_snapshot.source = spec.source
  }
  if (spec.fillMode) {
    params_snapshot.fill_mode = spec.fillMode
  }
  return {
    id: `${spec.regionId}-m${spec.sequence}`,
    regionId: spec.regionId,
    sequence: spec.sequence,
    fillMode: spec.fillMode ?? info.fillMode,
    elapsedMs,
    fittingReconstructed: spec.fittingReconstructed ?? false,
    // Always null, as on every mask the native side makes today: it carries
    // an outcome only for a job saved with a legacy cloud review state
    // (`src-tauri/src/library.rs#review_flags`).
    cloudOutcome: null,
    provenance: {
      engine,
      engine_version: info.version,
      model_sha256: info.modelSha ? spec.rng.sha256() : null,
      execution_provider: info.provider,
      params_snapshot,
      mask_sha256: spec.rng.sha256(),
      source_sha256: spec.sourceSha,
      // A cloud render records its own (`mock.js#commitCloudResult`).
      cloud: null,
      created: spec.created,
    },
  }
}

/**
 * A context that can commit a mask. Fixture construction and a live session
 * differ only in where the revision number and the timestamp come from -
 * `pagebuilder.js` counts from a fixed epoch so launches stay byte-identical,
 * the mock engine reads its injected clock - so they expose the same shape and
 * share `commitMask`.
 *
 * @typedef {Object} CommitContext
 * @property {import('./rng.js').Rng} rng
 * @property {() => number} nextSequence - next mask revision number
 * @property {() => string} created - ISO 8601 timestamp for `Provenance.created`
 */

/**
 * Commits a cleaned mask to a region. The single definition of what cleaning
 * a region resets: the outcome becomes `cleaned`, and the gate-skip and
 * decline records are cleared, because a region that now has a mask was
 * neither skipped nor declined.
 *
 * @param {import('../model/types.js').Region} region
 * @param {CommitContext} ctx
 * @param {Partial<MaskSpec> & { tool?: string, source?: string }} spec - engine and any overrides passed to `buildMask`
 * @returns {import('../model/types.js').Mask} the committed mask
 */
export function commitMask(region, ctx, spec) {
  region.outcome = 'cleaned'
  region.gateSkipCause = null
  region.declineReason = null
  if (spec.tool) {
    region.tool = spec.tool
  }
  region.mask = buildMask({
    regionId: region.id,
    sequence: ctx.nextSequence(),
    rng: ctx.rng,
    created: ctx.created(),
    sourceSha: region.sourceSha ?? '',
    tool: spec.tool ?? region.tool,
    source: spec.source ?? region.source,
    ...spec,
  })
  return region.mask
}
