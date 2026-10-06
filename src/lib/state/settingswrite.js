/**
 * Settings writes share one native record. A full snapshot is read when its
 * turn arrives, after earlier targeted writes have settled. Callers that save
 * one choice pass a fixed patch so a later optimistic choice cannot rewrite
 * the earlier request.
 */

const queues = new WeakMap()
const saved = new WeakMap()

/** @param {any} backend @param {Record<string, any>|(() => Record<string, any>)} patch */
export function writeSettingsSerialized(backend, patch) {
  const previous = queues.get(backend) ?? Promise.resolve()
  const writing = previous.then(async () => {
    const value = typeof patch === 'function' ? patch() : patch
    await backend.writeSettings(value)
    saved.set(backend, { ...(saved.get(backend) ?? {}), ...value })
    return value
  })
  queues.set(backend, writing.catch(() => {}))
  return writing
}

/** Last confirmed value in this session, if a newer optimistic choice fails. */
export function savedSetting(backend, key, fallback) {
  const values = saved.get(backend)
  return values && Object.hasOwn(values, key) ? values[key] : fallback
}
