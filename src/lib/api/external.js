/**
 * Open a web page in the system browser.
 *
 * Inside the app the opener plugin does it (`withGlobalTauri` exposes it, and
 * the capability allows the project's page and community invite). In the
 * browser mock a new tab is the same thing. A refusal is not the caller's
 * problem: the link simply does nothing.
 *
 * @param {string} url
 */
export function openExternal(url) {
  const opener = /** @type {any} */ (globalThis).__TAURI__?.opener
  if (opener?.openUrl) {
    opener.openUrl(url).catch(() => {})
    return
  }
  globalThis.open?.(url, '_blank', 'noopener,noreferrer')
}
