import { afterEach, describe, expect, it } from 'vitest'

import {
  detectionMaskUrl,
  fold,
  layerUrl,
  pageTileUrls,
  pageVersion,
  proxyPlan,
  tileOrigin,
  tileUrl,
  PROXY_SHORT_EDGE,
  PROXY_TILE_LONG_EDGE,
  TILE_SCHEME,
  COLOR_PIPELINE_VERSION,
} from './tile.js'

/**
 * Tauri's own `convertFileSrc`, verbatim from `tauri/scripts/core.js`, with the
 * platform as a parameter.
 *
 * Copied rather than described because the whole point of asking Tauri for the
 * origin is that it is *Tauri's* rule and not a guess restated here - a test
 * that reimplemented it loosely could pass against a rule the webview does not
 * follow. Windows and Android have never been executed on this machine;
 * this is what the shipped script does on them, not a measurement of one.
 *
 * @param {'macos'|'linux'|'windows'|'android'} osName
 * @param {string} [protocolScheme]
 */
function tauriWindow(osName, protocolScheme = 'http') {
  return {
    convertFileSrc(filePath, protocol = 'asset') {
      const path = encodeURIComponent(filePath)
      return osName === 'windows' || osName === 'android'
        ? `${protocolScheme}://${protocol}.localhost/${path}`
        : `${protocol}://localhost/${path}`
    },
  }
}

/** @param {'macos'|'linux'|'windows'|'android'|null} osName */
function inside(osName, protocolScheme) {
  if (osName === null) delete globalThis.__TAURI_INTERNALS__
  else globalThis.__TAURI_INTERNALS__ = tauriWindow(osName, protocolScheme)
}

/** A page in the shape the adapter sends, with `count` cleaned regions. */
function aPage({ id = 'ch-1', index = 0, sha = 'aa11', masks = [] } = {}) {
  return {
    id: `${id}-p${index}`,
    chapterId: id,
    index,
    sourceSha: sha,
    regions: masks.map((mask, i) => ({
      id: `r${i}`,
      mask: mask && {
        id: mask.id,
        sequence: mask.sequence,
        provenance: {
          mask_sha256: mask.sha,
          created: mask.created,
          engine: mask.engine,
        },
      },
    })),
  }
}

afterEach(() => inside(null))

describe('the tile origin', () => {
  /**
   * `tile://localhost/` on macOS, iOS and Linux; `http://tile.localhost/` on
   * Windows and Android. Both are whitelisted in
   * `src-tauri/tauri.conf.json`'s `img-src`, and if this ever answered a third
   * thing the CSP would block it.
   */
  it('is the one Tauri chose for this platform', () => {
    inside('macos')
    expect(tileOrigin()).toBe('tile://localhost/')
    inside('linux')
    expect(tileOrigin()).toBe('tile://localhost/')
    inside('windows')
    expect(tileOrigin()).toBe('http://tile.localhost/')
    inside('android')
    expect(tileOrigin()).toBe('http://tile.localhost/')
  })

  /**
   * A window built with `use_https_scheme` serves the same protocol over
   * `https://tile.localhost/`. Nothing in this repository sets it, and this is
   * here so that turning it on is a failing CSP rather than a silent one: the
   * origin follows the window, and `tauri.conf.json` would have to follow it
   * too.
   */
  it('follows the window scheme rather than assuming http', () => {
    inside('windows', 'https')
    expect(tileOrigin()).toBe('https://tile.localhost/')
  })

  it('is null outside a Tauri window, where there is no protocol at all', () => {
    inside(null)
    expect(tileOrigin()).toBeNull()
    expect(tileUrl(aPage(), 'source')).toBeNull()
  })
})

describe('a tile URL', () => {
  it('is the shape the protocol parses', () => {
    inside('macos')
    const url = tileUrl(aPage({ id: 'ch-1', index: 7 }), 'cleaned')
    // The path `src-tauri/src/tile.rs#parse` splits into three, and the
    // cache-busting query it does not read.
    expect(url).toMatch(/^tile:\/\/localhost\/ch-1\/7\/cleaned\?v=[0-9a-f]{16}$/)
  })

  /**
   * `a` is the native appearance this copy of the page was listed with, which
   * `tile.rs#serve_request` checks against the manifest it draws from before
   * a response may be kept. A source tile is its file alone and needs none.
   */
  it('carries the listed appearance on a cleaned tile, for the protocol to check', () => {
    inside('macos')
    const page = { ...aPage({ id: 'ch-1', index: 7 }), appearance: '0123456789abcdef' }
    expect(tileUrl(page, 'cleaned', 0)).toMatch(
      /^tile:\/\/localhost\/ch-1\/7\/cleaned\/0\?v=[0-9a-f]{16}&a=0123456789abcdef$/,
    )
    expect(tileUrl(page, 'source', 0)).not.toContain('&a=')
    // A newer listing names the newer state, in both parts of the query.
    const newer = tileUrl({ ...page, appearance: 'fedcba9876543210' }, 'cleaned', 0)
    expect(newer).toContain('&a=fedcba9876543210')
    expect(newer?.split('&')[0]).not.toBe(tileUrl(page, 'cleaned', 0)?.split('&')[0])
  })

  it('carries the same path under the Windows origin', () => {
    inside('windows')
    const url = tileUrl(aPage({ id: 'ch-1', index: 7 }), 'cleaned')
    expect(url?.startsWith('http://tile.localhost/ch-1/7/cleaned?v=')).toBe(true)
  })

  /** The Rust side percent-decodes, so this side must percent-encode. */
  it('escapes a chapter id that is not a bare path segment', () => {
    inside('macos')
    expect(tileUrl(aPage({ id: 'ch 1/2' }), 'source')).toContain('/ch%201%2F2/0/source?v=')
  })

  it('is null for a page with no chapter or no index', () => {
    inside('macos')
    expect(tileUrl(null, 'source')).toBeNull()
    expect(tileUrl({ ...aPage(), chapterId: '' }, 'source')).toBeNull()
    expect(tileUrl({ ...aPage(), index: undefined }, 'source')).toBeNull()
  })

  it('names the scheme the Rust module registers', () => {
    expect(TILE_SCHEME).toBe('tile')
  })

  /** The fourth segment `src-tauri/src/tile.rs#parse` reads. */
  it('names a proxy tile when it is given one', () => {
    inside('macos')
    const page = aPage({ id: 'ch-1', index: 7 })
    expect(tileUrl(page, 'cleaned', 0)).toMatch(
      /^tile:\/\/localhost\/ch-1\/7\/cleaned\/0\?v=[0-9a-f]{16}$/,
    )
    expect(tileUrl(page, 'cleaned', 3)).toContain('/ch-1/7/cleaned/3?v=')
    // No tile is the page at its own resolution, which is a different URL.
    expect(tileUrl(page, 'cleaned')).toContain('/ch-1/7/cleaned?v=')
  })

  it('refuses a tile index that is not one, rather than asking for tile 0', () => {
    inside('macos')
    const page = aPage()
    expect(tileUrl(page, 'source', -1)).toBeNull()
    expect(tileUrl(page, 'source', 1.5)).toBeNull()
    expect(tileUrl(page, 'source', /** @type {any} */ ('0'))).toBeNull()
  })
})

/**
 * One detection's mask, which `src-tauri/src/tile.rs#parse` reads as the
 * `detection` form: the region id as the fourth segment and the mask's own
 * digest and edit sequence as the version.
 */
describe('a detection mask URL', () => {
  const detection = (id = 'c1-p0-r3', sha = 'ab12') => ({
    id,
    outcome: 'detected',
    mask: { id: `${id}-m1`, provenance: { mask_sha256: sha } },
  })

  it('names the page, the region and the mask digest', () => {
    inside('macos')
    expect(detectionMaskUrl(aPage({ id: 'ch-1', index: 4 }), detection())).toBe(
      'tile://localhost/ch-1/4/detection/c1-p0-r3?v=ab12.1',
    )
  })

  it('carries the same path under the Windows origin', () => {
    inside('windows')
    expect(detectionMaskUrl(aPage({ id: 'ch-1', index: 4 }), detection())).toBe(
      'http://tile.localhost/ch-1/4/detection/c1-p0-r3?v=ab12.1',
    )
  })

  /** The Rust side percent-decodes every segment, the region id included. */
  it('escapes a chapter or region id that is not a bare path segment', () => {
    inside('macos')
    const url = detectionMaskUrl(aPage({ id: 'ch 1/2', index: 0 }), detection('r 1/2#x'))
    expect(url).toBe('tile://localhost/ch%201%2F2/0/detection/r%201%2F2%23x?v=ab12.1')
  })

  it('moves when the mask digest does', () => {
    inside('macos')
    const page = aPage()
    expect(detectionMaskUrl(page, detection('r1', 'aa'))).not.toBe(detectionMaskUrl(page, detection('r1', 'bb')))
  })

  /** An edit can change the lettering and leave the mask file's digest alone. */
  it('moves when the edit sequence does, with the digest unchanged', () => {
    inside('macos')
    const page = aPage()
    const edited = { ...detection('r1', 'aa'), mask: { id: 'r1-m1', sequence: 3, provenance: { mask_sha256: 'aa' } } }
    expect(detectionMaskUrl(page, edited)).toBe('tile://localhost/ch-1/0/detection/r1?v=aa.3')
    expect(detectionMaskUrl(page, detection('r1', 'aa'))).toBe('tile://localhost/ch-1/0/detection/r1?v=aa.1')
  })

  it('is null outside a Tauri window, where the mock has no pixels', () => {
    inside(null)
    expect(detectionMaskUrl(aPage(), detection())).toBeNull()
  })

  it('is null for a page or region it cannot name, and for a region with no mask', () => {
    inside('macos')
    expect(detectionMaskUrl(null, detection())).toBeNull()
    expect(detectionMaskUrl({ ...aPage(), chapterId: '' }, detection())).toBeNull()
    expect(detectionMaskUrl({ ...aPage(), index: undefined }, detection())).toBeNull()
    expect(detectionMaskUrl(aPage(), null)).toBeNull()
    expect(detectionMaskUrl(aPage(), { ...detection(), id: '' })).toBeNull()
    expect(detectionMaskUrl(aPage(), { ...detection(), mask: null })).toBeNull()
  })
})

/**
 * These numbers are also asserted, in the same words, by
 * `cleaner_core::image::proxy`'s own tests - the two implementations are
 * separate on purpose (see the module header) and this is what pins them
 * together.
 */
describe('a layer URL', () => {
  const layer = (id = 'c1-p0-r3', key = 'k1') => ({ id, outcome: 'cleaned', mask: { id: `${id}-m1`, layerKey: key } })

  it('names the page it is drawn on and the region, versioned by the layer key and the source', () => {
    inside('macos')
    const page = aPage({ id: 'ch-1', index: 4 })
    expect(layerUrl(page, layer())).toBe(`tile://localhost/ch-1/4/layer/c1-p0-r3?v=${fold(`${COLOR_PIPELINE_VERSION}:k1|aa11`)}`)
  })

  it('moves with the layer key and not with the opacity, which the canvas draws', () => {
    inside('macos')
    const page = aPage()
    const faded = { ...layer(), mask: { ...layer().mask, layer: { opacity: 40 } } }
    expect(layerUrl(page, faded)).toBe(layerUrl(page, layer()))
    expect(layerUrl(page, layer('c1-p0-r3', 'k2'))).not.toBe(layerUrl(page, layer()))
  })

  it('names a neighbour\'s patch by the page it is drawn on, and moves with where the two sit', () => {
    inside('macos')
    const page = aPage({ index: 1, sha: 'bb22' })
    const anchor = aPage({ index: 0, sha: 'aa11' })
    const across = layerUrl(page, layer(), anchor)
    expect(across).toMatch(/^tile:\/\/localhost\/ch-1\/1\/layer\/c1-p0-r3\?v=/)
    expect(across).not.toBe(layerUrl(page, layer()))
    expect(layerUrl(page, layer(), aPage({ index: 0, sha: 'cc33' }))).not.toBe(across)
  })

  it('escapes a chapter or region id that is not a bare path segment', () => {
    inside('macos')
    expect(layerUrl(aPage({ id: 'ch 1' }), layer('a/b'))).toMatch(/\/ch%201\/0\/layer\/a%2Fb\?v=/)
  })

  it('is null outside a Tauri window, and for a region with no layer key', () => {
    expect(layerUrl(aPage(), layer())).toBeNull()
    inside('macos')
    expect(layerUrl(aPage(), { id: 'r', outcome: 'detected', mask: { id: 'r-m1', layerKey: null } })).toBeNull()
    expect(layerUrl(aPage(), { id: 'r', mask: null })).toBeNull()
    expect(layerUrl(null, layer())).toBeNull()
  })
})

describe('the tiles a page warms', () => {
  it('are its source tiles alone: its patches are layers, warmed apart', () => {
    inside('macos')
    const page = { ...aPage(), width: 800, height: 5000 }
    expect(pageTileUrls(page).map((url) => new URL(url).pathname)).toEqual([
      '/ch-1/0/source/0', '/ch-1/0/source/1', '/ch-1/0/source/2',
    ])
  })
})

describe('the proxy plan', () => {
  it('caps the short edge and leaves the long one to the tiles', () => {
    const plan = proxyPlan({ width: 2400, height: 3600 })
    expect(plan.width).toBe(PROXY_SHORT_EDGE)
    expect(plan.height).toBe(1536)
    expect(plan.vertical).toBe(true)
  })

  it('does not render a webtoon segment as a sliver', () => {
    // The failure rule 7 names outright: cap the long edge and this page is
    // 40×1024.
    const plan = proxyPlan({ width: 800, height: 20_000 })
    expect(plan.width).toBe(800)
    expect(plan.height).toBe(20_000)
    expect(plan.tiles).toHaveLength(10)
  })

  it('cuts the long axis into tiles that cover the page exactly once', () => {
    const plan = proxyPlan({ width: 800, height: 5000 })
    expect(plan.tiles).toHaveLength(3)
    let covered = 0
    for (const [at, tile] of plan.tiles.entries()) {
      expect(tile.index).toBe(at)
      expect(tile.offset).toBeCloseTo(covered, 10)
      covered += tile.extent
    }
    expect(covered).toBeCloseTo(100, 10)
    // The last band is the short one.
    expect(plan.tiles[2].extent).toBeLessThan(plan.tiles[0].extent)
  })

  it('tiles the long axis whichever axis that is', () => {
    const plan = proxyPlan({ width: 6000, height: 1200 })
    expect(plan.vertical).toBe(false)
    expect(plan.height).toBe(PROXY_SHORT_EDGE)
    expect(plan.width).toBe(5120)
    expect(plan.tiles).toHaveLength(3)
  })

  it('is a cap and never an enlargement', () => {
    const plan = proxyPlan({ width: 400, height: 600 })
    expect(plan.width).toBe(400)
    expect(plan.height).toBe(600)
    expect(plan.tiles).toHaveLength(1)
  })

  /**
   * Guessing one whole tile for an undeclared page is a wrong picture
   * rendered with confidence, once `PageArtwork` stretches whatever
   * that one tile turns out to be over 100% of the sheet. Refusing - no
   * tiles - instead sends the component down the same stand-in path it
   * already takes when there is no protocol to ask at all.
   */
  it('refuses rather than guesses one tile for a page that has not said how big it is', () => {
    for (const page of [null, undefined, {}, { width: 0, height: 0 }, { width: 800 }]) {
      const plan = proxyPlan(page)
      expect(plan.tiles).toHaveLength(0)
      expect(plan.width).toBe(0)
      expect(plan.height).toBe(0)
    }
  })

  it('agrees with the constants the Rust half is compiled with', () => {
    expect(PROXY_SHORT_EDGE).toBe(1024)
    expect(PROXY_TILE_LONG_EDGE).toBe(2048)
  })
})

describe('the version token', () => {
  /**
   * A hash decides staleness. The token is folded from content
   * hashes and from nothing else, so calling it twice on an unchanged page is
   * the same URL - which is what makes the response cacheable at all.
   */
  it('is stable for an unchanged page', () => {
    const page = aPage({ masks: [{ id: 'm1', sequence: 1, sha: 'ff' }] })
    expect(pageVersion(page, 'cleaned')).toBe(pageVersion(page, 'cleaned'))
    expect(pageVersion(page, 'source')).toBe(pageVersion(aPage(), 'source'))
  })

  it('changes when the source does', () => {
    expect(pageVersion(aPage({ sha: 'aa' }), 'source')).not.toBe(
      pageVersion(aPage({ sha: 'bb' }), 'source'),
    )
  })

  /** Every way a patch set can move: a new mask, a re-run, a deletion. */
  it('changes when the patch set does, and only for the cleaned variant', () => {
    const none = aPage()
    const one = aPage({ masks: [{ id: 'm1', sequence: 1, sha: 'ff' }] })
    const rerun = aPage({ masks: [{ id: 'm1', sequence: 2, sha: 'ee' }] })
    const two = aPage({
      masks: [
        { id: 'm1', sequence: 1, sha: 'ff' },
        { id: 'm2', sequence: 1, sha: 'dd' },
      ],
    })

    const cleaned = [none, one, rerun, two].map((page) => pageVersion(page, 'cleaned'))
    expect(new Set(cleaned).size).toBe(4)

    // The source page is the same file throughout: a clean run must not
    // invalidate the tile of the page it started from, or the wipe re-fetches
    // the original on every region.
    const sources = [none, one, rerun, two].map((page) => pageVersion(page, 'source'))
    expect(new Set(sources).size).toBe(1)
  })

  /**
   * Re-running inpainting produces new pixels while the mask geometry (and sequence/id)
   * can remain identical. The token must distinguish different runs via created timestamp or engine.
   */
  it('changes when inpainting is rerun with identical mask geometry but different timestamp or engine', () => {
    const first = aPage({
      masks: [{ id: 'r0-m1', sequence: 1, sha: 'ff', created: '2026-01-01T00:00:00Z', engine: 'lama' }],
    })
    const rerun = aPage({
      masks: [{ id: 'r0-m1', sequence: 1, sha: 'ff', created: '2026-01-01T00:01:00Z', engine: 'lama' }],
    })
    const diffEngine = aPage({
      masks: [{ id: 'r0-m1', sequence: 1, sha: 'ff', created: '2026-01-01T00:00:00Z', engine: 'fill' }],
    })
    expect(pageVersion(first, 'cleaned')).not.toBe(pageVersion(rerun, 'cleaned'))
    expect(pageVersion(first, 'cleaned')).not.toBe(pageVersion(diffEngine, 'cleaned'))
  })

  /**
   * The backend is free to return a page's regions in any order; that is not a
   * change to the page.
   */
  it('does not move when the regions are merely reordered', () => {
    const forwards = aPage({
      masks: [
        { id: 'm1', sequence: 1, sha: 'ff' },
        { id: 'm2', sequence: 1, sha: 'dd' },
      ],
    })
    const backwards = aPage({
      masks: [
        { id: 'm2', sequence: 1, sha: 'dd' },
        { id: 'm1', sequence: 1, sha: 'ff' },
      ],
    })
    expect(pageVersion(forwards, 'cleaned')).toBe(pageVersion(backwards, 'cleaned'))
  })

  /** A region with no mask is a region nothing was applied to. */
  it('ignores regions that were never cleaned', () => {
    const page = aPage({ masks: [null, { id: 'm1', sequence: 1, sha: 'ff' }, null] })
    const only = aPage({ masks: [{ id: 'm1', sequence: 1, sha: 'ff' }] })
    expect(pageVersion(page, 'cleaned')).toBe(pageVersion(only, 'cleaned'))
  })

  /**
   * `Cache-Control: immutable` pins a tile for a year, so the token has to
   * cover everything that decides what a tile *is* - including the provisional
   * geometry constants. Asserted by construction rather than by
   * mutating a module constant: the token is built from them, so a change to
   * either moves it.
   */
  it('covers the tile geometry as well as the content', () => {
    const page = aPage()
    const token = pageVersion(page, 'source')
    expect(token).toBe(fold(`${COLOR_PIPELINE_VERSION}:${PROXY_SHORT_EDGE}x${PROXY_TILE_LONG_EDGE}|${page.sourceSha}|0`))
    expect(token).not.toBe(fold(`|${page.sourceSha}`))
  })

  /**
   * The defect this token was rebuilt for: opacity and geometry were not in
   * it, and the counter that stood in for them was lost when a reload
   * replaced the page - so the old URL, and its old picture, came back.
   */
  describe('from the native appearance digests', () => {
    /** @param {{page?: string, layer?: string, locked?: boolean, opacity?: number}} spec */
    const native = ({ page = 'p0000000000000a', layer = 'l000000000000a', locked = false, opacity = 100 } = {}) => {
      const built = /** @type {any} */ (aPage({ masks: [{ id: 'm1', sequence: 1, sha: 'ff' }] }))
      built.appearance = page
      built.regions[0].mask.appearance = layer
      built.regions[0].mask.layer = { opacity, offsetX: 0, offsetY: 0, rotation: 0, locked }
      return built
    }

    it('moves when the page or a layer digest moves', () => {
      const base = pageVersion(native(), 'cleaned')
      expect(pageVersion(native({ page: 'p000000000000b' }), 'cleaned')).not.toBe(base)
      expect(pageVersion(native({ layer: 'l000000000000b' }), 'cleaned')).not.toBe(base)
    })

    it('is the same string after a reload that answers the same saved state', () => {
      const before = native()
      const edited = pageVersion(before, 'cleaned')
      // What `loadPages` hands back: a fresh object, no interface-side fields.
      const reloaded = structuredClone(native())
      expect(pageVersion(reloaded, 'cleaned')).toBe(edited)
    })

    it('reads the digest and not the style, so a lock alone leaves the URL alone', () => {
      expect(pageVersion(native({ locked: true }), 'cleaned')).toBe(pageVersion(native(), 'cleaned'))
    })

    it('leaves detections out: they draw nothing', () => {
      const page = native()
      const withDetection = structuredClone(page)
      withDetection.regions.push({ id: 'd1', outcome: 'detected', mask: { id: 'd1-m1', appearance: null } })
      expect(pageVersion(withDetection, 'cleaned')).toBe(pageVersion(page, 'cleaned'))
    })
  })

  it('folds a digest-less mask from its style, so the mock still moves on opacity and placement', () => {
    const page = /** @type {any} */ (aPage({ masks: [{ id: 'm1', sequence: 1, sha: 'ff' }] }))
    const versions = new Set()
    for (const layer of [
      undefined,
      { opacity: 50 },
      { opacity: 50, offsetX: 4 },
      { opacity: 50, offsetX: 4, offsetY: 2 },
      { opacity: 50, offsetX: 4, offsetY: 2, rotation: 15 },
    ]) {
      page.regions[0].mask.layer = layer
      versions.add(pageVersion(page, 'cleaned'))
    }
    expect(versions.size).toBe(5)
  })

  it('is sixteen hex characters, so it is a URL-safe cache key', () => {
    expect(fold('')).toMatch(/^[0-9a-f]{16}$/)
    expect(fold('a')).toMatch(/^[0-9a-f]{16}$/)
    expect(fold('a')).not.toBe(fold('b'))
    // Non-ASCII must not collapse: a chapter name reaches this string.
    expect(fold('第1話')).not.toBe(fold('第2話'))
  })
})
