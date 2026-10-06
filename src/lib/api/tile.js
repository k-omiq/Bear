/**
 * `tile://` URLs - the one place the interface builds one.
 *
 * Pixels reach the browser over a custom protocol
 * rather than as base64 in an IPC payload, and the UI uses plain `<img>`. The
 * Rust half is `src-tauri/src/tile.rs`; this half decides only *what to ask
 * for*, and the two agree on one shape:
 *
 *     tile://localhost/<chapterId>/<pageIndex>/<variant>[/<tile>]?v=<version>
 *     http://tile.localhost/<chapterId>/<pageIndex>/<variant>[/<tile>]?v=<version>
 *
 * `variant` is `source` or `cleaned`.
 *
 * ## What the interface asks for is a proxy, never the page
 *
 * *The UI holds proxies, never the strip.* Previews cap the
 * **short** edge at 1024 - capping the long edge would draw an 800×20000 page
 * as a 40×1024 sliver - and tile the long axis, so a page reaches the webview
 * as a handful of independently fetched, decoded and evicted images rather than
 * as one 64 MB bitmap. [`proxyPlan`] is this side's copy of that arithmetic and
 * `src-tauri/src/tile.rs` serves against `cleaner_core::image::proxy`'s. The
 * fourth path segment is the tile index; a URL without one is the page at its
 * own resolution, which rule 7 reserves for the viewport past 100% zoom.
 *
 * Two copies of one rule is a drift risk taken deliberately, on the same terms
 * as `TILE_SCHEME` below: the alternative is putting the tile geometry on the
 * seam, which is a seam change. They are pinned to the same numbers by a test
 * on each side, but the risk a drift carries is not symmetric.
 *
 * A drift where this side's plan asks for **more** tiles than Rust's is loud:
 * the extra index is a `404` and a broken `<img>`, never a wrong picture. A
 * drift the other way is not. If this side's plan is *smaller* - this file's
 * `PROXY_TILE_LONG_EDGE` raised without the Rust one moving too, or a page
 * reaching [`proxyPlan`] with no declared dimensions - this side asks for a
 * subset of the tiles the real page has, and `PageArtwork.svelte` stretches
 * each one it does get over `extent%` of its own, too-small `proxyLong`. The
 * page renders. It renders wrong, and nothing here says so. The missing-
 * dimensions half of that is the one this file can close without a seam
 * change: [`proxyPlan`] refuses - answers no tiles - rather than guessing one
 * whole tile that does not match what Rust actually cut. No tiles is the same
 * stand-in path `PageArtwork` already takes when `tileUrl` returns `null`; one
 * guessed tile is a confident wrong picture.
 *
 * ## The origin comes from Tauri, not from a platform test here
 *
 * The origin differs by platform - `tile://localhost/` on macOS, iOS and
 * Linux, `http://tile.localhost/` on Windows and Android. Tauri already makes
 * exactly this choice, in `convertFileSrc`, from the operating system it was
 * compiled for and from whether the window was built with `use_https_scheme`.
 * Asking it is therefore better than sniffing a user agent: a user agent is a
 * guess, this is the answer, and it cannot drift from what the webview actually
 * registered. It also means a machine this code has never run on - Windows and
 * Linux are both unexecuted here - gets the right origin without
 * anybody having predicted it.
 *
 * `convertFileSrc('')` is used for the origin alone. It is not used to build
 * the whole URL, because it percent-encodes its argument as a **single** path
 * segment, and this path has three.
 *
 * ## `v` is a hash, never a timestamp
 *
 * A hash decides staleness, an mtime only explains it. The token
 * is folded from the page's own content hashes - the source `sha256` and, for
 * `cleaned`, the native appearance digests of the page and of every layer on
 * it (see `pageVersion`) - so the URL changes exactly when the bytes behind it
 * change, and never otherwise. The
 * protocol handler can then answer `Cache-Control: immutable`, which is the
 * whole reason the token exists: without it the webview would either re-fetch
 * every page on every render or show a stale one after an edit.
 *
 * The fold is not a cryptographic hash and does not need to be. Its inputs
 * already are; its job is to make a short cache key out of them.
 */

/** The scheme, matching `tile::SCHEME` in `src-tauri/src/tile.rs`. */
export const TILE_SCHEME = 'tile'

/**
 * The short edge's ceiling for a preview. Matches
 * `cleaner_core::image::proxy::PROXY_SHORT_EDGE`.
 */
export const PROXY_SHORT_EDGE = 1024

/**
 * How much of the long axis one tile covers, in proxy pixels. Matches
 * `cleaner_core::image::proxy::PROXY_TILE_LONG_EDGE`.
 *
 * **Provisional.** What decides it is how many
 * bytes WebView2's UI thread can absorb in one response - it raises
 * `WebResourceRequested` on that thread and pauses page load while the handler
 * runs, at a maintainer-measured ~200 ms for 10 MB against ~5 ms on macOS
 * - and that measurement has never been taken on a Windows
 * machine here. That is what happens when a one-machine number is
 * written down as a general one, so this is not tuned; it is bounded, at the
 * next power of two above the short-edge cap. It is folded into the version
 * token below, so changing it cannot leave a differently-cut tile behind in an
 * immutable cache.
 */
export const PROXY_TILE_LONG_EDGE = 2048
export const COLOR_PIPELINE_VERSION = 1

/**
 * The two variants the protocol serves. `original` is `PageArtwork`'s word for
 * the same thing and is mapped at that one call site rather than accepted here,
 * so the URL vocabulary has one spelling.
 *
 * @typedef {'source'|'cleaned'} TileVariant
 */

/**
 * Where tiles come from on this platform, with a trailing slash, or `null`
 * outside a Tauri window.
 *
 * `null` is not a failure: the mock backend still ships as `setBackend`'s
 * fallback and has no pixels at all, so a component that gets `null` draws its
 * stand-in artwork. That is what keeps the interface and its 432 tests working
 * in a plain browser.
 *
 * @returns {string|null}
 */
export function tileOrigin() {
  const convert = /** @type {any} */ (globalThis).__TAURI_INTERNALS__?.convertFileSrc
  if (typeof convert !== 'function') return null
  const origin = convert('', TILE_SCHEME)
  return typeof origin === 'string' && origin.endsWith('/') ? origin : null
}

/**
 * @typedef {Object} ProxyTile
 * @property {number} index - the URL's fourth segment
 * @property {number} offset - where the tile starts along the long axis, in percent
 * @property {number} extent - how much of the long axis it covers, in percent
 */

/**
 * @typedef {Object} ProxyPlan
 * @property {number} width - the proxy's width in px
 * @property {number} height - the proxy's height in px
 * @property {boolean} vertical - whether the tiled long axis is `y`
 * @property {ProxyTile[]} tiles - in order, covering the page exactly
 */

/**
 * How one page is cut into preview tiles - the same arithmetic as
 * `cleaner_core::image::proxy::ProxyPlan::for_page`, in integers so the two can
 * agree exactly rather than nearly.
 *
 * A page that declares no dimensions gets **no tiles**, not one tile guessed
 * at 1×1. Answering one whole tile here would ask the protocol for a real
 * tile index - 0 - and stretch whatever it returns over 100% of the sheet,
 * which is a wrong picture rendered with confidence (see the module header).
 * Refusing instead means `tiles` comes back empty, `PageArtwork.svelte` never
 * emits an `<img>`, and the sheet falls through to the same stand-in path it
 * already uses when `tileUrl` has no protocol to call. That path is honest
 * about not having pixels; a guessed tile is not. Today `library.rs` always
 * supplies `source.w`/`source.h` and the seam types are non-optional, so this
 * branch does not fire - that makes the refusal cheap to keep, not safe to
 * drop, because the invariant lives in the manifest's types and not in
 * anything this file controls.
 *
 * @param {{width?: number, height?: number}|null|undefined} page
 * @returns {ProxyPlan}
 */
export function proxyPlan(page) {
  const pageWidth = dimension(page?.width)
  const pageHeight = dimension(page?.height)
  if (pageWidth === null || pageHeight === null) {
    return { width: 0, height: 0, vertical: false, tiles: [] }
  }
  const short = Math.min(pageWidth, pageHeight)
  const long = Math.max(pageWidth, pageHeight)

  // A cap, never an enlargement: a preview of a 400×600 page is 400×600.
  const proxyShort = Math.min(short, PROXY_SHORT_EDGE)
  const proxyLong =
    proxyShort === short ? long : Math.max(proxyShort, Math.round((long * proxyShort) / short))

  const vertical = pageHeight >= pageWidth
  const count = Math.max(1, Math.ceil(proxyLong / PROXY_TILE_LONG_EDGE))

  /** @type {ProxyTile[]} */
  const tiles = []
  for (let index = 0; index < count; index += 1) {
    const start = index * PROXY_TILE_LONG_EDGE
    const size = Math.min(PROXY_TILE_LONG_EDGE, proxyLong - start)
    tiles.push({ index, offset: (start / proxyLong) * 100, extent: (size / proxyLong) * 100 })
  }

  return {
    width: vertical ? proxyShort : proxyLong,
    height: vertical ? proxyLong : proxyShort,
    vertical,
    tiles,
  }
}

/**
 * The URL for one page's tile, or `null` when there is no protocol to serve it
 * or no page to name.
 *
 * `tile` is a proxy tile index - what every drawing component asks for. Leaving
 * it out asks for the page at its own resolution, which is reserved
 * for the viewport past 100% zoom.
 *
 * A `cleaned` URL also carries `a`, the native appearance this copy of the
 * page was listed with. The protocol draws from the manifest as it is now,
 * and this copy can be older - a page listing read before a layer write
 * landed and installed after it - so it lets a response be cached only when
 * `a` names the state the pixels came from, and answers any other with
 * `no-store` (`tile.rs#serve_request`). A page with no native digest (the
 * mock, a run's stub header) sends none and is never cached.
 *
 * @param {import('./backend.js').ApiPage|null|undefined} page
 * @param {TileVariant} variant
 * @param {number} [tile]
 * @returns {string|null}
 */
export function tileUrl(page, variant, tile) {
  const origin = tileOrigin()
  if (!origin || !page || !page.chapterId || !Number.isInteger(page.index)) return null
  if (tile !== undefined && !(Number.isInteger(tile) && tile >= 0)) return null
  const chapter = encodeURIComponent(page.chapterId)
  const path = tile === undefined ? '' : `/${tile}`
  const appearance = variant === 'cleaned' && page.appearance ? `&a=${encodeURIComponent(page.appearance)}` : ''
  return `${origin}${chapter}/${page.index}/${variant}${path}?v=${pageVersion(page, variant)}${appearance}`
}

/**
 * The URL of one detected region's mask image, or `null` when there is no
 * protocol to serve it (the mock, the tests) or nothing to name.
 *
 *     tile://localhost/<chapterId>/<pageIndex>/detection/<regionId>?v=<maskSha256>.<sequence>
 *
 * The response is a PNG cropped to the mask's tight bounds: opaque white
 * where Clean may erase (the mask with its padding, and the lettering),
 * transparent elsewhere. Where it sits on the page is not in the URL but in
 * the `x-mask-bounds` header, in page pixels, so it is read with `fetch`
 * rather than an `<img>` (`editor/detectionmasks.svelte.js`).
 *
 * `v` is the mask's own digest and its `sequence`, which is one more than the
 * hand edits it has had, so the URL moves whenever the drawn pixels do: an
 * edit can change the lettering and leave the mask file's digest as it was,
 * and the sequence moves then too. The interface caches by URL alone; the
 * protocol answers `no-store` because it keeps no copy of its own.
 * A region with no mask has nothing to fetch and gets `null` too.
 *
 * @param {import('./backend.js').ApiPage|null|undefined} page
 * @param {import('./backend.js').ApiRegion|null|undefined} region
 * @returns {string|null}
 */
export function detectionMaskUrl(page, region) {
  const origin = tileOrigin()
  if (!origin || !page || !page.chapterId || !Number.isInteger(page.index)) return null
  if (!region || typeof region.id !== 'string' || !region.id || !region.mask) return null
  const sha = region.mask.provenance?.mask_sha256
  const sequence = Number.isInteger(region.mask.sequence) ? region.mask.sequence : 1
  const version = `${typeof sha === 'string' ? sha : ''}.${sequence}`
  return `${origin}${encodeURIComponent(page.chapterId)}/${page.index}/detection/` +
    `${encodeURIComponent(region.id)}?v=${encodeURIComponent(version)}`
}

/**
 * The URL of one patch's layer as page `page` draws it, or `null` when there
 * is no protocol to serve it (the mock, the tests) or no layer to name.
 *
 *     tile://localhost/<chapterId>/<pageIndex>/layer/<regionId>?v=<version>
 *
 * The editor draws a page as its `source` tiles with one of these over them
 * per patch (`editor/PatchLayers.svelte`), so a clean changes one image and
 * never the page. The response is the part of the patch that lands on `page`
 * - in a longstrip, `region` may belong to the page `anchor` and reach across
 * a join - as a see-through PNG on the page's proxy grid, with where it sits
 * in the `x-layer-bounds` header, in proxy pixels. A patch with nothing on
 * the page answers `204`.
 *
 * `v` folds `mask.layerKey` (`tile::layer_appearance`), which moves with the
 * patch's pixels and geometry and never with its opacity, which the canvas
 * draws; and the two pages' sources and their distance apart, which decide
 * where a part lands when the strip is reordered.
 *
 * @param {import('./backend.js').ApiPage|null|undefined} page
 * @param {import('./backend.js').ApiRegion|null|undefined} region
 * @param {import('./backend.js').ApiPage|null|undefined} [anchor] - the page `region` belongs to; `page` by default
 * @returns {string|null}
 */
export function layerUrl(page, region, anchor = page) {
  const origin = tileOrigin()
  if (!origin || !page || !page.chapterId || !Number.isInteger(page.index)) return null
  const key = region?.mask?.layerKey
  if (!region || typeof region.id !== 'string' || !region.id || typeof key !== 'string' || !key) return null
  const reach = anchor && anchor !== page ? `|${anchor.sourceSha ?? ''}|${Number(anchor.index) - page.index}` : ''
  const version = fold(`${COLOR_PIPELINE_VERSION}:${key}|${page.sourceSha ?? ''}${reach}`)
  return `${origin}${encodeURIComponent(page.chapterId)}/${page.index}/layer/` +
    `${encodeURIComponent(region.id)}?v=${version}`
}

/**
 * Every `source` proxy tile URL of one page - exactly what
 * `PageArtwork.svelte` asks for when the page is drawn, so fetching these ahead
 * warms the same responses. The page's patches are drawn as layers over them
 * and warmed apart (`editor/patchlayers.svelte.js#warmLayers`). Empty outside
 * a Tauri window.
 *
 * @param {import('./backend.js').ApiPage|null|undefined} page
 * @returns {string[]}
 */
export function pageTileUrls(page) {
  const urls = []
  for (const tile of proxyPlan(page).tiles) {
    const url = tileUrl(page, 'source', tile.index)
    if (url) urls.push(url)
  }
  return urls
}

/**
 * The cache-busting token for one page and variant.
 *
 * `source` depends on the source file alone. `cleaned` depends on everything
 * that decides what its tiles draw: patch content and order, visibility,
 * opacity and geometry. **That identity is native.** `page.appearance` is
 * `tile::page_appearance`, read out of the manifest with the same selection
 * `tile::render` composites - in a longstrip that includes a neighbour's patch
 * that reaches across a join - so it is the same string for the same saved
 * state after a reload, an undo or a reopen, and a different one for any
 * saved change to what the page shows. A position lock is not an appearance
 * and does not move it.
 *
 * Every in-hand layer's own `mask.appearance` (`tile::record_appearance`)
 * sits beside it, because the page header is only as fresh as the last
 * `loadPages`: it arrives with the edit that changed it, so the URL moves the
 * moment the answer lands rather than one reload later. Detections draw
 * nothing and are left out. A mask with no digest (the mock) is folded from
 * its content fields and its style instead.
 *
 * The editor no longer draws `cleaned` tiles - it draws `source` tiles and a
 * layer per patch (`layerUrl`) - so this token names the flattened page for
 * what still wants it: the home screen's cover, and the mock's paint hold.
 *
 * Masks are folded in sorted order rather than array order, because the
 * backend is free to return regions in a different order without the page
 * having changed. The proxy geometry is folded in because it decides what a
 * tile index *means*; the native digest folds Rust's copy of it too, so a
 * change to either constant moves every URL.
 *
 * @param {import('./backend.js').ApiPage} page
 * @param {TileVariant} variant
 * @returns {string}
 */
export function pageVersion(page, variant) {
  const parts = [
    `${COLOR_PIPELINE_VERSION}:${PROXY_SHORT_EDGE}x${PROXY_TILE_LONG_EDGE}`,
    page.sourceSha ?? '',
    String(page.tileRevision ?? 0),
  ]
  if (variant === 'cleaned') {
    parts.push(page.appearance ?? '')
    const masks = (page.regions ?? [])
      .filter((region) => region.mask != null && region.outcome !== 'detected')
      .map((region) => maskAppearance(region.mask))
      .sort()
    parts.push(...masks)
  }
  return fold(parts.join('|'))
}

/**
 * One layer's appearance: its native digest, or - for a mask that has none -
 * its content fields and every style field that changes a pixel.
 *
 * @param {any} mask
 * @returns {string}
 */
export function maskAppearance(mask) {
  if (typeof mask?.appearance === 'string' && mask.appearance) return mask.appearance
  const layer = mask?.layer ?? {}
  return [
    mask?.id, mask?.sequence, mask?.provenance?.mask_sha256 ?? '', mask?.provenance?.created ?? '',
    mask?.provenance?.engine ?? '', layer.opacity ?? 100, layer.offsetX ?? 0, layer.offsetY ?? 0,
    layer.rotation ?? 0,
  ].join(':')
}

/**
 * FNV-1a, twice, at two offsets - sixteen hex characters.
 *
 * `Math.imul` rather than `BigInt` because this runs inside a `$derived` for
 * every page on screen, and a longstrip column has forty of them in two
 * variants each.
 *
 * @param {string} text
 * @returns {string}
 */
export function fold(text) {
  return word(text, 0x811c9dc5) + word(text, 0x9e3779b9)
}

/**
 * A dimension as a positive whole number of pixels, or `null` when the page
 * does not declare one - the refusal `proxyPlan` acts on above.
 *
 * @param {unknown} value
 * @returns {number|null}
 */
function dimension(value) {
  const number = Math.round(Number(value))
  return Number.isFinite(number) && number > 0 ? number : null
}

/**
 * @param {string} text
 * @param {number} offset
 * @returns {string}
 */
function word(text, offset) {
  let hash = offset
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i)
    hash = Math.imul(hash, 0x01000193)
  }
  return (hash >>> 0).toString(16).padStart(8, '0')
}
