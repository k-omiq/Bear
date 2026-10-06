/** Keep the desktop webview from exposing browser reload/navigation actions.
 * Only prevent defaults: custom editor menus still receive their events.
 */
export function installReloadGuard(target = window) {
  const context = (event) => event.preventDefault()
  const key = (event) => {
    const reload = event.key === 'F5' || ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r')
    if (reload) {
      event.preventDefault()
      event.stopImmediatePropagation()
    }
  }
  target.addEventListener('contextmenu', context)
  target.addEventListener('keydown', key, true)
  return () => {
    target.removeEventListener('contextmenu', context)
    target.removeEventListener('keydown', key, true)
  }
}
