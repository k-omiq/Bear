//! Preview proxies: the only picture the interface is allowed to hold.
//!
//! Rule 7 - *the UI holds proxies, never the strip*:
//!
//! > Previews cap the **short** edge at 1024 and tile the long axis - capping
//! > the long edge would render an 800×20000 page as a 40×1024 sliver.
//!
//! Both halves matter, and the second is the one that gets dropped. Capping the
//! short edge alone still leaves an 800×20000 webtoon segment whole: 16 MB over
//! the protocol and about 64 MB of decoded bitmap sitting in the webview for one
//! page, which the budget does not have and which a column of them
//! multiplies. So the long axis is cut into
//! tiles, each one an independent response the browser fetches, decodes and
//! evicts on its own.
//!
//! ## A cap is a cap, never an enlargement
//!
//! `detect::letterbox` scales a small page **up**, because the detector's input
//! is a fixed square and upstream trained it that way. This does the
//! opposite and the difference is deliberate: a preview of a 400×600 page
//! is 400×600. Enlarging
//! it would spend bytes inventing pixels the scan does not have.
//!
//! ## What a proxy is not
//!
//! It is not the page. A proxy is 8-bit, in a mode the browser can draw, and
//! that is a **display** conversion of exactly the kind
//! [`super::convert`] exists for - the same standing this module's sibling
//! `tile::encode_for_display` has when it flattens CMYK. A 16-bit page's proxy
//! is 8-bit and its 16-bit samples are untouched on disk; an indexed page's
//! proxy is RGB and the palette is untouched on disk; the fidelity contract
//! is stated over the exported file, never over what a webview drew.
//! Nothing here is ever written to an output file.
//!
//! Alpha is carried, and averaged **premultiplied**, because a plain average of
//! colour across a transparent edge drags the fringe toward whatever happens to
//! be stored under `a = 0`.

use super::{BitDepth, ColorMode, Raster};
use crate::mask::Mask;

/// The short edge's ceiling, from rule 7. The rule's number,
/// not a measurement: it is the resolution the interface draws at, and the
/// point of the rule is which edge it applies to.
pub const PROXY_SHORT_EDGE: u32 = 1024;

/// How much of the long axis one tile covers, in **proxy** pixels.
///
/// **Provisional.** The number that decides this
/// is how many bytes WebView2's UI thread can absorb per response: it raises
/// `WebResourceRequested` on that thread and pauses page load while the handler
/// runs, and the maintainer benchmark cited is ~5 ms on macOS against
/// **~200 ms on Windows for 10 MB**. That measurement
/// has never been taken on a Windows machine here - this is open, in this
/// phase, for exactly that, and a one-machine number written down as a
/// general one is exactly the mistake to avoid. So this
/// is not tuned; it is *bounded*: at the 1024 short edge a tile is at most
/// 1024×2048, which is the next power of two up from the cap and keeps one
/// response to a few megabytes of RGB8 before compression. Moving it is a
/// one-constant change on this side and on `src/lib/api/tile.js`'s, and the
/// URL's version token folds both constants in so a change cannot leave a
/// differently-cut tile in an immutable cache.
pub const PROXY_TILE_LONG_EDGE: u32 = 2048;

/// One page's proxy: its size, and how the long axis is cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyPlan {
    pub page_w: u32,
    pub page_h: u32,
    pub proxy_w: u32,
    pub proxy_h: u32,
    /// Whether the long axis - the one that is tiled - is `y`.
    pub vertical: bool,
    /// How many tiles the long axis is cut into. Never zero.
    pub tiles: usize,
}

/// One tile's rectangle, in **proxy** pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl ProxyPlan {
    /// The plan for a page of these dimensions.
    ///
    /// Integer arithmetic throughout, so the frontend's own copy of this
    /// (`src/lib/api/tile.js#proxyPlan`) can agree exactly rather than nearly:
    /// the two are separate implementations of one rule, and a tile count that
    /// disagreed by one would be a `404` on the last tile of a page.
    pub fn for_page(page_w: u32, page_h: u32) -> ProxyPlan {
        let page_w = page_w.max(1);
        let page_h = page_h.max(1);
        let short = page_w.min(page_h);
        let long = page_w.max(page_h);

        let proxy_short = short.min(PROXY_SHORT_EDGE);
        let proxy_long = if proxy_short == short {
            long
        } else {
            let scaled = (long as u64 * proxy_short as u64 + short as u64 / 2) / short as u64;
            (scaled.max(1) as u32).max(proxy_short)
        };

        let vertical = page_h >= page_w;
        let (proxy_w, proxy_h) = if vertical {
            (proxy_short, proxy_long)
        } else {
            (proxy_long, proxy_short)
        };
        let tiles = (proxy_long.div_ceil(PROXY_TILE_LONG_EDGE) as usize).max(1);

        ProxyPlan {
            page_w,
            page_h,
            proxy_w,
            proxy_h,
            vertical,
            tiles,
        }
    }

    /// The plan for this raster.
    pub fn of(page: &Raster) -> ProxyPlan {
        ProxyPlan::for_page(page.width, page.height)
    }

    /// Whether the proxy is the page at 1:1 in one piece - the case where a
    /// tile is a copy and the caller can serve the source file instead.
    pub fn is_whole_page(&self) -> bool {
        self.tiles == 1 && self.proxy_w == self.page_w && self.proxy_h == self.page_h
    }

    /// Tile `index`, or `None` past the end. The last tile along the axis is
    /// short whenever the axis does not divide evenly.
    pub fn tile(&self, index: usize) -> Option<TileRect> {
        if index >= self.tiles {
            return None;
        }
        let start = index as u32 * PROXY_TILE_LONG_EDGE;
        Some(if self.vertical {
            TileRect {
                x: 0,
                y: start,
                w: self.proxy_w,
                h: PROXY_TILE_LONG_EDGE.min(self.proxy_h - start),
            }
        } else {
            TileRect {
                x: start,
                y: 0,
                w: PROXY_TILE_LONG_EDGE.min(self.proxy_w - start),
                h: self.proxy_h,
            }
        })
    }
}

/// One tile of `page`'s proxy, or `None` for a tile the plan does not have -
/// which includes every tile of a raster with no pixels in it.
///
/// Only the tile's own source rows are read, so a 20 000 px segment's proxy
/// costs one tile's worth of output buffer rather than a whole second copy of
/// the page. (The page itself is already decoded by the caller - the *page* is
/// the unit here, not the strip, which is the distinction rule 1 draws.)
///
/// A zero dimension is refused here rather than clamped, because
/// [`ProxyPlan::for_page`] has already clamped it: the plan of a 0×0 raster is
/// a 1×1 proxy, and sampling that plan's one pixel reads row 0 of a raster that
/// has no row 0. Nothing [`super::decode`] produces has a zero dimension -
/// neither PNG nor TIFF can express one - but this takes any `&Raster`, and a
/// caller that builds one is entitled to an answer rather than a panic.
pub fn proxy_tile(page: &Raster, index: usize) -> Option<Raster> {
    managed_proxy_tile(page, index).ok().flatten()
}

/// Fallible entry point used by the UI: unsupported profiles are actionable
/// errors, never silently replaced by uncalibrated colors.
pub fn managed_proxy_tile(
    page: &Raster,
    index: usize,
) -> Result<Option<Raster>, super::ImageError> {
    if page.width == 0 || page.height == 0 {
        return Ok(None);
    }
    let plan = ProxyPlan::of(page);
    let Some(rect) = plan.tile(index) else {
        return Ok(None);
    };
    display_window(page, rect, plan.proxy_w, plan.proxy_h).map(Some)
}

/// Whole-page and tiled previews share this same transform and sampling path.
pub fn display(page: &Raster) -> Result<Raster, super::ImageError> {
    display_window(
        page,
        TileRect {
            x: 0,
            y: 0,
            w: page.width,
            h: page.height,
        },
        page.width,
        page.height,
    )
}

fn display_window(
    page: &Raster,
    rect: TileRect,
    width: u32,
    height: u32,
) -> Result<Raster, super::ImageError> {
    use super::color::{alpha, ManagedColor};
    let transform = ManagedColor::for_raster(page)?;
    // RGB output permits an unambiguous sRGB declaration even for Gray ICCs.
    let has_alpha = page.mode.alpha_channel().is_some() || page.trns.is_some();
    Ok(reduce_managed(rect, width, height, page.width, page.height, has_alpha, |x, y| {
        (transform.pixel(page, x, y), alpha(page, x, y))
    }))
}

fn reduce_managed(rect: TileRect, width: u32, height: u32, page_width: u32, page_height: u32, has_alpha: bool, sample: impl Fn(u32, u32) -> ([f32; 3], f32)) -> Raster {
    use super::color::{byte, encoded, linear};
    let mode = if has_alpha {
        ColorMode::Rgba
    } else {
        ColorMode::Rgb
    };
    let mut out = Raster {
        width: rect.w,
        height: rect.h,
        mode,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: Some(1),
        color: Default::default(),
        data: vec![0; rect.w as usize * rect.h as usize * mode.samples()],
    };
    for ty in 0..rect.h {
        let (y0, y1) = span(rect.y + ty, height, page_height);
        for tx in 0..rect.w {
            let (x0, x1) = span(rect.x + tx, width, page_width);
            let mut sum = [0.0f64; 3];
            let mut coverage = 0.0f64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let (rgb, a) = sample(sx, sy);
                    let a = a as f64;
                    for c in 0..3 {
                        sum[c] += linear(rgb[c]) as f64 * a;
                    }
                    coverage += a;
                }
            }
            for (c, total) in sum.iter().enumerate() {
                let value = if coverage > 0.0 {
                    encoded((total / coverage) as f32)
                } else {
                    0.0
                };
                out.set_sample(tx, ty, c, byte(value) as u16);
            }
            if has_alpha {
                out.set_sample(
                    tx,
                    ty,
                    3,
                    byte((coverage / ((x1 - x0) * (y1 - y0)) as f64) as f32) as u16,
                );
            }
        }
    }
    out
}

/// A patch's managed sRGB layer on the same grid as the page's tiles.
/// Metadata must describe the native patch samples, including palette and tRNS.
pub fn managed_proxy_layer(pixels: &Raster, mask: &Mask, plan: &ProxyPlan) -> Result<Option<(TileRect, Raster)>, super::ImageError> {
    use super::color::{alpha, ManagedColor};
    let bounds = mask.bounds;
    let (x0, y0) = (bounds.x.max(0), bounds.y.max(0));
    let (x1, y1) = (bounds.right().min(i64::from(plan.page_w)), bounds.bottom().min(i64::from(plan.page_h)));
    if x0 >= x1 || y0 >= y1 || pixels.width != bounds.w || pixels.height != bounds.h { return Ok(None); }
    let first = |at: i64, proxy: u32, page: u32| (at as u64 * u64::from(proxy) / u64::from(page)) as u32;
    let past = |at: i64, proxy: u32, page: u32| ((at as u64 * u64::from(proxy)).div_ceil(u64::from(page)) as u32).min(proxy);
    let (px, py) = (first(x0, plan.proxy_w, plan.page_w), first(y0, plan.proxy_h, plan.page_h));
    let rect = TileRect { x: px, y: py, w: past(x1, plan.proxy_w, plan.page_w)-px, h: past(y1, plan.proxy_h, plan.page_h)-py };
    let transform = ManagedColor::for_raster(pixels)?;
    let layer = reduce_managed(rect, plan.proxy_w, plan.proxy_h, plan.page_w, plan.page_h, true, |x,y| {
        let coverage = mask.coverage(i64::from(x), i64::from(y));
        if coverage == 0 { return ([0.0;3], 0.0); }
        let (lx,ly) = ((i64::from(x)-bounds.x) as u32, (i64::from(y)-bounds.y) as u32);
        (transform.pixel(pixels,lx,ly), alpha(pixels,lx,ly) * f32::from(coverage)/255.0)
    });
    Ok(Some((rect,layer)))
}

/// The source pixels one proxy pixel covers along one axis. Never empty: a
/// proxy at 1:1 maps pixel `i` to exactly `[i, i+1)`.
fn span(at: u32, proxy: u32, page: u32) -> (u32, u32) {
    let start = (at as u64 * page as u64 / proxy as u64) as u32;
    let end = ((at as u64 + 1) * page as u64 / proxy as u64) as u32;
    (start.min(page - 1), end.max(start + 1).min(page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::fixtures;
    use crate::mask::Rect;

    /// A page of a stated size, mid-gray with a black block, so an average has
    /// something to be wrong about.
    fn a_page(width: u32, height: u32) -> Raster {
        let mut page = Raster {
            width,
            height,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![128; (width * height) as usize],
        };
        for y in 0..height.min(64) {
            for x in 0..width.min(64) {
                page.set_sample(x, y, 0, 0);
            }
        }
        page
    }

    /// Rule 7's own sentence: capping the long edge is what a naive reading
    /// does, and it renders this page as a 40px sliver.
    #[test]
    fn a_webtoon_segment_keeps_its_width_and_is_not_reduced_to_a_sliver() {
        let plan = ProxyPlan::for_page(800, 20_000);
        assert_eq!((plan.proxy_w, plan.proxy_h), (800, 20_000));
        assert_ne!(
            plan.proxy_w, 40,
            "the long edge was capped instead of the short one"
        );
    }

    #[test]
    fn a_wide_page_is_reduced_by_its_short_edge() {
        let plan = ProxyPlan::for_page(2400, 3600);
        assert_eq!(plan.proxy_w, PROXY_SHORT_EDGE);
        assert_eq!(plan.proxy_h, 1536);
        assert!(plan.vertical);
    }

    /// The same rule with the axes swapped. A page is not assumed portrait.
    #[test]
    fn a_landscape_page_is_tiled_along_x() {
        let plan = ProxyPlan::for_page(6000, 1200);
        assert!(!plan.vertical);
        assert_eq!(plan.proxy_h, PROXY_SHORT_EDGE);
        assert_eq!(plan.proxy_w, 5120);
        assert_eq!(plan.tiles, 3);
        assert_eq!(
            plan.tile(0).unwrap(),
            TileRect {
                x: 0,
                y: 0,
                w: 2048,
                h: 1024
            }
        );
        assert_eq!(
            plan.tile(2).unwrap(),
            TileRect {
                x: 4096,
                y: 0,
                w: 1024,
                h: 1024
            }
        );
    }

    /// Unlike the detector's letterbox, which scales up because the model's
    /// input is a fixed square.
    #[test]
    fn a_page_smaller_than_the_cap_is_left_alone() {
        let plan = ProxyPlan::for_page(400, 600);
        assert_eq!((plan.proxy_w, plan.proxy_h), (400, 600));
        assert_eq!(plan.tiles, 1);
        assert!(plan.is_whole_page());
    }

    #[test]
    fn the_long_axis_is_cut_into_whole_tiles_and_one_short_one() {
        let plan = ProxyPlan::for_page(800, 20_000);
        assert_eq!(plan.tiles, 10);

        let mut covered = 0;
        for index in 0..plan.tiles {
            let rect = plan.tile(index).unwrap();
            assert_eq!(rect.w, 800, "a tile did not span the short edge");
            assert_eq!(rect.y, covered);
            covered += rect.h;
        }
        assert_eq!(covered, plan.proxy_h, "the tiles did not cover the page");
        assert!(plan.tile(plan.tiles).is_none());
        assert!(!plan.is_whole_page());
    }

    #[test]
    fn a_tile_is_the_part_of_the_page_it_names() {
        let page = a_page(800, 5000);
        let plan = ProxyPlan::of(&page);
        assert_eq!(plan.tiles, 3);

        let first = proxy_tile(&page, 0).unwrap();
        assert_eq!((first.width, first.height), (800, 2048));
        // The black block is in the first tile's top-left corner and nowhere
        // else, which is what says the tile is not merely the right size.
        assert_eq!(first.sample(10, 10, 0), 0);
        assert_eq!(first.sample(10, 100, 0), 128);

        let last = proxy_tile(&page, 2).unwrap();
        assert_eq!(last.height, 5000 - 2 * 2048);
        assert_eq!(last.sample(10, 10, 0), 128);
        assert!(proxy_tile(&page, 3).is_none());
    }

    /// At 1:1 a proxy is a copy, sample for sample. This is the property that
    /// makes the small-page case free rather than merely cheap.
    #[test]
    fn a_proxy_at_one_to_one_changes_no_sample() {
        let page = a_page(600, 900);
        let tile = proxy_tile(&page, 0).unwrap();
        assert_eq!((tile.width, tile.height), (600, 900));
        for y in [0, 63, 64, 899] {
            for x in [0, 63, 64, 599] {
                assert_eq!(tile.sample(x, y, 0), page.sample(x, y, 0), "at {x},{y}");
            }
        }
    }

    /// A downscale averages the box it covers. A 2:1 reduction of a page that
    /// is half black and half mid-gray along a boundary must produce the mean
    /// on the boundary row and neither of the two extremes.
    #[test]
    fn a_downscale_averages_rather_than_drops_pixels() {
        let mut page = a_page(2048, 2048);
        for y in 0..2048 {
            for x in 0..2048 {
                page.set_sample(x, y, 0, if y % 2 == 0 { 0 } else { 200 });
            }
        }
        let plan = ProxyPlan::of(&page);
        assert_eq!((plan.proxy_w, plan.proxy_h), (1024, 1024));

        let tile = proxy_tile(&page, 0).unwrap();
        // Every proxy pixel covers one black row and one 200 row.
        for y in [0, 1, 500, 1023] {
            assert_eq!(
                tile.sample(7, y, 0),
                146,
                "row {y} is not a linear-light mean"
            );
        }
    }

    /// Every fixture, through the proxy path, in a mode a browser can draw and
    /// with nothing left of a palette or a sub-byte depth.
    #[test]
    fn every_fixture_becomes_something_the_browser_can_draw() {
        for fixture in fixtures::all() {
            let name = fixture.name;
            let tile = proxy_tile(&fixture.raster, 0).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(tile.depth, BitDepth::Eight, "{name}");
            assert_eq!(tile.palette, None, "{name}");
            assert!(
                matches!(
                    tile.mode,
                    ColorMode::Gray | ColorMode::GrayAlpha | ColorMode::Rgb | ColorMode::Rgba
                ),
                "{name} became {:?}",
                tile.mode
            );
            assert_eq!(
                (tile.width, tile.height),
                (fixture.raster.width, fixture.raster.height)
            );
        }
    }

    /// A gray page stays gray. Tripling it into RGB is three times the bytes
    /// over the one path whose throughput is the open question.
    #[test]
    fn gray_preview_has_explicit_srgb_rgb_channels() {
        for name in ["l8", "l16", "bitonal"] {
            let page = fixtures::by_name(name).raster;
            assert_eq!(proxy_tile(&page, 0).unwrap().mode, ColorMode::Rgb, "{name}");
        }
        assert_eq!(
            proxy_tile(&fixtures::by_name("la8").raster, 0)
                .unwrap()
                .mode,
            ColorMode::Rgba
        );
    }

    /// CMYK is the mode whose samples do not survive the conversion, so its
    /// profile must not follow them - the same rule `tile::encode_for_display`
    /// already keeps for a whole page.
    #[test]
    fn a_cmyk_profile_does_not_follow_rgb_samples() {
        let mut page = fixtures::by_name("cmyk8").raster;
        page.icc = Some(fixtures::cmyk_profile());
        let tile = proxy_tile(&page, 0).unwrap();
        assert_eq!(tile.mode, ColorMode::Rgb);
        assert_eq!(tile.icc, None);
    }

    /// And every other mode's profile does, because the samples are still the
    /// page's colours at a lower resolution.
    #[test]
    fn a_profile_survives_a_mode_that_keeps_its_samples() {
        let page = fixtures::by_name("rgb8-icc").raster;
        assert!(page.icc.is_some());
        assert_eq!(proxy_tile(&page, 0).unwrap().srgb_intent, Some(1));
        assert_eq!(proxy_tile(&page, 0).unwrap().icc, None);
    }

    /// An indexed page with `tRNS` has transparency the palette carries; RGB
    /// would drop it silently.
    #[test]
    fn an_indexed_page_with_transparency_keeps_it() {
        let mut page = fixtures::by_name("indexed-p").raster;
        page.trns = Some(vec![0, 255, 255, 255]);
        let tile = proxy_tile(&page, 0).unwrap();
        assert_eq!(tile.mode, ColorMode::Rgba);

        page.trns = None;
        assert_eq!(proxy_tile(&page, 0).unwrap().mode, ColorMode::Rgb);
    }

    /// A fully transparent pixel must not drag its neighbours' colour: the
    /// average is premultiplied, so what is under `a = 0` contributes nothing.
    #[test]
    fn a_transparent_edge_does_not_bleed_into_the_average() {
        let mut page = Raster {
            width: 2048,
            height: 2048,
            mode: ColorMode::Rgba,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![0; 2048 * 2048 * 4],
        };
        for y in 0..2048 {
            for x in 0..2048 {
                let opaque = x % 2 == 0;
                // The transparent half stores black; a plain average would
                // halve the red of every proxy pixel.
                page.set_sample(x, y, 0, if opaque { 240 } else { 0 });
                page.set_sample(x, y, 3, if opaque { 255 } else { 0 });
            }
        }

        let tile = proxy_tile(&page, 0).unwrap();
        assert_eq!(
            tile.sample(5, 5, 0),
            240,
            "the transparent half bled into the colour"
        );
        assert_eq!(tile.sample(5, 5, 3), 128, "coverage was not averaged");
    }

    /// Source-over of an 8-bit layer onto an 8-bit grey tile, as a browser
    /// draws it.
    fn over(below: u16, colour: u16, alpha: u16) -> u16 {
        ((u32::from(colour) * u32::from(alpha) + u32::from(below) * (255 - u32::from(alpha)) + 127) / 255) as u16
    }

    /// Opaque layer interiors agree with the managed flattened page. Fractional
    /// edge coverage exposes why the editor must draw authoritative cleaned
    /// tiles: browser source-over in encoded sRGB cannot reproduce linear-light
    /// downsampling, independently of the mask/profile metadata on each layer.
    #[test]
    fn managed_layer_interiors_match_but_browser_blended_edges_require_flattened_tiles() {
        let mut page = a_page(2400, 3000);
        for y in 0..3000 {
            for x in 0..2400 {
                page.set_sample(x, y, 0, ((x / 7 + y / 11) % 256) as u16);
            }
        }
        let bounds = Rect::new(301, 457, 533, 211);
        let mut pixels = a_page(bounds.w, bounds.h);
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                pixels.set_sample(x, y, 0, (255 - (x * 3 + y) % 200) as u16);
            }
        }
        let mut stamped = page.clone();
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                stamped.set_sample(bounds.x as u32 + x, bounds.y as u32 + y, 0, pixels.sample(x, y, 0));
            }
        }

        let plan = ProxyPlan::of(&page);
        let (rect, layer) =
            managed_proxy_layer(&pixels, &Mask::filled(bounds), &plan).unwrap().expect("the patch is on the page");
        assert_eq!(layer.mode, ColorMode::Rgba);
        let (source, cleaned) = (proxy_tile(&page, 0).unwrap(), proxy_tile(&stamped, 0).unwrap());
        let mut worst_edge = 0;
        for ty in 0..rect.h {
            for tx in 0..rect.w {
                let (x, y) = (rect.x + tx, rect.y + ty);
                let drawn = over(source.sample(x, y, 0), layer.sample(tx, ty, 0), layer.sample(tx, ty, 3));
                let wanted = cleaned.sample(x, y, 0);
                if layer.sample(tx, ty, 3) == 255 {
                    assert_eq!(drawn, wanted, "interior at {x},{y}");
                } else {
                    worst_edge = worst_edge.max(drawn.abs_diff(wanted));
                }
            }
        }
        assert!(worst_edge > 2, "this fixture must expose encoded-browser versus linear-reduction edge blending; page artwork must use authoritative flattened tiles");
        // One proxy pixel past the rectangle, the page is the source's.
        assert_eq!(source.sample(rect.x - 1, rect.y + 5, 0), cleaned.sample(rect.x - 1, rect.y + 5, 0));
    }

    #[test]
    fn managed_layer_uses_native_gamma_and_color_key_before_downsampling() {
        let pixels = Raster {
            width: 2, height: 1, mode: ColorMode::Rgb, depth: BitDepth::Eight,
            icc: None, palette: None, trns: Some(vec![0,64,0,128,0,192]), srgb_intent: None,
            color: super::super::ColorDescription { gamma: Some(100_000), ..Default::default() },
            data: vec![64,128,192,128,128,128],
        };
        let plan = ProxyPlan { page_w:2, page_h:1, proxy_w:1, proxy_h:1, vertical:false, tiles:1 };
        let (_, layer) = managed_proxy_layer(&pixels, &Mask::filled(Rect::new(0,0,2,1)), &plan).unwrap().unwrap();
        // IEC sRGB encoding of linear 128/255 is 187.845; the keyed pixel
        // contributes no color. This expectation does not use our transform.
        assert_eq!(layer.data, vec![188,188,188,128]);
        assert_eq!(layer.srgb_intent, Some(1));
        assert_eq!(layer.icc, None);
    }

    #[test]
    fn a_patch_off_the_page_has_no_layer() {
        let plan = ProxyPlan::for_page(400, 600);
        let bounds = Rect::new(-50, 10, 40, 40);
        assert!(managed_proxy_layer(&a_page(40, 40), &Mask::filled(bounds), &plan).unwrap().is_none());
    }

    /// A raster with no pixels in it. The plan clamps a zero dimension to 1 so
    /// its own arithmetic is defined, which leaves one proxy pixel covering a
    /// source row that does not exist - read, that is an index past the end of
    /// an empty `data`. Nothing `decode` produces looks like this, and
    /// `proxy_tile` takes any `&Raster`.
    #[test]
    fn a_raster_with_a_zero_dimension_has_no_tiles_rather_than_panicking() {
        let empty = |width, height| Raster {
            width,
            height,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: Vec::new(),
        };
        assert!(proxy_tile(&empty(0, 400), 0).is_none());
        assert!(proxy_tile(&empty(400, 0), 0).is_none());
        assert!(proxy_tile(&empty(0, 0), 0).is_none());
    }
}
