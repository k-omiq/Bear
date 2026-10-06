/** Saved capabilities belong to a deployment, including its address and revision. */
import { notify } from './app.svelte.js'
import { writeSettingsSerialized } from './settingswrite.js'

export const cloudConfig = $state({ entries: {} })
let owner = null
let loading = null
let generation = 0
let storageReadable = true
const reads = new Map()

export function configurationBelongsTo(backend) {
  return owner === backend
}

export function configurationKey(readiness, target = null) {
  const selected = readiness?.target
  const provider = target?.provider ?? selected?.type
  const profileId = target?.profileId ?? selected?.profile_id
  const endpoint = readiness?.endpoints?.find((entry) => entry.provider === provider && entry.id === profileId)
    ?? (selected?.type === provider && selected?.profile_id === profileId ? readiness.profile : null)
  return endpoint && provider && profileId
    ? JSON.stringify([provider, profileId, endpoint.endpointUrl ?? '', endpoint.updatedAtMs ?? '']) : null
}

export function rememberedConfiguration(key) {
  return key ? cloudConfig.entries[key] ?? null : null
}

/** Only validated observations are restored. No tokens or error details are stored. */
export function loadCloudConfiguration(backend) {
  if (owner === backend) return loading
  owner = backend
  const epoch = ++generation
  storageReadable = true
  cloudConfig.entries = {}
  reads.clear()
  loading = Promise.resolve().then(() => backend.readSettings?.()).then((settings) => {
    if (owner !== backend || epoch !== generation) return
    const entries = settings?.cloudConfigurationMemory
    if (!entries || typeof entries !== 'object' || Array.isArray(entries)) return
    const restored = {}
    for (const [key, entry] of Object.entries(entries)) {
      try {
        const identity = JSON.parse(key)
        if (!Array.isArray(identity) || identity.length !== 4 || !['modal', 'beam'].includes(identity[0])) continue
        const value = {}
        for (const feature of ['clean', 'denoise']) {
          const known = entry?.[feature]
          if (!known || !['ready', 'missing'].includes(known.state)) continue
          value[feature] = { state: known.state,
            presets: Array.isArray(known.presets) ? known.presets.filter((id) => typeof id === 'string') : [],
            modelId: typeof known.modelId === 'string' ? known.modelId : null }
        }
        restored[key] = value
      } catch { /* A damaged entry cannot become a capability. */ }
    }
    // A live observation made during the read wins over disk.
    cloudConfig.entries = { ...restored, ...cloudConfig.entries }
  }).catch(() => {
    if (owner === backend && epoch === generation) {
      storageReadable = false
      notify({ key: 'cloud.configuration.loadFailed', tone: 'warn' })
    }
  })
  return loading
}

export async function rememberConfiguration(backend, key, feature, observation) {
  if (!key) return
  await loadCloudConfiguration(backend)
  if (owner !== backend) return
  cloudConfig.entries = { ...cloudConfig.entries,
    [key]: { ...cloudConfig.entries[key], [feature]: observation } }
  const snapshot = JSON.parse(JSON.stringify(cloudConfig.entries))
  // A failed initial read cannot authorize replacing records we never saw.
  if (!storageReadable) {
    notify({ key: 'cloud.configuration.saveFailed', tone: 'warn' })
    return
  }
  try {
    await writeSettingsSerialized(backend, { cloudConfigurationMemory: snapshot })
  } catch {
    notify({ key: 'cloud.configuration.saveFailed', tone: 'warn' })
  }
}

/** Share overlapping reads; retain saved evidence when a deployment is unreachable. */
export async function checkConfiguration(backend, key, feature, read, force = false) {
  await loadCloudConfiguration(backend)
  if (owner !== backend) return null
  const known = rememberedConfiguration(key)?.[feature]
  const epoch = generation
  if (known && !force) return known
  const readKey = `${key}:${feature}`
  if (reads.has(readKey)) return reads.get(readKey)
  const task = (async () => {
    try {
      const observation = await read()
      if (owner !== backend || epoch !== generation) return null
      // A failure from a running job is newer evidence than a metadata read
      // already in flight. It must not be erased by that delayed answer.
      const latest = rememberedConfiguration(key)?.[feature]
      if (latest && latest !== known) return latest
      await rememberConfiguration(backend, key, feature, observation)
      return observation
    } catch (error) {
      if (owner !== backend || epoch !== generation) return null
      if (await configurationFailed(backend, key, feature, error)) return rememberedConfiguration(key)?.[feature]
      return rememberedConfiguration(key)?.[feature] ?? null
    }
  })()
  reads.set(readKey, task)
  try { return await task }
  finally { if (reads.get(readKey) === task) reads.delete(readKey) }
}

/** Definitive setup failures change only the failed operation on its original deployment. */
export async function configurationFailed(backend, key, feature, error) {
  const code = String(error instanceof Error ? error.message : error ?? '').replace(/^Error:\s*/, '')
  const stable = code.split(':')[0].trim()
  const accessFailure = ['gateway_unauthorized', 'credential_missing', 'endpoint_invalid'].includes(stable)
    || code.startsWith('gateway authorization failure (status ')
    || code === 'runtime credential missing or invalid for profile'
    || code === 'invalid or missing profile configuration'
  const misconfigured = accessFailure || (feature === 'denoise'
    ? code.startsWith('capability_unavailable: gateway denoise is not configured')
      || code.startsWith('capability_unavailable: the deployment has no models')
    : stable === 'weights_unavailable' || code.startsWith('cloud render weights need repair')
      || code.startsWith('capability_unavailable: gateway render is not configured'))
  if (!key || !misconfigured) return false
  await loadCloudConfiguration(backend)
  if (owner !== backend) return false
  const changed = rememberedConfiguration(key)?.[feature]?.state !== 'missing'
  if (!changed) return true
  await rememberConfiguration(backend, key, feature, { state: 'missing', presets: [],
    modelId: rememberedConfiguration(key)?.[feature]?.modelId ?? null })
  if (changed) notify({ key: feature === 'denoise' ? 'cloud.configuration.denoiseChanged' : 'cloud.configuration.cleanChanged', tone: 'warn' })
  return true
}

export function resetCloudConfiguration() {
  generation += 1
  owner = null
  loading = null
  reads.clear()
  cloudConfig.entries = {}
  storageReadable = true
}
