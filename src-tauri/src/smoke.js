// The page half of the launch check (`smoke.rs`). Evaluated into the main
// window once its page has loaded, only when the app was started with
// MANGA_CLEANER_SMOKE_REPORT set. Plain script, no imports: it runs beside the
// bundled app rather than inside it, through the same `__TAURI_INTERNALS__`
// the app's own API calls go through.
(async () => {
  const internals = window.__TAURI_INTERNALS__
  const errors = []
  const note = (error) => errors.push(String(error?.message ?? error?.reason ?? error))
  window.addEventListener('error', (event) => note(event.error ?? event.message))
  window.addEventListener('unhandledrejection', (event) => note(event.reason))
  const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
  const mounted = () => (document.getElementById('app')?.childElementCount ?? 0) > 0

  const started = performance.now()
  for (let waited = 0; !mounted() && waited < 30000; waited += 100) await pause(100)
  const report = {
    mounted: mounted(),
    mountMs: Math.round(performance.now() - started),
    userAgent: navigator.userAgent,
    viewport: [window.innerWidth, window.innerHeight, window.devicePixelRatio],
  }

  // A command round trip, through the capability the window really has.
  try {
    report.diagnostics = await internals.invoke('diagnostics')
    report.ipc = true
  } catch (error) {
    report.ipc = false
    note(error)
  }

  // Every layer and mask image is a cross-origin fetch to the tile scheme
  // (src/lib/api/boundedimage.js). A chapter that does not exist answers with
  // an HTTP error, which proves the scheme is reachable; a refused fetch
  // rejects instead, which is the failure this looks for.
  try {
    const response = await fetch(`${internals.convertFileSrc('', 'tile')}smoke/0/source`)
    report.tile = { reachable: true, status: response.status, body: (await response.text()).slice(0, 200) }
  } catch (error) {
    report.tile = { reachable: false, error: String(error) }
  }

  // Errors thrown by the first work the app starts on its own.
  await pause(3000)
  report.stillMounted = mounted()
  report.errors = errors
  report.ok = report.mounted && report.stillMounted && report.ipc && report.tile.reachable
  await internals.invoke('plugin:event|emit', { event: 'smoke-frontend', payload: report })
})()
