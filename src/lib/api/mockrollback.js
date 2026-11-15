/**
 * The mock's released versions, for the rollback dialog in a browser.
 *
 * Around the fixture's own version: one newer release (an update, so the
 * dialog leaves it out), the installed one, four it can go back to, and one
 * below the floor. `0.9.1` has no manifest, the way a release from before
 * per-version manifests has none until it is back-filled, so it shows the
 * download page path. `0.8.9` fails its signature, the way a wrongly signed build is
 * refused. `?rollback=deb` makes this copy a `.deb` install, which cannot be
 * replaced in place. `?rollback=offline` (or the mock's `rollback: 'offline'`
 * option) makes the list fail, the way an offline or rate-limited machine does.
 */

const FLOOR = '0.8.5'

const RELEASES = [
  { version: '0.9.4', date: '2026-11-12T10:00:00Z', notes: 'A newer release. It is an update, so the rollback list leaves it out.', readsFormat: 6, honoursSkip: true },
  { version: '0.9.3', date: '2026-11-10T10:00:00Z', notes: 'Nano Banana cleans.', readsFormat: 6, honoursSkip: true },
  { version: '0.9.2', date: '2026-11-04T09:30:00Z', notes: 'The clone stamp has a new Live copy mode: the copy stays linked to the area you picked and updates when that area changes. The eraser now works on moved, turned and duplicated copies.', readsFormat: 5, honoursSkip: false },
  { version: '0.9.1', date: '2026-11-03T21:14:06Z', notes: 'Faster SAM-TS-L setup.', readsFormat: 5, honoursSkip: false },
  { version: '0.9.0', date: '2026-10-31T19:07:37Z', notes: '', readsFormat: 5, honoursSkip: false },
  { version: '0.8.9', date: '2026-10-28T20:08:54Z', notes: 'Shown with a bad signature in the mock.', readsFormat: 5, honoursSkip: false },
  { version: '0.8.4', date: '2026-09-17T20:35:04Z', notes: 'The first public beta.', readsFormat: 2, honoursSkip: false },
]

/** @param {string} version */
const pageUrl = (version) => `https://github.com/k-omiq/Bear/releases/tag/v${version}`

/**
 * @param {string} currentVersion
 * @param {{offline?: boolean}} [options]
 */
export function mockRollbackCatalog(currentVersion, options = {}) {
  const search = String(globalThis.location?.search ?? '')
  if (options.offline || /[?&]rollback=offline\b/.test(search)) {
    throw new Error('rollback_offline: api.github.com: HTTP 403 rate limit exceeded')
  }
  const deb = /[?&]rollback=deb\b/.test(search)
  return {
    currentVersion,
    bundle: deb ? 'deb' : 'app',
    inPlace: !deb,
    floor: FLOOR,
    manifestFormat: 6,
    versions: RELEASES.map((r) => ({ ...r, pageUrl: pageUrl(r.version) })),
  }
}

/**
 * The update handle the dialog installs from: progress in a few steps, then
 * done. `0.9.1` has no manifest and `0.8.9` is refused at its signature.
 *
 * @param {string} currentVersion
 * @param {string} version
 * @param {(ms: number) => Promise<void>} wait
 */
export function mockRollbackPrepare(currentVersion, version, wait) {
  const release = RELEASES.find((r) => r.version === version)
  if (!release) throw new Error(`rollback_unreachable: no manifest for ${version}`)
  if (version === '0.9.1') throw new Error(`rollback_unreachable: no manifest for ${version}`)
  return {
    currentVersion,
    version,
    date: release.date,
    body: release.notes,
    /** @param {(event: any) => void} [onEvent] */
    async downloadAndInstall(onEvent) {
      const total = 72_000_000
      onEvent?.({ event: 'Started', data: { contentLength: total } })
      for (let i = 0; i < 5; i++) {
        await wait(250)
        onEvent?.({ event: 'Progress', data: { chunkLength: total / 5 } })
      }
      onEvent?.({ event: 'Finished' })
      if (version === '0.8.9') throw new Error('signature verification failed: the build is not signed by this app')
    },
  }
}
