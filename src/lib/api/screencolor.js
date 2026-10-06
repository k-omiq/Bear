/**
 * The colour picker's eyedropper: pick any colour off the screen.
 *
 * Not on the seam, for the reason `folder.js` gives for the folder chooser: it
 * produces no library state and belongs to the window, not to the data.
 *
 * Two routes, the web one first:
 *
 *   - `EyeDropper`, where the webview has it. That is Chromium: the browser
 *     mock, and WebView2 in the Windows app.
 *   - `pick_screen_color` in the macOS app, whose `WKWebView` has no
 *     `EyeDropper`. The command opens AppKit's own sampler
 *     (`src-tauri/src/screen_color.rs`).
 *
 * Anywhere else - WebKitGTK on Linux - there is no route, and the picker draws
 * no button: the swatch and the hex field reach the same value.
 */

import { isTauri } from './tauri.js'
import { isApplePlatform } from '../shortcuts.js'
import { hexOnCommit } from '../ui/color.js'

/**
 * How to pick a colour off the screen here, or `null` where there is no way.
 *
 * The picker answers `#rrggbb`, or `null` when the user pressed Escape.
 *
 * @returns {(() => Promise<string|null>) | null}
 */
export function screenColorPicker() {
  const EyeDropper = /** @type {any} */ (globalThis).EyeDropper
  if (typeof EyeDropper === 'function') {
    return async () => {
      try {
        const picked = await new EyeDropper().open()
        return hexOnCommit(String(picked?.sRGBHex ?? ''))
      } catch {
        return null // Escape rejects with AbortError
      }
    }
  }
  const invoke = globalThis.__TAURI__?.core?.invoke ?? globalThis.__TAURI_INTERNALS__?.invoke
  if (isTauri() && isApplePlatform() && typeof invoke === 'function') {
    return async () => {
      const picked = await invoke('pick_screen_color')
      return typeof picked === 'string' ? hexOnCommit(picked) : null
    }
  }
  return null
}
