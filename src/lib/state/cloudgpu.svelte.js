/** GPU presence and activity, kept separately for every cloud deployment. */
import { untrack } from 'svelte'
import { getBackend } from '../api/backend.js'
import { notify, pushModal } from './app.svelte.js'
import { cloud, cloudUsable } from './cloud.svelte.js'
import { session } from './session.svelte.js'

export const GPU_POLL_MS = 15_000
export const ANALYSIS_SILENCE_MS = 10 * 60 * 1000
export const STOP_SETTLE_MS = 2 * 60 * 1000
const ROLES = ['render', 'analysis']
const ANALYSIS_RUNNING = ['confirmed', 'submitted_tile', 'result_cached_tile']
/** @typedef {{provider: 'beam'|'modal', profileId: string}} Target */
/** @typedef {{target: Target, containers: import('../api/backend.js').CloudGpuContainer[], supported: boolean|null,
 * identity: string, unsupported: 'outdated'|'provider'|null, stale: boolean, stopping: Record<string, number>, receivedAt: number}} Deployment */
const empty = () => ({ containers: [], supported: null, unsupported: null, stale: false, stopping: {}, receivedAt: 0 })
export const cloudGpu = $state({
  // Selected-profile projection, also used by Settings to offer an update.
  ...empty(), now: 0,
  /** @type {Record<string, Deployment>} */
  deployments: {},
  /** @type {Record<string, {heard: number, target: Target|null}>} */
  analyses: {},
  /** @type {Record<string, {role: string, target: Target|null}>} */
  runs: {},
})
const enabled = () => session.cloudAllowed && cloud.readiness.allowed
/** @returns {Target|null} */
export function selectedGpuTarget() {
  const target = cloud.readiness.target
  return cloudUsable() && target ? { provider: target.type, profileId: target.profile_id } : null
}
const targetKey = (target) => target ? `${target.provider}:${target.profileId}` : null
const sameTarget = (a, b) => targetKey(a) === targetKey(b)
const signature = (target) => {
  const endpoint = cloud.readiness.endpoints?.find((e) => e.provider === target.provider && e.id === target.profileId)
  return endpoint ? `${endpoint.endpointUrl}:${endpoint.updatedAtMs ?? ''}` : ''
}
const readSeq = new Map()
const reading = new Map()
let watchEpoch = 0
const runListeners = new Map()

function ensure(target) {
  const key = targetKey(target)
  if (!cloudGpu.deployments[key] || cloudGpu.deployments[key].identity !== signature(target)) {
    cloudGpu.deployments[key] = { target, identity: signature(target), ...empty() }
  }
  return cloudGpu.deployments[key]
}
function projectSelected(target, view) {
  if (!sameTarget(target, selectedGpuTarget())) return
  for (const field of ['containers', 'supported', 'unsupported', 'stale', 'stopping', 'receivedAt']) cloudGpu[field] = view[field]
}
function viewFor(target) {
  // Keep the projection usable for callers and fixtures that set it directly.
  return sameTarget(target, selectedGpuTarget()) ? cloudGpu : cloudGpu.deployments[targetKey(target)]
}
/** All observed GPUs stay visible until their own endpoint reports them gone. */
export function gpuDeployments() {
  if (!enabled()) return []
  return Object.values(cloudGpu.deployments).filter((view) => view.identity === signature(view.target)).map((view) => {
    const endpoint = cloud.readiness.endpoints?.find((e) => e.provider === view.target.provider && e.id === view.target.profileId)
    return { ...view, name: endpoint?.name || view.target.profileId }
  })
}
export function runningAnalyses(target = null) {
  const cutoff = Date.now() - ANALYSIS_SILENCE_MS
  return Object.values(cloudGpu.analyses).filter((a) => a.heard >= cutoff && (!target || sameTarget(a.target, target))).length
}
/** Capture the grant's destination before a start can race a selection change. */
export function trackCloudRun(runId, role, backend = getBackend(), target = selectedGpuTarget()) {
  if (!runId || cloudGpu.runs[runId] || !target) return
  cloudGpu.runs = { ...cloudGpu.runs, [runId]: { role, target } }
  ensure(target)
  void refreshCloudGpu(backend, target)
  const off = backend.subscribe?.((event) => {
    if (event.type === 'run-finished' && event.runId === runId) finishCloudRun(runId, backend)
  })
  if (typeof off === 'function') runListeners.set(runId, off)
}
export function finishCloudRun(runId, backend = getBackend()) {
  const run = cloudGpu.runs[runId]
  if (!run) return
  const runs = { ...cloudGpu.runs }; delete runs[runId]; cloudGpu.runs = runs
  runListeners.get(runId)?.(); runListeners.delete(runId)
  void refreshCloudGpu(backend, run.target)
}
export function isStopping(role, target = selectedGpuTarget()) {
  const until = viewFor(target)?.stopping[role]
  return typeof until === 'number' && until > Date.now()
}
function settleStops(view) {
  const up = new Set(view.containers.map((c) => c.role))
  view.stopping = Object.fromEntries(Object.entries(view.stopping).filter(([role, until]) => up.has(role) && until > Date.now()))
}
function forgetStop(role, target) {
  const view = ensure(target)
  const next = { ...view.stopping }; delete next[role]; view.stopping = next
  projectSelected(target, view)
}
/** A response updates only its own deployment; switching defaults cannot discard it. */
export function refreshCloudGpu(backend = getBackend(), target = selectedGpuTarget()) {
  const key = targetKey(target)
  const held = reading.get(key)
  if (target && held && held.identity === signature(target) && held.epoch === watchEpoch) return held.promise
  const promise = readGpu(backend, target)
  reading.set(key, { identity: target ? signature(target) : '', epoch: watchEpoch, promise })
  const clear = () => { if (reading.get(key)?.promise === promise) reading.delete(key) }
  void promise.then(clear, clear)
  return promise
}
async function readGpu(backend, target) {
  if (!target || !enabled()) { if (!target) cloudGpu.containers = []; return }
  const key = targetKey(target), mine = (readSeq.get(key) || 0) + 1
  readSeq.set(key, mine)
  const epoch = watchEpoch, identity = signature(target)
  const current = () => enabled() && epoch === watchEpoch && readSeq.get(key) === mine && identity === signature(target)
  const view = ensure(target)
  try {
    const status = await backend.getCloudGpuStatus(target)
    if (!current()) return
    view.stale = false
    view.supported = status?.supported === true
    view.unsupported = view.supported ? null : status?.unsupported === 'outdated' ? 'outdated' : 'provider'
    view.containers = view.supported && Array.isArray(status.containers) ? status.containers : []
    if (!view.supported) view.stopping = {}
    view.receivedAt = Date.now(); cloudGpu.now = view.receivedAt
    settleStops(view); projectSelected(target, view)
  } catch {
    if (current()) { view.stale = true; projectSelected(target, view) }
  }
}
export function scaledownLeftMs(container, target = selectedGpuTarget()) {
  if (container.state !== 'idle' || typeof container.scaledownInMs !== 'number') return null
  return Math.max(0, container.scaledownInMs - Math.max(0, cloudGpu.now - (viewFor(target)?.receivedAt || 0)))
}
function roleWorking(role, target) {
  return viewFor(target)?.containers.some((c) => c.role === role && c.state === 'busy') ||
    (role === 'render' ? cloud.jobs.some((job) => sameTarget(job.target, target)) : runningAnalyses(target) > 0) ||
    Object.values(cloudGpu.runs).some((run) => run.role === role && sameTarget(run.target, target))
}
function confirmStop(role) {
  return new Promise((resolve) => pushModal({
    kind: 'stopCloudGpu', titleKey: 'cloud.gpu.confirm.title',
    props: { bodyKey: role === 'render' ? 'cloud.gpu.confirm.bodyRender' : 'cloud.gpu.confirm.bodyAnalysis' },
    actions: [{ id: 'cancel', labelKey: 'shell.action.cancel' }, { id: 'stop', labelKey: 'cloud.gpu.confirm.action', variant: 'primary' }],
    onresolve: (result) => resolve(result === 'stop'),
  }))
}
/** Explicit target remains the target of this stop, even after the default changes. */
export async function stopCloudGpu(role, backend = getBackend(), target = selectedGpuTarget()) {
  if (!ROLES.includes(role) || !target || !enabled() || isStopping(role, target)) return
  const epoch = watchEpoch, identity = signature(target)
  const explicit = arguments.length >= 3
  const current = () => enabled() && epoch === watchEpoch && identity === signature(target) &&
    (explicit || sameTarget(target, selectedGpuTarget()))
  if (roleWorking(role, target) && !(await confirmStop(role))) return
  if (!current() || isStopping(role, target)) return
  const view = ensure(target)
  view.stopping = { ...view.stopping, [role]: Date.now() + STOP_SETTLE_MS }; projectSelected(target, view)
  try {
    const result = await backend.stopCloudGpu({ ...target, role })
    if (!current()) return
    if (result?.supported === false) {
      view.supported = false; view.containers = []; view.stopping = {}; projectSelected(target, view); return
    }
    if (!result?.stopped?.includes(role)) forgetStop(role, target)
    await refreshCloudGpu(backend, target)
  } catch {
    if (!current()) return
    forgetStop(role, target); notify({ key: 'cloud.gpu.notice.stopFailed', tone: 'warn' })
  }
}
export function onAnalysisRecord(record) {
  if (!record || typeof record !== 'object') return
  const { proposal_id: id, phase, provider, profile_id: profileId } = record
  const name = phase?.phase
  if (typeof id !== 'string' || !id || typeof name !== 'string') return
  const target = (provider === 'beam' || provider === 'modal') && typeof profileId === 'string'
    ? { provider, profileId } : cloudGpu.analyses[id]?.target || selectedGpuTarget()
  const next = { ...cloudGpu.analyses }
  if (ANALYSIS_RUNNING.includes(name)) { next[id] = { heard: Date.now(), target }; if (target) ensure(target) }
  else delete next[id]
  cloudGpu.analyses = next
}
function targetsToPoll() {
  const targets = new Map()
  const selected = selectedGpuTarget()
  if (selected && cloudGpu.supported !== false) targets.set(targetKey(selected), selected)
  for (const view of Object.values(cloudGpu.deployments)) {
    if (view.supported !== false && (view.containers.length || roleWorking('render', view.target) || roleWorking('analysis', view.target))) targets.set(targetKey(view.target), view.target)
  }
  return [...targets.values()]
}
let started = null
export function startCloudGpu(backend = getBackend()) {
  if (started) return
  const run = { stop: () => {}, unlisten: [] }; started = run
  try {
    Promise.resolve(backend.onRemoteAnalysis?.(onAnalysisRecord)).then((off) => {
      if (typeof off !== 'function') return
      if (started === run) run.unlisten.push(off); else off()
    }).catch(() => {})
  } catch { /* Backend may not publish analysis events. */ }
  let keyBefore, busyBefore = -1
  run.stop = $effect.root(() => {
    $effect(() => {
      const target = selectedGpuTarget(), key = target ? `${targetKey(target)}:${signature(target)}` : null
      untrack(() => {
        if (key === keyBefore) return
        keyBefore = key
        Object.assign(cloudGpu, empty())
        if (target) { projectSelected(target, ensure(target)); void refreshCloudGpu(backend, target) }
        if (enabled()) for (const endpoint of cloud.readiness.endpoints || []) {
          const other = { provider: endpoint.provider, profileId: endpoint.id }
          if (!cloudGpu.deployments[targetKey(other)]) void refreshCloudGpu(backend, other)
        }
      })
    })
    $effect(() => {
      const busy = cloud.jobs.length + Object.keys(cloudGpu.analyses).length + Object.keys(cloudGpu.runs).length
      untrack(() => {
        for (const job of cloud.jobs) if (job.target) ensure(job.target)
        if (busyBefore >= 0 && busy > busyBefore) for (const target of targetsToPoll()) void refreshCloudGpu(backend, target)
        busyBefore = busy
      })
    })
    $effect(() => {
      const views = Object.values(cloudGpu.deployments)
      const wanted = enabled() && (cloud.jobs.length || Object.keys(cloudGpu.analyses).length || Object.keys(cloudGpu.runs).length || views.some((v) => v.containers.length))
      if (!wanted) return
      const timer = setInterval(() => {
        const cutoff = Date.now() - ANALYSIS_SILENCE_MS
        cloudGpu.analyses = Object.fromEntries(Object.entries(cloudGpu.analyses).filter(([, a]) => a.heard >= cutoff))
        for (const target of targetsToPoll()) void refreshCloudGpu(backend, target)
      }, GPU_POLL_MS)
      return () => clearInterval(timer)
    })
    $effect(() => {
      if (!enabled() || !Object.values(cloudGpu.deployments).some((v) => !v.stale && v.containers.some((c) => c.state === 'idle'))) return
      const timer = setInterval(() => { cloudGpu.now = Date.now() }, 1000)
      return () => clearInterval(timer)
    })
  })
}
export function stopCloudGpuWatch() {
  const run = started; started = null
  run?.stop(); for (const off of run?.unlisten || []) off()
  watchEpoch += 1; readSeq.clear(); reading.clear()
  Object.assign(cloudGpu, empty(), { now: 0, deployments: {}, analyses: {}, runs: {} })
  for (const off of runListeners.values()) off(); runListeners.clear()
}
